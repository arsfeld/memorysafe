use memorysafe_core::{
    Action, AdmitContext, Decision, ItemId, PolicyError, RecallBudget, ScoredCandidate, WorkingSet,
};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

/// What the engine does when a policy misbehaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailureStance {
    /// Fall back to `BaselinePolicy` for that call.
    #[default]
    FailSafe,
    /// Refuse the operation.
    FailClosed,
}

#[derive(Debug, Error, PartialEq)]
pub enum Invalid {
    #[error("decision evicts {item}, which was not offered as an eviction candidate")]
    EvictionOutsideScope { item: ItemId },
    #[error("decision evicts {item}, which is pinned or under an unexpired protection window")]
    EvictsPinned { item: ItemId },
    #[error("decision carries no reason")]
    NoReason,
    #[error("working set contains {item}, which was never offered to the policy")]
    UnofferedItem { item: ItemId },
    #[error("working set reports {reported} tokens but its items sum to {actual}")]
    TokenAccountingWrong { reported: u32, actual: u32 },
    #[error("decision merges into {item}, which does not exist in the request's scope")]
    MergeTargetMissing { item: ItemId },
    #[error("working set returns {item} more than once")]
    DuplicateItem { item: ItemId },
    #[error("working set returns {returned} items against a budget of {allowed}")]
    OverItemBudget { returned: usize, allowed: usize },
    #[error("working set reports {returned} tokens against a budget of {allowed}")]
    OverTokenBudget { returned: u32, allowed: u32 },
}

