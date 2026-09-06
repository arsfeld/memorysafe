use memorysafe_core::{
    Action, AdmitContext, Decision, ItemId, PolicyError, ScoredCandidate, WorkingSet,
};
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
    #[error("decision evicts {item}, which is pinned")]
    EvictsPinned { item: ItemId },
    #[error("decision carries no reason")]
    NoReason,
    #[error("working set contains {item}, which was never offered to the policy")]
    UnofferedItem { item: ItemId },
    #[error("working set reports {reported} tokens but its items sum to {actual}")]
    TokenAccountingWrong { reported: u32, actual: u32 },
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

/// The leak-prevention check. A policy may narrow the candidate set it was
/// given; it may never introduce an item the backend's hard filters excluded.
pub fn working_set(ws: &WorkingSet, offered: &[ScoredCandidate]) -> Result<(), Invalid> {
    for selected in &ws.items {
        if !offered.iter().any(|c| c.item.id == selected.item.id) {
            return Err(Invalid::UnofferedItem {
                item: selected.item.id.clone(),
            });
        }
    }

    let actual: u32 = ws
        .items
        .iter()
        .map(|s| {
            offered
                .iter()
                .find(|c| c.item.id == s.item.id)
                .map(|c| c.estimated_tokens)
                .unwrap_or(0)
        })
        .sum();
    if ws.tokens_used != actual {
        return Err(Invalid::TokenAccountingWrong {
            reported: ws.tokens_used,
            actual,
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
        PolicyId, Protection, Reason, ReasonCode, Scope, ScopeStats, Score, ScoredCandidate,
        SelectedItem, SensitivityLevel, Source, SourceKind, WorkingSet, features,
    };
    use time::OffsetDateTime;

    fn scope() -> Scope {
        Scope::new("t", "s", "n").unwrap()
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
        let d = Decision {
            subject: None,
            action: Action::Retain {
                protection: Protection::Normal,
            },
            evictions: vec![Eviction {
                item: ItemId::new(),
                reason: reason(),
            }],
            reasons: vec![reason()],
            policy: PolicyId::new("rogue", "0.1.0"),
        };
        assert!(matches!(
            decision(&d, &ctx(vec![])),
            Err(Invalid::EvictionOutsideScope { .. })
        ));
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
        assert!(matches!(
            decision(&d, &ctx(vec![c])),
            Err(Invalid::EvictsPinned { .. })
        ));
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
        assert!(matches!(decision(&d, &ctx(vec![])), Err(Invalid::NoReason)));
    }

    #[test]
    fn a_working_set_containing_an_unoffered_item_is_refused() {
        // The leak-prevention check: a policy may only narrow, never widen.
        let offered = candidate(item("offered"));
        let smuggled = item("never offered to the policy");
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
        assert!(matches!(
            working_set(&ws, std::slice::from_ref(&offered)),
            Err(Invalid::UnofferedItem { .. })
        ));
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
        assert!(working_set(&ws, &[a, b]).is_ok());
    }

    #[test]
    fn a_panicking_policy_is_caught_rather_than_taking_down_the_process() {
        let result: Result<u32, PolicyFailure> =
            call_policy(|| panic!("the closed scorer exploded"));
        assert!(matches!(result, Err(PolicyFailure::Panicked(_))));
    }

    #[test]
    fn a_policy_returning_an_error_is_reported_as_such() {
        let result: Result<u32, PolicyFailure> =
            call_policy(|| Err(memorysafe_core::PolicyError::MissingEmbedding));
        assert!(matches!(result, Err(PolicyFailure::Errored(_))));
    }
}