#[derive(Debug, Error)]
pub enum PolicyFailure {
    #[error("policy panicked: {0}")]
    Panicked(String),
    #[error("policy returned an error: {0}")]
    Errored(#[from] PolicyError),
}

/// Nothing a policy returns is applied until it passes this.
pub fn decision(d: &Decision, ctx: &AdmitContext) -> Result<(), Invalid> {
    if d.reasons.is_empty() && d.evictions.is_empty() {
        return Err(Invalid::NoReason);
    }

    for e in &d.evictions {
        let Some(candidate) = ctx.eviction_candidates.iter().find(|c| c.item.id == e.item) else {
            return Err(Invalid::EvictionOutsideScope {
                item: e.item.clone(),
            });
        };
        // Pinning is absolute; the engine enforces it even if a policy forgets.
        if !candidate.item.protection.is_evictable(ctx.now) {
            return Err(Invalid::EvictsPinned {
                item: e.item.clone(),
            });
        }
    }

    if let Action::Retain { .. } | Action::Merge { .. } = d.action
        && d.reasons.is_empty()
    {
        return Err(Invalid::NoReason);
    }

    Ok(())
}

/// `Action::Merge { into, .. }` names a target `AdmitContext` has nothing to
/// check it against — no set of existing items is carried there, structurally
/// (see Task 33's provenance note: Task 32's brief claimed `validate::decision`
/// would check this and it cannot). The caller looks the id up in the
/// request's scope and hands the result here; `None` means either the id does
/// not exist at all, or it exists outside the scope this write is addressed
/// to — `Backend::get` is itself scope-filtered, so a single lookup answers
/// both questions at once. A policy naming a nonexistent or out-of-scope
/// merge target is a policy bug, not a silently dropped write.
pub fn merge_target(
    action: &Action,
    target: Option<&memorysafe_core::MemoryItem>,
) -> Result<(), Invalid> {
    if let Action::Merge { into, .. } = action
        && target.is_none()
    {
        return Err(Invalid::MergeTargetMissing { item: into.clone() });
    }
    Ok(())
}

/// The leak-prevention check. A policy may narrow the candidate set it was
/// given; it may never introduce an item — nor a substituted item wearing an
/// offered id — that the backend's hard filters excluded.
///
/// Compares the WHOLE `MemoryItem`, not just its id. An id-only check would
/// let a policy return an offered id with an arbitrary body, scope,
/// sensitivity, tags or attrs attached — a third-party policy that caches
/// items across calls and mixes up which body belongs to which id produces
/// exactly this shape, and an id match alone cannot tell it from a genuine
/// candidate. `BaselinePolicy::compose` clones the offered candidate's item
/// unmodified into `SelectedItem`, so strict equality never rejects
/// legitimate policy output.
///
/// `ws.omitted` is checked too: `OmittedItem` carries only an id (never a
/// body, so there is nothing to substitute), but that id must still have
/// been offered — `memorysafe_core::recall`'s own doc comment states
/// `omitted` is serialised into the MCP and HTTP responses, so an
/// out-of-scope id smuggled in there reaches a caller exactly as an
/// unvalidated `ws.items` entry would.
///
/// **The caller's `RecallBudget` is enforced here and nowhere else.**
/// `read::recall` clamps only its own *fetch* (`max_items * OVERFETCH`,
/// bounded by `MAX_CANDIDATES`), which is a load control on the backend, not
/// a bound on what the policy may return: a policy handed 500 candidates was
/// free to return all 500 to a caller who asked for 5, and the engine shipped
/// it. `RecallBudget::fits` is the same predicate a policy is expected to
/// compose against, so this can only reject a policy that ignored the budget
/// it was given. A `None` on either half of the budget means "no bound", and
/// is not a defect to report.
///
/// **A repeated item is refused.** Nothing above catches it on its own:
/// `offered.iter().any(...)` is satisfied by each copy independently, and the
/// token sum double-counts the repeat *consistently*, so a policy returning
/// one item twice with the doubled `tokens_used` passed every check. The
/// caller then gets the same memory twice inside a budget that was supposed
/// to be spent on distinct ones. `ws.omitted` is not deduplicated here —
/// `omitted` is documented as a truncated *sample*, so a repeat there is
/// cosmetic rather than a budget or leak defect.
///
/// The offered set is indexed by id once rather than rescanned per selected
/// item; the identity comparison is unchanged, since the whole
/// `MemoryItem` is still compared against the candidate the id resolves to.
pub fn working_set(
    ws: &WorkingSet,
    offered: &[ScoredCandidate],
    budget: &RecallBudget,
) -> Result<(), Invalid> {
    let by_id: HashMap<&ItemId, &ScoredCandidate> =
        offered.iter().map(|c| (&c.item.id, c)).collect();

    let mut seen: HashSet<&ItemId> = HashSet::with_capacity(ws.items.len());
    let mut actual: u32 = 0;
    for selected in &ws.items {
        let id = &selected.item.id;
        match by_id.get(id) {
            Some(candidate) if candidate.item == selected.item => {
                actual = actual.saturating_add(candidate.estimated_tokens);
            }
            // Both "never offered" and "offered id, substituted body" are the
            // same refusal, as they were before the index: the id is not a
            // licence to attach an arbitrary item to it.
            _ => {
                return Err(Invalid::UnofferedItem { item: id.clone() });
            }
        }
        if !seen.insert(id) {
            return Err(Invalid::DuplicateItem { item: id.clone() });
        }
    }

    for omitted in &ws.omitted {
        if !by_id.contains_key(&omitted.id) {
            return Err(Invalid::UnofferedItem {
                item: omitted.id.clone(),
            });
        }
    }

    if ws.tokens_used != actual {
        return Err(Invalid::TokenAccountingWrong {
            reported: ws.tokens_used,
            actual,
        });
    }

    // After the accounting check, so a policy that overran the budget *and*
    // misreported its total is reported as the accounting failure — the more
    // fundamental of the two, and the one that makes the budget figure below
    // untrustworthy in the first place.
    if let Some(max) = budget.max_items
        && ws.items.len() > max
    {
        return Err(Invalid::OverItemBudget {
            returned: ws.items.len(),
            allowed: max,
        });
    }
    if let Some(max) = budget.max_tokens
        && ws.tokens_used > max
    {
        return Err(Invalid::OverTokenBudget {
            returned: ws.tokens_used,
            allowed: max,
        });
    }

    Ok(())
}

/// Runs a policy call under `catch_unwind`. A third-party or closed-source
/// policy must not be able to take down the process.
pub fn call_policy<T, F>(f: F) -> Result<T, PolicyFailure>
where
    F: FnOnce() -> Result<T, PolicyError> + std::panic::UnwindSafe,
{
    match std::panic::catch_unwind(f) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(PolicyFailure::Errored(e)),
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            Err(PolicyFailure::Panicked(message))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::{
        Action, Budget, CapacityState, Eviction, ItemId, MaintenanceCandidate, MemoryItem,
        MergeStrategy, OmittedItem, PolicyId, Protection, Reason, ReasonCode, Scope, ScopeStats,
        Score, ScoredCandidate, SelectedItem, SensitivityLevel, Source, SourceKind, WorkingSet,
        features,
    };
    use time::OffsetDateTime;

    fn scope() -> Scope {
        Scope::new("t", "s", "n").unwrap()
    }

    /// No budget at all, so the tests that predate budget enforcement keep
    /// testing exactly what they tested. `RecallBudget::default()` would
    /// **not** do: it is `Some(20)`/`Some(2000)`, so an over-budget bug in
    /// one of those tests could be masked or manufactured by a limit the test
    /// never meant to set.
    fn unbounded() -> RecallBudget {
        RecallBudget {
            max_tokens: None,
            max_items: None,
        }
    }

    fn item(body: &str) -> MemoryItem {
        MemoryItem {
            id: ItemId::new(),
            scope: scope(),
            body: body.into(),
            kind: "fact".into(),
            source: Source {
                kind: SourceKind::Agent,
                id: None,
            },
            occurred_at: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            tags: vec![],
            attrs: Default::default(),
            sensitivity: SensitivityLevel::Internal,
            ttl: None,
            protection: Protection::Normal,
            pending_embedding: false,
        }
    }

    fn candidate(item: MemoryItem) -> ScoredCandidate {
        ScoredCandidate {
            item,
            relevance: 0.5,
            vector_score: None,
            keyword_score: None,
            value: Score::clamped(0.5),
            fragility: Score::clamped(0.5),
            estimated_tokens: 5,
            last_accessed_at: None,
            access_count: 0,
        }
    }

    // `AdmitContext::eviction_candidates` is `Vec<MaintenanceCandidate>`, not
    // `Vec<ScoredCandidate>` — that field carries no relevance because there
    // is no recall query behind an eviction offer. `candidate()` above builds
    // the other type, for the `working_set` tests below that genuinely need
    // `&[ScoredCandidate]`; this is its counterpart for eviction candidates.
    fn maintenance_candidate(item: MemoryItem) -> MaintenanceCandidate {
        MaintenanceCandidate {
            item,
            value: Score::clamped(0.5),
            fragility: Score::clamped(0.5),
            last_accessed_at: None,
            access_count: 0,
        }
    }

    fn ctx(evictable: Vec<MaintenanceCandidate>) -> AdmitContext {
        AdmitContext {
            scope: scope(),
            capacity: CapacityState {
                budget: Budget::UNBOUNDED,
                used_items: 0,
                used_bytes: 0,
            },
            eviction_candidates: evictable,
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn reason() -> Reason {
        Reason::new(ReasonCode::NovelContent, "ok", features! {})
    }

    #[test]
    fn a_well_formed_decision_is_accepted() {
        let c = maintenance_candidate(item("evictable"));
        let d = Decision {
            subject: None,
            action: Action::Retain {
                protection: Protection::Normal,
            },
            evictions: vec![Eviction {
                item: c.item.id.clone(),
                reason: reason(),
            }],
            reasons: vec![reason()],
            policy: PolicyId::new("baseline", "0.1.0"),
        };
        assert!(decision(&d, &ctx(vec![c])).is_ok());
    }

    #[test]
    fn a_decision_evicting_an_item_outside_the_offered_set_is_refused() {
        let outside_id = ItemId::new();
        let d = Decision {
            subject: None,
            action: Action::Retain {
                protection: Protection::Normal,
            },
            evictions: vec![Eviction {
                item: outside_id.clone(),
                reason: reason(),
            }],
            reasons: vec![reason()],
            policy: PolicyId::new("rogue", "0.1.0"),
        };
        assert_eq!(
            decision(&d, &ctx(vec![])),
            Err(Invalid::EvictionOutsideScope { item: outside_id }),
            "the payload must name the item actually evicted, not just the variant"
        );
    }

    #[test]
    fn a_decision_evicting_a_pinned_item_is_refused() {
        let mut pinned = item("pinned");
        pinned.protection = Protection::Pinned;
        let c = maintenance_candidate(pinned);
        let d = Decision {
            subject: None,
            action: Action::Retain {
                protection: Protection::Normal,
            },
            evictions: vec![Eviction {
                item: c.item.id.clone(),
                reason: reason(),
            }],
            reasons: vec![reason()],
            policy: PolicyId::new("rogue", "0.1.0"),
        };
        assert_eq!(
            decision(&d, &ctx(vec![c.clone()])),
            Err(Invalid::EvictsPinned { item: c.item.id }),
            "the payload must name the item actually pinned, not just the variant"
        );
    }

    #[test]
    fn a_decision_evicting_an_item_under_an_unexpired_protection_window_is_refused() {
        // `Protection::is_evictable` refuses both `Pinned` AND an unexpired
        // `Protected { until }` window, and both share the `EvictsPinned`
        // variant. This pins the second case, which no other test reaches,
        // and — by using a `now` that differs from every other test's fixed
        // `OffsetDateTime::UNIX_EPOCH` — makes `ctx.now` actually load-bearing:
        // a mutant that ignored `ctx.now` and checked evictability against a
        // hardcoded instant would need to coincidentally agree with this
        // window boundary to survive.
        let now = OffsetDateTime::from_unix_timestamp(1_000).unwrap();
        let until = OffsetDateTime::from_unix_timestamp(2_000).unwrap();
        let mut protected = item("under an active protection window");
        protected.protection = Protection::Protected { until };
        let c = maintenance_candidate(protected);
        let d = Decision {
            subject: None,
            action: Action::Retain {
                protection: Protection::Normal,
            },
            evictions: vec![Eviction {
                item: c.item.id.clone(),
                reason: reason(),
            }],
            reasons: vec![reason()],
            policy: PolicyId::new("rogue", "0.1.0"),
        };
        let mut context = ctx(vec![c.clone()]);
        context.now = now;
        assert_eq!(
            decision(&d, &context),
            Err(Invalid::EvictsPinned { item: c.item.id })
        );
    }

    #[test]
    fn a_decision_evicting_an_item_whose_protection_window_has_expired_is_accepted() {
        // The positive counterpart: once `now` passes `until`, the same
        // `Protected` item is evictable. Together with the test above, this
        // pair only agrees with the implementation if `ctx.now` — not some
        // other, unrelated instant — is what `is_evictable` is actually
        // checked against.
        let now = OffsetDateTime::from_unix_timestamp(2_000).unwrap();
        let until = OffsetDateTime::from_unix_timestamp(1_000).unwrap();
        let mut protected = item("protection window has lapsed");
        protected.protection = Protection::Protected { until };
        let c = maintenance_candidate(protected);
        let d = Decision {
            subject: None,
            action: Action::Retain {
                protection: Protection::Normal,
            },
            evictions: vec![Eviction {
                item: c.item.id.clone(),
                reason: reason(),
            }],
            reasons: vec![reason()],
            policy: PolicyId::new("baseline", "0.1.0"),
        };
        let mut context = ctx(vec![c.clone()]);
        context.now = now;
        assert!(decision(&d, &context).is_ok());
    }

    #[test]
    fn a_decision_with_no_reason_is_refused() {
        let d = Decision {
            subject: None,
            action: Action::Retain {
                protection: Protection::Normal,
            },
            evictions: vec![],
            reasons: vec![],
            policy: PolicyId::new("rogue", "0.1.0"),
        };
        assert_eq!(decision(&d, &ctx(vec![])), Err(Invalid::NoReason));
    }

    #[test]
    fn a_reject_decision_justified_only_by_eviction_reasons_needs_no_top_level_reason() {
        // The asymmetric case the top guard's `&&` exists for: `reasons` is
        // empty but `evictions` is not, and each eviction already carries its
        // own reason. A `||` in place of that `&&` would refuse this
        // legitimate decision outright.
        let c = maintenance_candidate(item("evictable"));
        let d = Decision {
            subject: None,
            action: Action::Reject,
            evictions: vec![Eviction {
                item: c.item.id.clone(),
                reason: reason(),
            }],
            reasons: vec![],
            policy: PolicyId::new("baseline", "0.1.0"),
        };
        assert!(decision(&d, &ctx(vec![c])).is_ok());
    }

    #[test]
    fn a_retain_decision_with_evictions_but_no_top_level_reason_is_refused() {
        // Same asymmetric shape as the test above (evictions non-empty,
        // top-level reasons empty), but `Action::Retain` — which the eviction
        // list alone cannot justify, unlike `Reject`. This is what the
        // *second* guard (`Action::Retain{..} | Action::Merge{..}` plus
        // `d.reasons.is_empty()`) exists to catch; deleting that guard
        // wholesale would flip this decision from refused to accepted.
        let c = maintenance_candidate(item("evictable"));
        let d = Decision {
            subject: None,
            action: Action::Retain {
                protection: Protection::Normal,
            },
            evictions: vec![Eviction {
                item: c.item.id.clone(),
                reason: reason(),
            }],
            reasons: vec![],
            policy: PolicyId::new("rogue", "0.1.0"),
        };
        assert_eq!(decision(&d, &ctx(vec![c])), Err(Invalid::NoReason));
    }

    #[test]
    fn a_merge_decision_with_evictions_but_no_top_level_reason_is_refused() {
        // The `Action::Merge{..}` arm of the same second guard, which the
        // Retain-only test above cannot exercise — a mutant narrowing the
        // pattern to `Action::Retain{..}` alone would still pass every other
        // test here.
        let c = maintenance_candidate(item("evictable"));
        let d = Decision {
            subject: None,
            action: Action::Merge {
                into: ItemId::new(),
                strategy: MergeStrategy::AppendAndUnion,
            },
            evictions: vec![Eviction {
                item: c.item.id.clone(),
                reason: reason(),
            }],
            reasons: vec![],
            policy: PolicyId::new("rogue", "0.1.0"),
        };
        assert_eq!(decision(&d, &ctx(vec![c])), Err(Invalid::NoReason));
    }

    #[test]
    fn a_working_set_containing_an_unoffered_item_is_refused() {
        // The leak-prevention check: a policy may only narrow, never widen.
        let offered = candidate(item("offered"));
        let smuggled = item("never offered to the policy");
        let smuggled_id = smuggled.id.clone();
        let ws = WorkingSet {
            items: vec![SelectedItem {
                item: smuggled,
                relevance: 0.9,
                reason: reason(),
            }],
            tokens_used: 5,
            omitted: vec![],
            omitted_total: 0,
            audit_id: None,
        };
        assert_eq!(
            working_set(&ws, std::slice::from_ref(&offered), &unbounded()),
            Err(Invalid::UnofferedItem { item: smuggled_id }),
            "the payload must name the smuggled item, not just the variant"
        );
    }

    #[test]
    fn a_working_set_substituting_a_different_body_under_an_offered_id_is_refused() {
        // The stronger property the brief actually asks for: "may contain
        // only candidates that were supplied to it". An id-only check would
        // accept this — the id genuinely was offered — but the body attached
        // to it here was not. A policy that caches items across calls and
        // mixes up which body belongs to which id produces exactly this
        // shape, and only whole-item equality catches it.
        let offered = candidate(item("the real body"));
        let mut substituted = offered.item.clone();
        substituted.body = "a different body entirely".into();
        let offered_id = offered.item.id.clone();
        let ws = WorkingSet {
            items: vec![SelectedItem {
                item: substituted,
                relevance: 0.9,
                reason: reason(),
            }],
            tokens_used: 5,
            omitted: vec![],
            omitted_total: 0,
            audit_id: None,
        };
        assert_eq!(
            working_set(&ws, std::slice::from_ref(&offered), &unbounded()),
            Err(Invalid::UnofferedItem { item: offered_id })
        );
    }

    #[test]
    fn a_working_sets_omitted_list_containing_an_unoffered_item_is_refused() {
        // `OmittedItem` carries only an id — nothing to substitute — but that
        // id must still have been offered. `omitted` is serialised straight
        // into the MCP/HTTP response, so an unchecked id here leaks exactly
        // as an unchecked `items` entry would.
        let offered = candidate(item("offered"));
        let smuggled_id = ItemId::new();
        let ws = WorkingSet {
            items: vec![],
            tokens_used: 0,
            omitted: vec![OmittedItem {
                id: smuggled_id.clone(),
                reason: reason(),
            }],
            omitted_total: 1,
            audit_id: None,
        };
        assert_eq!(
            working_set(&ws, std::slice::from_ref(&offered), &unbounded()),
            Err(Invalid::UnofferedItem { item: smuggled_id })
        );
    }

    #[test]
    fn a_working_set_that_is_a_subset_is_accepted() {
        let a = candidate(item("a"));
        let b = candidate(item("b"));
        let ws = WorkingSet {
            items: vec![SelectedItem {
                item: a.item.clone(),
                relevance: 0.9,
                reason: reason(),
            }],
            tokens_used: 5,
            omitted: vec![],
            omitted_total: 0,
            audit_id: None,
        };
        assert!(working_set(&ws, &[a, b], &unbounded()).is_ok());
    }

    #[test]
    fn a_working_set_with_a_wrong_token_total_is_refused() {
        // The accounting check is untested by the two tests above: the
        // unoffered-item test never reaches it (it returns earlier), and the
        // accepted test's `tokens_used` is deliberately exact. Force a real
        // mismatch and pin the whole `TokenAccountingWrong` payload, not just
        // the variant — a field-swap bug in that payload is otherwise
        // invisible.
        let a = candidate(item("a")); // estimated_tokens: 5
        let ws = WorkingSet {
            items: vec![SelectedItem {
                item: a.item.clone(),
                relevance: 0.9,
                reason: reason(),
            }],
            tokens_used: 999,
            omitted: vec![],
            omitted_total: 0,
            audit_id: None,
        };
        assert_eq!(
            working_set(&ws, std::slice::from_ref(&a), &unbounded()),
            Err(Invalid::TokenAccountingWrong {
                reported: 999,
                actual: 5,
            })
        );
    }

    /// **The same item twice passed every other check in this function, and
    /// the reason it did is the whole point of this test.** `offered` is
    /// scanned with `any`, which each copy satisfies independently; and the
    /// token sum double-counts the repeat *consistently*, so a policy that
    /// also doubles `tokens_used` — as this test does — keeps the accounting
    /// check happy too. The caller then receives the same memory twice,
    /// inside a budget meant to be spent on distinct ones.
    ///
    /// `tokens_used` is deliberately the *consistent* 10, not 5: at 5 this
    /// would be caught by `TokenAccountingWrong` and would prove nothing
    /// about duplicate detection.
    #[test]
    fn a_working_set_returning_the_same_item_twice_is_refused() {
        let a = candidate(item("a")); // estimated_tokens: 5
        let selected = || SelectedItem {
            item: a.item.clone(),
            relevance: 0.9,
            reason: reason(),
        };
        let ws = WorkingSet {
            items: vec![selected(), selected()],
            tokens_used: 10,
            omitted: vec![],
            omitted_total: 0,
            audit_id: None,
        };
        assert_eq!(
            working_set(&ws, std::slice::from_ref(&a), &unbounded()),
            Err(Invalid::DuplicateItem {
                item: a.item.id.clone()
            }),
            "the payload must name the repeated item, not just the variant"
        );
    }

    /// **`read::recall` clamps the fetch, not the answer.** A policy handed
    /// 500 candidates for a request that asked for 5 could return all 500 and
    /// the engine shipped them; nothing between the policy and the caller
    /// compared the working set against `RecallBudget`.
    ///
    /// Two items against `max_items: 1`, so the refusal cannot be an
    /// off-by-one reading of "at least one over"; and the whole payload is
    /// pinned, not just the variant, so a swapped `returned`/`allowed` pair
    /// is visible.
    #[test]
    fn a_working_set_over_the_callers_item_budget_is_refused() {
        let a = candidate(item("a"));
        let b = candidate(item("b"));
        let ws = WorkingSet {
            items: vec![
                SelectedItem {
                    item: a.item.clone(),
                    relevance: 0.9,
                    reason: reason(),
                },
                SelectedItem {
                    item: b.item.clone(),
                    relevance: 0.8,
                    reason: reason(),
                },
            ],
            tokens_used: 10,
            omitted: vec![],
            omitted_total: 0,
            audit_id: None,
        };
        let budget = RecallBudget {
            max_tokens: None,
            max_items: Some(1),
        };
        assert_eq!(
            working_set(&ws, &[a.clone(), b.clone()], &budget),
            Err(Invalid::OverItemBudget {
                returned: 2,
                allowed: 1
            })
        );
        // Exactly at the budget is fine — the check is `>`, not `>=`, and a
        // test that only proved the over case would pass against either.
        let at_budget = RecallBudget {
            max_tokens: None,
            max_items: Some(2),
        };
        assert!(working_set(&ws, &[a, b], &at_budget).is_ok());
    }

    /// The token half of the same rule. Separate from the item half because
    /// each is separately deletable: a budget check that enforced only
    /// `max_items` would pass the test above and let a caller who asked for
    /// 2000 tokens receive far more in five very large items.
    #[test]
    fn a_working_set_over_the_callers_token_budget_is_refused() {
        let a = candidate(item("a")); // estimated_tokens: 5
        let ws = WorkingSet {
            items: vec![SelectedItem {
                item: a.item.clone(),
                relevance: 0.9,
                reason: reason(),
            }],
            tokens_used: 5,
            omitted: vec![],
            omitted_total: 0,
            audit_id: None,
        };
        let budget = RecallBudget {
            max_tokens: Some(4),
            max_items: None,
        };
        assert_eq!(
            working_set(&ws, std::slice::from_ref(&a), &budget),
            Err(Invalid::OverTokenBudget {
                returned: 5,
                allowed: 4
            })
        );
        let at_budget = RecallBudget {
            max_tokens: Some(5),
            max_items: None,
        };
        assert!(working_set(&ws, std::slice::from_ref(&a), &at_budget).is_ok());
    }

    /// A `None` on either half means "no bound" — `RecallBudget`'s own
    /// `fits` reads it that way — so a caller who omits a limit must not have
    /// one invented for them. Without this, a plausible implementation that
    /// substituted `RecallBudget::default()`'s `Some(20)`/`Some(2000)` for an
    /// absent bound would go unnoticed until a large legitimate recall
    /// started failing.
    #[test]
    fn an_absent_budget_bound_is_no_bound() {
        let a = candidate(item("a"));
        let ws = WorkingSet {
            items: vec![SelectedItem {
                item: a.item.clone(),
                relevance: 0.9,
                reason: reason(),
            }],
            tokens_used: 5,
            omitted: vec![],
            omitted_total: 0,
            audit_id: None,
        };
        assert!(working_set(&ws, std::slice::from_ref(&a), &unbounded()).is_ok());
    }

    #[test]
    fn a_panicking_policy_is_caught_rather_than_taking_down_the_process() {
        let result: Result<u32, PolicyFailure> =
            call_policy(|| panic!("the closed scorer exploded"));
        match result {
            Err(PolicyFailure::Panicked(message)) => {
                // The message is the only diagnostic an operator gets when a
                // closed-source policy explodes; a `matches!(.., Panicked(_))`
                // alone cannot tell a preserved message from `"unknown panic"`.
                assert_eq!(message, "the closed scorer exploded");
            }
            other => panic!("expected PolicyFailure::Panicked, got {other:?}"),
        }
    }

    #[test]
    fn a_policy_returning_an_error_is_reported_as_such() {
        let result: Result<u32, PolicyFailure> =
            call_policy(|| Err(memorysafe_core::PolicyError::MissingEmbedding));
        assert!(matches!(result, Err(PolicyFailure::Errored(_))));
    }

    #[test]
    fn a_successful_policy_call_returns_its_value() {
        // Every real policy call takes this path, and nothing here exercised
        // it before: both existing `call_policy` tests close over a failing
        // closure (one panics, one returns `Err`). `Ok(Ok(v)) => Ok(v)` could
        // be replaced with an unconditional `Err` and both of those stayed
        // green.
        let result: Result<u32, PolicyFailure> = call_policy(|| Ok(7));
        assert_eq!(result.unwrap(), 7);
    }

    // `merge_target`: `AdmitContext` carries no set of existing items to check
    // `Action::Merge { into, .. }` against, so the caller looks the target up
    // and hands the result in. These tests pin that the check fires only for
    // `Merge`, that a missing target is refused with the *actual* target id
    // (not just the variant), and that a resolved target is accepted.

    #[test]
    fn a_merge_decision_whose_target_exists_is_accepted() {
        let target = item("the existing memory");
        let d = Decision {
            subject: None,
            action: Action::Merge {
                into: target.id.clone(),
                strategy: MergeStrategy::AppendAndUnion,
            },
            evictions: vec![],
            reasons: vec![reason()],
            policy: PolicyId::new("baseline", "0.1.0"),
        };
        assert!(merge_target(&d.action, Some(&target)).is_ok());
    }

    #[test]
    fn a_merge_decision_whose_target_does_not_exist_is_refused() {
        let missing_id = ItemId::new();
        let action = Action::Merge {
            into: missing_id.clone(),
            strategy: MergeStrategy::AppendAndUnion,
        };
        assert_eq!(
            merge_target(&action, None),
            Err(Invalid::MergeTargetMissing { item: missing_id }),
            "the payload must name the missing target, not just the variant"
        );
    }

    #[test]
    fn a_retain_decision_is_accepted_regardless_of_any_target() {
        // The check must fire only for `Action::Merge`. A mutant that dropped
        // the `if let Action::Merge` guard and required a target
        // unconditionally would refuse this legitimate `Retain` decision,
        // which never has a merge target to look up.
        let action = Action::Retain {
            protection: Protection::Normal,
        };
        assert!(merge_target(&action, None).is_ok());
    }

    #[test]
    fn a_reject_decision_is_accepted_regardless_of_any_target() {
        // Same guard, the other non-merge variant.
        assert!(merge_target(&Action::Reject, None).is_ok());
    }
}
