//! Maintenance decisions for a page of a namespace: expire items past their
//! TTL, release protection windows that have elapsed, recompute fragility
//! against the neighbourhood as it now stands, consolidate near-duplicate
//! survivors, and reclaim capacity when the namespace is over budget.
//!
//! Pure, like the rest of the seam. Every removal here is a `Decision` the
//! engine applies; nothing in this file touches a backend, chooses a
//! transaction boundary, or decides when the next page runs.
//!
//! **At most one decision names any item.** The engine applies decisions
//! independently, so two naming one row — a release and a re-grant, a merge
//! and an eviction — would leave the outcome to whichever it happened to
//! apply last. Each step below therefore skips anything an earlier step has
//! already spoken for, and this module's last test asserts the invariant end
//! to end over a fixture that produces all four shapes at once.
//!
//! **What one page can and cannot see.** `ctx.batch` is a slice of the
//! namespace, so an item's page-mates undercount its real neighbourhood. Two
//! consequences, both deliberate:
//!
//! * Fragility is read as a CHANGE across this run rather than as a level —
//!   a page cannot establish that an item has no neighbours, but it can
//!   establish that one it did have has just been deleted.
//! * Near-duplicates that land on different pages are not consolidated. The
//!   next run pages differently once this run's removals have landed; nothing
//!   here tries to remember across pages, because `maintain` is pure and the
//!   cursor belongs to the engine.
//!
//! `ctx.is_final_batch` is not read. Every step here acts only on evidence
//! its own page carries, so none of them has anything to wait for; a step
//! that summed across the whole namespace would need it.

use crate::config::BaselineConfig;
use crate::{eviction, fragility, similarity};
use memorysafe_core::{
    Action, Decision, Eviction, ItemId, MaintainContext, MaintenanceCandidate, MemoryItem,
    MergeStrategy, PolicyId, Protection, Reason, ReasonCode, ScopeStats, Score, ScoredCandidate,
    features,
};
use std::collections::BTreeSet;
use time::Duration;

/// Deterministic age tie-breaker under the `eviction::cost` ordering: when two
/// candidates are equally cheap to lose, the one created earlier is reclaimed
/// first. Creation time alone — this ranks nothing else, and is never
/// consulted unless the cost comparison has already come out equal.
///
/// It exists for determinism rather than for judgement. Without it, equal-cost
/// candidates keep whatever order the backend's `list` happened to return,
/// so the same namespace could reclaim different items on two identical runs
/// and the audit trail would have no account of why.
fn reclaim_rank(item: &MemoryItem) -> i64 {
    item.created_at.unix_timestamp()
}

/// The subjects of a set of decisions — the items already spoken for.
fn subjects(ds: &[Decision]) -> BTreeSet<ItemId> {
    ds.iter().filter_map(|d| d.subject.clone()).collect()
}

/// Items past their TTL.
///
/// No pinned-item exemption, deliberately: `MemoryItem::must_forget` dominates
/// protection, because a pin that could defeat a TTL would let any caller opt
/// out of a legal retention limit by pinning. Protection governs eviction for
/// CAPACITY, which is `capacity_reclaim`'s business below and is where the
/// "pinning is absolute" guarantee actually lives.
fn ttl_expiries(ctx: &MaintainContext, policy: &PolicyId) -> Vec<Decision> {
    ctx.batch
        .iter()
        .map(|c| &c.item)
        .filter(|item| item.must_forget(ctx.now))
        .map(|item| Decision {
            subject: Some(item.id.clone()),
            action: Action::Reject,
            evictions: vec![Eviction {
                item: item.id.clone(),
                reason: Reason::new(
                    ReasonCode::TtlExpired,
                    "the item's time-to-live elapsed",
                    features! {
                        "age_days" => (ctx.now - item.created_at).whole_days() as f64,
                    },
                ),
            }],
            reasons: vec![],
            policy: policy.clone(),
        })
        .collect()
}

/// Capacity reclaim, cheapest first, pinned untouchable.
///
/// Computed BEFORE decay and consolidation even though its decisions are
/// emitted after theirs: both of those read the surviving neighbourhood, and a
/// survivor set that still contained items this run is reclaiming would give
/// them a corpus that no longer exists by the time the engine is done.
fn capacity_reclaim(
    ctx: &MaintainContext,
    expired: &BTreeSet<ItemId>,
    policy: &PolicyId,
) -> Vec<Decision> {
    let Some(max_items) = ctx.capacity.budget.max_items else {
        return Vec::new();
    };
    // This run's own expiries have already made room; counting from
    // `used_items` alone would reclaim live items to free space that was
    // about to be free anyway.
    let after_expiry = ctx.capacity.used_items.saturating_sub(expired.len() as u64);
    if after_expiry <= max_items {
        return Vec::new();
    }

    let mut over = after_expiry - max_items;
    // Rank by what it costs to lose the item, matching `admit`'s eviction
    // order — `eviction::cost` is the one answer to "what is cheapest to lose
    // right now", and both entry points must give it. Age breaks ties only.
    let mut reclaimable: Vec<&MaintenanceCandidate> = ctx
        .batch
        .iter()
        .filter(|c| !expired.contains(&c.item.id) && c.item.protection.is_evictable(ctx.now))
        .collect();
    reclaimable.sort_by(|a, b| {
        eviction::cost(a)
            .total_cmp(&eviction::cost(b))
            .then_with(|| reclaim_rank(&a.item).cmp(&reclaim_rank(&b.item)))
    });

    let mut out = Vec::new();
    for candidate in reclaimable {
        if over == 0 {
            break;
        }
        let item = &candidate.item;
        out.push(Decision {
            subject: Some(item.id.clone()),
            action: Action::Reject,
            evictions: vec![Eviction {
                item: item.id.clone(),
                reason: Reason::new(
                    ReasonCode::CapacityPressure,
                    "namespace is over budget; reclaimed the cheapest evictable item",
                    features! {
                        "used_items" => ctx.capacity.used_items as f64,
                        "max_items" => max_items as f64,
                        "value" => candidate.value.get(),
                        "fragility" => candidate.fragility.get(),
                        "eviction_cost" => eviction::cost(candidate),
                    },
                ),
            }],
            reasons: vec![],
            policy: policy.clone(),
        });
        over -= 1;
    }
    out
}

/// `subject`'s fragility measured against exactly the neighbourhood it is
/// handed, through the same `fragility::score` the write path uses.
///
/// **The stored `subject.fragility` is deliberately not an input.** It is the
/// value this recomputation replaces: it was measured against a corpus that
/// included items this run is deleting. Feeding it in is how a "decay" ends up
/// as `stored * factor`, a number that can only ever fall — see F-A.
///
/// `relevance` is `similarity::overlap(subject, neighbour)`, "how much of the
/// subject could be relearned from that neighbour", which is exactly the
/// question fragility asks of a neighbourhood. It is a token-set proxy rather
/// than a cosine because `MaintainContext` carries no embeddings.
/// `fragility::score` reads only `relevance`; the neighbours' own scores are
/// carried through unchanged rather than invented, and there is no recall
/// query here, so the query-relative fields are `None`.
fn recomputed_fragility(
    subject: &MaintenanceCandidate,
    neighbourhood: &[&MaintenanceCandidate],
    stats: &ScopeStats,
) -> Score {
    let neighbours: Vec<ScoredCandidate> = neighbourhood
        .iter()
        .map(|n| ScoredCandidate {
            item: n.item.clone(),
            relevance: similarity::overlap(&subject.item.body, &n.item.body),
            vector_score: None,
            keyword_score: None,
            value: n.value,
            fragility: n.fragility,
            estimated_tokens: 0,
            last_accessed_at: n.last_accessed_at,
            access_count: n.access_count,
        })
        .collect();
    // `stats` is forwarded exactly as the engine supplied it, as `assess`
    // does — meeting `fragility::score`'s baseline precondition is the
    // caller's job, and a policy cannot check it.
    fragility::score(&neighbours, stats)
}

/// Fragility recomputed against the neighbourhood as it now stands.
///
/// **This can go UP, and that is the case worth having.** `fragility::score`
/// measures how hard an item would be to relearn if it were lost, so an item
/// whose corroborating neighbours this run has just deleted is harder to
/// relearn than it was before the run — `fragility::score` answers an empty
/// neighbourhood with `Score::ONE` outright. The word "decay" describes the
/// usual direction, not the rule; losing a below-average neighbour raises the
/// average of what is left and the value falls, losing the only close one and
/// it rises. Both directions are tested.
///
/// A rise is only ACTED on, though, and only in one way: it can earn the item
/// the same time-boxed protection window `admit` grants at the same threshold.
/// Two limits make that the right size of response:
///
/// * A page is a slice of the namespace, so page-mates undercount an item's
///   true neighbourhood and the recomputed value is an upper bound. The one
///   fact a page can establish on its own is a LOSS — an item that was
///   corroborating this one has just been deleted — which is why the value is
///   read as a change (`now > before`) rather than as a level.
/// * Fragility is not stored on `MemoryItem`, so there is no number to write
///   back. A protection window is the only durable consequence a recomputation
///   here can have, and it expires on its own.
///
/// A fall produces nothing. Revoking a window early is not this step's job;
/// `protection_releases` ends windows when they run out.
fn fragility_decay(
    ctx: &MaintainContext,
    cfg: &BaselineConfig,
    removed: &BTreeSet<ItemId>,
    policy: &PolicyId,
) -> Vec<Decision> {
    // Nothing left the corpus, so no neighbourhood changed and every
    // comparison below would be a value against itself.
    if removed.is_empty() {
        return Vec::new();
    }
    let survivors: Vec<&MaintenanceCandidate> = ctx
        .batch
        .iter()
        .filter(|c| !removed.contains(&c.item.id))
        .collect();

    let mut out = Vec::new();
    for subject in &survivors {
        // Pinned outranks a window already, and an unelapsed window is one.
        if !subject.item.protection.is_evictable(ctx.now) {
            continue;
        }
        let was_neighbours: Vec<&MaintenanceCandidate> = ctx
            .batch
            .iter()
            .filter(|c| c.item.id != subject.item.id)
            .collect();
        let now_neighbours: Vec<&MaintenanceCandidate> = survivors
            .iter()
            .copied()
            .filter(|c| c.item.id != subject.item.id)
            .collect();
        let was = recomputed_fragility(subject, &was_neighbours, &ctx.stats);
        let now = recomputed_fragility(subject, &now_neighbours, &ctx.stats);

        if now <= was || now.get() < cfg.protection_fragile_threshold {
            continue;
        }
        out.push(Decision {
            subject: Some(subject.item.id.clone()),
            action: Action::Retain {
                protection: Protection::Protected {
                    until: ctx.now + Duration::days(cfg.protection_window_days),
                },
            },
            evictions: vec![],
            reasons: vec![Reason::new(
                ReasonCode::ProtectedFragile,
                "corroborating neighbours were removed this run; the item is now \
                 expensive to relearn",
                features! {
                    "fragility" => now.get(),
                    "fragility_before" => was.get(),
                    "threshold" => cfg.protection_fragile_threshold,
                },
            )],
            policy: policy.clone(),
        });
    }
    out
}

/// Protection windows that have run out, returning the item to ordinary
/// eviction eligibility. Skips anything `fragility_decay` has just granted a
/// fresh window to: releasing and re-granting in one run is two contradictory
/// `Action::Retain`s for one row.
fn protection_releases(
    ctx: &MaintainContext,
    removed: &BTreeSet<ItemId>,
    regranted: &BTreeSet<ItemId>,
    policy: &PolicyId,
) -> Vec<Decision> {
    ctx.batch
        .iter()
        .map(|c| &c.item)
        .filter(|item| !removed.contains(&item.id) && !regranted.contains(&item.id))
        .filter_map(|item| match item.protection {
            Protection::Protected { until } if until <= ctx.now => Some(Decision {
                subject: Some(item.id.clone()),
                action: Action::Retain {
                    protection: Protection::Normal,
                },
                evictions: vec![],
                reasons: vec![Reason::new(
                    ReasonCode::ProtectedFragile,
                    "protection window elapsed; item returns to normal eviction eligibility",
                    features! { "expired_at" => until.unix_timestamp() as f64 },
                )],
                policy: policy.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Near-duplicate survivors folded together.
///
/// **Not `admit`'s merge with different arguments.** There the incoming side
/// has no id, no audit history and no embedding, and is merged before it ever
/// exists. Here both sides are stored rows: the absorbed item has an `ItemId`
/// other rows reference, audit records naming it, and an embedding. This
/// function decides only — the engine writes the merged content to `into`,
/// deletes the absorbed item, and files ONE `Merged` audit record naming both,
/// all in one transaction (Task 35, which carries its own test for it).
///
/// **Which side is absorbed is decided by coverage, not by age.**
/// `similarity::overlap(x, y)` is the fraction of `x` already present in `y`,
/// so the item with the higher coverage is the one whose content survives the
/// merge intact — absorbing the other would drop whatever it says that its
/// partner does not. Age (newer first, then the greater `ItemId`) breaks a tie
/// and nothing more; identical bodies cover each other equally and something
/// has to be deterministic. `MergeStrategy::AppendAndUnion` for the same
/// reason: `ReplaceBody` would discard the target's body, which on this path
/// is a stored row's content rather than a candidate that never existed.
///
/// Two items may not be merged if the absorbed side is not removable
/// (`Protection::is_evictable` — a pin, or a window still running), or if
/// either side is already the subject of a decision this run. An item takes
/// part in at most ONE merge per run: a chain of them would have the engine
/// writing merged content into a row the same run deletes. What is left over
/// is picked up by the next run.
fn consolidations(
    ctx: &MaintainContext,
    cfg: &BaselineConfig,
    removed: &BTreeSet<ItemId>,
    spoken_for: &BTreeSet<ItemId>,
    policy: &PolicyId,
) -> Vec<Decision> {
    let mut survivors: Vec<&MemoryItem> = ctx
        .batch
        .iter()
        .map(|c| &c.item)
        .filter(|item| !removed.contains(&item.id))
        .collect();
    // Pair iteration has to be independent of the order the backend listed
    // the page in, or the same namespace consolidates differently run to run.
    survivors.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut claimed: BTreeSet<ItemId> = BTreeSet::new();
    let mut out = Vec::new();
    for (i, &a) in survivors.iter().enumerate() {
        for &b in &survivors[i + 1..] {
            if claimed.contains(&a.id) || claimed.contains(&b.id) {
                continue;
            }
            let mut options = [
                (a, b, similarity::overlap(&a.body, &b.body)),
                (b, a, similarity::overlap(&b.body, &a.body)),
            ];
            // Most-covered absorbed first; then the newer; then the greater
            // id. Every comparison is reversed because the preferred option
            // has to sort to the front.
            options.sort_by(|x, y| {
                y.2.total_cmp(&x.2)
                    .then_with(|| y.0.created_at.cmp(&x.0.created_at))
                    .then_with(|| y.0.id.cmp(&x.0.id))
            });
            for (absorbed, target, coverage) in options {
                if coverage < cfg.merge_threshold
                    || !absorbed.protection.is_evictable(ctx.now)
                    || spoken_for.contains(&absorbed.id)
                {
                    continue;
                }
                out.push(Decision {
                    subject: Some(absorbed.id.clone()),
                    action: Action::Merge {
                        into: target.id.clone(),
                        strategy: MergeStrategy::AppendAndUnion,
                    },
                    evictions: vec![],
                    reasons: vec![Reason::new(
                        ReasonCode::HighRedundancy,
                        "consolidated into a surviving near-duplicate; its content is \
                         already carried there",
                        features! {
                            "similarity" => coverage,
                            "threshold" => cfg.merge_threshold,
                        },
                    )],
                    policy: policy.clone(),
                });
                claimed.insert(absorbed.id.clone());
                claimed.insert(target.id.clone());
                break;
            }
        }
    }
    out
}

/// One page of a namespace's maintenance, as a list of decisions for the
/// engine to apply.
///
/// Emitted in the order the spec narrates maintenance — expire, release,
/// decay, consolidate, reclaim — so the audit trail reads the way the policy
/// is documented. Reclaim is COMPUTED earlier than it is emitted, because
/// decay and consolidation both have to see the survivor set it produces;
/// nothing else depends on emission order.
pub fn decisions(ctx: &MaintainContext, cfg: &BaselineConfig, policy: PolicyId) -> Vec<Decision> {
    let expiries = ttl_expiries(ctx, &policy);
    let mut removed = subjects(&expiries);

    let reclaims = capacity_reclaim(ctx, &removed, &policy);
    removed.extend(subjects(&reclaims));

    let regrants = fragility_decay(ctx, cfg, &removed, &policy);
    let regranted = subjects(&regrants);

    let releases = protection_releases(ctx, &removed, &regranted, &policy);

    // `removed` says who is gone (and so cannot be a merge target either);
    // `spoken_for` says who already has a decision (and so cannot be the
    // absorbed side). A released item is the second without being the first.
    let mut spoken_for = removed.clone();
    spoken_for.extend(regranted);
    spoken_for.extend(subjects(&releases));
    let merges = consolidations(ctx, cfg, &removed, &spoken_for, &policy);

    let mut out = expiries;
    out.extend(releases);
    out.extend(regrants);
    out.extend(merges);
    out.extend(reclaims);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BaselineConfig;
    use crate::testkit::{item, scope};
    use memorysafe_core::{
        Action, Budget, CapacityState, ItemId, MergeStrategy, PolicyId, Protection, Reason,
        ReasonCode, ScopeStats, Score,
    };
    use time::{Duration, OffsetDateTime};

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(365)
    }

    /// The stored `fragility` every fixture candidate arrives carrying.
    ///
    /// Deliberately distinct from every value the decay step's own
    /// recomputation produces in these tests (`0.0` and `1.0` at baseline
    /// `0.4`), so an implementation that echoed the number it was handed
    /// instead of recomputing one is visible in the evidence assertions
    /// rather than hidden behind a coincidence.
    const STORED_FRAGILITY: f32 = 0.25;
    const STORED_VALUE: f32 = 0.5;

    fn candidate_for(item: MemoryItem) -> MaintenanceCandidate {
        MaintenanceCandidate {
            item,
            value: Score::clamped(STORED_VALUE),
            fragility: Score::clamped(STORED_FRAGILITY),
            // Never recalled — `(None, 0)`, the encoding `MaintenanceCandidate`
            // documents. Nothing in this module reads either field.
            last_accessed_at: None,
            access_count: 0,
        }
    }

    /// A corpus baseline that actually means something, as
    /// `fragility::score`'s precondition requires: `ScopeStats::default()`'s
    /// `0.0` is "no data", not "a corpus whose similarity sits at the floor".
    fn stats(mean: f32) -> ScopeStats {
        ScopeStats {
            item_count: 100,
            mean_neighbour_similarity: mean,
            ..Default::default()
        }
    }

    fn ctx(batch: Vec<MemoryItem>, used: u64, max: Option<u64>) -> MaintainContext {
        ctx_with_stats(batch, used, max, ScopeStats::default())
    }

    fn ctx_with_stats(
        batch: Vec<MemoryItem>,
        used: u64,
        max: Option<u64>,
        stats: ScopeStats,
    ) -> MaintainContext {
        MaintainContext {
            scope: scope(),
            batch: batch.into_iter().map(candidate_for).collect(),
            // Every test here hands `decisions` the whole namespace in one
            // page — there is no further page to wait for.
            is_final_batch: true,
            capacity: CapacityState {
                budget: Budget {
                    max_items: max,
                    max_bytes: None,
                },
                used_items: used,
                used_bytes: 0,
            },
            stats,
            now: now(),
        }
    }

    fn pid() -> PolicyId {
        PolicyId::new("baseline", "0.1.0")
    }

    /// Every decision naming `id` as its subject. Several tests assert on the
    /// LENGTH of this, because "exactly one decision names this item" is the
    /// invariant that keeps the engine from applying two contradictory
    /// instructions to one row.
    fn about<'a>(ds: &'a [Decision], id: &ItemId) -> Vec<&'a Decision> {
        ds.iter()
            .filter(|d| d.subject.as_ref() == Some(id))
            .collect()
    }

    fn reason(d: &Decision, code: ReasonCode) -> &Reason {
        d.reasons
            .iter()
            .find(|r| r.code == code)
            .unwrap_or_else(|| panic!("decision carries no {code:?} reason: {:?}", d.reasons))
    }

    fn evidence(r: &Reason, key: &str) -> f64 {
        *r.evidence
            .get(key)
            .unwrap_or_else(|| panic!("reason carries no {key:?} evidence: {:?}", r.evidence))
    }

    // ---------------------------------------------------------------- TTL --

    #[test]
    fn an_expired_item_is_forgotten_with_a_ttl_reason() {
        let mut expired = item("gone stale");
        expired.created_at = now() - Duration::days(10);
        expired.ttl = Some(Duration::days(1));
        let id = expired.id.clone();

        let ds = decisions(
            &ctx(vec![expired], 1, None),
            &BaselineConfig::default(),
            pid(),
        );
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].evictions[0].item, id);
        assert_eq!(ds[0].evictions[0].reason.code, ReasonCode::TtlExpired);
    }

    #[test]
    fn an_unexpired_item_is_left_alone() {
        let mut fresh = item("still good");
        fresh.created_at = now();
        fresh.ttl = Some(Duration::days(30));
        assert!(
            decisions(
                &ctx(vec![fresh], 1, None),
                &BaselineConfig::default(),
                pid()
            )
            .is_empty()
        );
    }

    #[test]
    fn a_pinned_item_does_not_survive_its_own_ttl() {
        // DIVERGENCE from the brief, which asserted the opposite ("a pinned
        // item must never be evicted, TTL included") — see the task report.
        // The brief's own Step 3 implementation contradicts its Step 1 test
        // here: it calls `must_forget` with no pinned-item exemption, under a
        // comment saying there deliberately is none. Three signals agree with
        // the code and against the test:
        //
        //   * `MemoryItem::must_forget`'s doc: "Expiry dominates protection;
        //     protection only governs eviction for capacity." The method
        //     exists ONLY to express that, and `maintain` is its only possible
        //     caller anywhere in the workspace — exempting pinned items here
        //     makes it dead letter and silently un-enforces the guarantee.
        //   * `memorysafe_core`'s own `expiry_dominates_pinning` test asserts
        //     `must_forget` is true for exactly this fixture.
        //   * The governance argument: a pin that defeated a TTL would let any
        //     caller opt out of a legal retention limit by pinning.
        //
        // The half of "pinning is absolute" that IS true — a pin blocks
        // CAPACITY eviction — is asserted below and in
        // `reclaim_never_touches_pinned_items`.
        let mut pinned = item("pinned forever");
        pinned.created_at = now() - Duration::days(10);
        pinned.ttl = Some(Duration::days(1));
        pinned.protection = Protection::Pinned;
        let id = pinned.id.clone();

        let ds = decisions(
            &ctx(vec![pinned], 1, None),
            &BaselineConfig::default(),
            pid(),
        );
        assert_eq!(ds.len(), 1, "a pin overrode a retention limit");
        assert_eq!(ds[0].evictions[0].item, id);
        assert_eq!(
            ds[0].evictions[0].reason.code,
            ReasonCode::TtlExpired,
            "a pinned item past its TTL must be forgotten for expiry, not for capacity"
        );
    }

    #[test]
    fn a_pinned_item_within_its_ttl_is_left_alone() {
        // The other side of the divergence above, and the one that keeps it
        // from reading as "pins do nothing": pinning still exempts an item
        // from everything except its own retention limit. Rejects a repair
        // that dropped the `must_forget` gate and expired pinned items
        // outright.
        let mut pinned = item("pinned and still within its limit");
        pinned.created_at = now();
        pinned.ttl = Some(Duration::days(30));
        pinned.protection = Protection::Pinned;
        assert!(
            decisions(
                &ctx(vec![pinned], 1, None),
                &BaselineConfig::default(),
                pid()
            )
            .is_empty()
        );
    }

    // --------------------------------------------------- protection window --

    #[test]
    fn an_elapsed_protection_window_is_released() {
        let mut protected = item("no longer special");
        protected.protection = Protection::Protected {
            until: now() - Duration::days(1),
        };
        let ds = decisions(
            &ctx(vec![protected], 1, None),
            &BaselineConfig::default(),
            pid(),
        );
        assert_eq!(ds.len(), 1);
        assert!(matches!(
            ds[0].action,
            Action::Retain {
                protection: Protection::Normal
            }
        ));
    }

    #[test]
    fn an_unelapsed_protection_window_is_left_alone() {
        // Rejects `until <= now` weakened to an unconditional release, which
        // would end every protection window the first time maintenance ran.
        let mut protected = item("still special");
        protected.protection = Protection::Protected {
            until: now() + Duration::days(1),
        };
        assert!(
            decisions(
                &ctx(vec![protected], 1, None),
                &BaselineConfig::default(),
                pid()
            )
            .is_empty()
        );
    }

    // ------------------------------------------------------------ reclaim --

    #[test]
    fn an_over_budget_namespace_reclaims_cheapest_first() {
        let batch: Vec<MemoryItem> = (0..5).map(|i| item(&format!("memory {i}"))).collect();
        let ds = decisions(&ctx(batch, 5, Some(3)), &BaselineConfig::default(), pid());
        let evicted: usize = ds.iter().map(|d| d.evictions.len()).sum();
        assert_eq!(
            evicted, 2,
            "5 items against a budget of 3 means 2 evictions"
        );
        assert!(
            ds.iter()
                .any(|d| d.has_reason(ReasonCode::CapacityPressure))
        );
    }

    #[test]
    fn reclaim_takes_the_cheapest_to_lose_first_not_merely_the_oldest() {
        // Rejects a reclaim that ranks on `reclaim_rank` (creation time)
        // alone, ignoring `eviction::cost`. Every fixture below is the same
        // age, so an age-only ranking cannot distinguish them and would evict
        // in batch order, taking `expensive` first. `eviction::cost` is
        // `value * fragility`, so the two are ordered by that product.
        //
        // Vacuous if the fixtures had different ages: the age tie-break would
        // then be doing the work and the cost sort would be untested.
        let expensive = item("irreplaceable and valuable");
        let cheap = item("replaceable and worthless");
        let (expensive_id, cheap_id) = (expensive.id.clone(), cheap.id.clone());

        let mut c = ctx(vec![expensive, cheap], 2, Some(1));
        c.batch[0].value = Score::clamped(1.0);
        c.batch[0].fragility = Score::clamped(1.0); // cost 1.0
        c.batch[1].value = Score::clamped(0.2);
        c.batch[1].fragility = Score::clamped(0.1); // cost 0.02

        let ds = decisions(&c, &BaselineConfig::default(), pid());
        let evicted: Vec<&ItemId> = ds
            .iter()
            .flat_map(|d| d.evictions.iter())
            .map(|e| &e.item)
            .collect();
        assert_eq!(evicted, vec![&cheap_id], "reclaim took the expensive item");
        assert!(!evicted.contains(&&expensive_id));
    }

    #[test]
    fn reclaim_breaks_a_cost_tie_on_age_oldest_first() {
        // Rejects deleting `reclaim_rank` (or its `then_with`), which would
        // leave equal-cost candidates in whatever order the backend listed
        // them — the non-determinism `reclaim_rank` exists to remove.
        // Every candidate here has the same cost by construction, so the age
        // tie-break is the only thing that can decide, and the OLDEST is
        // deliberately listed LAST so batch order and age order disagree.
        let mut newer = item("written this morning");
        newer.created_at = now() - Duration::days(1);
        let mut older = item("written last year");
        older.created_at = now() - Duration::days(300);
        let older_id = older.id.clone();

        let ds = decisions(
            &ctx(vec![newer, older], 2, Some(1)),
            &BaselineConfig::default(),
            pid(),
        );
        let evicted: Vec<&ItemId> = ds
            .iter()
            .flat_map(|d| d.evictions.iter())
            .map(|e| &e.item)
            .collect();
        assert_eq!(
            evicted,
            vec![&older_id],
            "the older of two equal-cost items goes first"
        );
    }

    #[test]
    fn reclaim_never_touches_pinned_items() {
        let mut batch: Vec<MemoryItem> = (0..4).map(|i| item(&format!("memory {i}"))).collect();
        for b in &mut batch {
            b.protection = Protection::Pinned;
        }
        let ds = decisions(&ctx(batch, 4, Some(1)), &BaselineConfig::default(), pid());
        let evicted: usize = ds.iter().map(|d| d.evictions.len()).sum();
        assert_eq!(
            evicted, 0,
            "pinned items are absolute even under capacity pressure"
        );
    }

    #[test]
    fn a_namespace_within_budget_produces_no_decisions() {
        let batch: Vec<MemoryItem> = (0..2).map(|i| item(&format!("memory {i}"))).collect();
        assert!(decisions(&ctx(batch, 2, Some(10)), &BaselineConfig::default(), pid()).is_empty());
    }

    #[test]
    fn expiry_counts_against_the_budget_before_reclaim_does() {
        // Rejects a reclaim that ignores this run's own expiries and computes
        // its overage from `used_items` alone: 3 used against a budget of 2
        // looks like 1 to reclaim, but the TTL step is already removing one,
        // so the correct answer is none. A policy that reclaimed anyway would
        // delete a live item to make room that had just been made.
        let mut expiring = item("gone stale");
        expiring.created_at = now() - Duration::days(10);
        expiring.ttl = Some(Duration::days(1));
        let keep_a = item("alpha beta gamma delta");
        let keep_b = item("epsilon zeta eta theta");

        let ds = decisions(
            &ctx(vec![expiring, keep_a, keep_b], 3, Some(2)),
            &BaselineConfig::default(),
            pid(),
        );
        assert!(
            !ds.iter()
                .any(|d| d.has_reason(ReasonCode::CapacityPressure)),
            "the expiry already brought the namespace within budget"
        );
    }

    // ------------------------------------------------ F-A: fragility decay --

    #[test]
    fn expiring_an_items_only_neighbour_raises_its_fragility() {
        // F-A, stated as the brief states it: two items, mutually each
        // other's only neighbour; expire one; the survivor's fragility must
        // be STRICTLY GREATER afterwards.
        //
        // Rejects the plausible implementation written from the word "decay"
        // — `fragility * decay_factor`, or anything else that can only ever
        // move the number down. Under that implementation both values below
        // are the stored 0.25 scaled by the factor, so `after > before` is
        // false and `after == 1.0` is false. Confirmed by building it; see
        // the task report's falsification section.
        //
        // Both numbers are derived from `fragility::score`'s documented
        // mapping, not read off a run:
        //   before: A's tokens are a subset of B's, so `overlap(A, B) = 5/5 =
        //     1.0`; local_density 1.0 against a 0.4 baseline is the "denser
        //     than typical" branch, `0.5 - 0.5 * (1.0-0.4)/(1.0-0.4)` = 0.0.
        //     Exact in f32: numerator and denominator are the same expression.
        //   after: no neighbours at all, which `fragility::score` answers with
        //     `Score::ONE` outright.
        let a = item("the kernel scheduler uses cfs");
        let b = item("the kernel scheduler uses cfs on linux");
        let (ca, cb) = (candidate_for(a), candidate_for(b));
        let st = stats(0.4);

        let before = recomputed_fragility(&ca, &[&cb], &st);
        let after = recomputed_fragility(&ca, &[], &st);

        // The contract assertion goes FIRST so that it, and not one of the
        // exact anchors below, is what a wrong implementation trips on —
        // the panic then reports the two fragility values it actually
        // produced, which is the evidence that the rejection is real.
        assert!(
            after > before,
            "removing an item's only neighbour must RAISE its fragility: \
             before={} after={}",
            before.get(),
            after.get()
        );
        assert_eq!(before.get(), 0.0, "A is fully covered by B while B exists");
        assert_eq!(after.get(), 1.0, "nothing else says what A says any more");
    }

    #[test]
    fn a_run_that_expires_an_items_only_neighbour_earns_it_a_protection_window() {
        // The same contract as above, through `decisions` rather than the
        // helper, and the second half of F-A's sentence: the resulting
        // decision must carry the RAISED value in its evidence, not the stale
        // one. `STORED_FRAGILITY` (0.25) is what the context handed in; the
        // assertions below reject an implementation that reported it.
        let a = item("the kernel scheduler uses cfs");
        let mut b = item("the kernel scheduler uses cfs on linux");
        b.created_at = now() - Duration::days(10);
        b.ttl = Some(Duration::days(1));
        let (a_id, b_id) = (a.id.clone(), b.id.clone());
        let cfg = BaselineConfig::default();

        let ds = decisions(
            &ctx_with_stats(vec![a, b], 2, None, stats(0.4)),
            &cfg,
            pid(),
        );

        assert_eq!(about(&ds, &b_id).len(), 1, "B is forgotten on TTL");
        assert_eq!(
            about(&ds, &b_id)[0].evictions[0].reason.code,
            ReasonCode::TtlExpired
        );

        let grants = about(&ds, &a_id);
        assert_eq!(
            grants.len(),
            1,
            "A's fragility rose to the protection threshold and nothing said so"
        );
        let grant = grants[0];
        assert!(
            matches!(
                grant.action,
                Action::Retain {
                    protection: Protection::Protected { until }
                } if until == now() + Duration::days(cfg.protection_window_days)
            ),
            "expected a protection window ending 30 days out, got {:?}",
            grant.action
        );

        let r = reason(grant, ReasonCode::ProtectedFragile);
        assert_eq!(
            evidence(r, "fragility"),
            1.0,
            "evidence is not the raised value"
        );
        assert_eq!(evidence(r, "fragility_before"), 0.0);
        assert!(
            evidence(r, "fragility") > evidence(r, "fragility_before"),
            "the audit record must show the rise that justified the window"
        );
        assert_ne!(
            evidence(r, "fragility"),
            STORED_FRAGILITY as f64,
            "evidence carried the stale stored fragility instead of a recomputed one"
        );
    }

    #[test]
    fn losing_a_distant_page_mate_lowers_fragility_rather_than_raising_it() {
        // The other direction, and the reason F-A needs its own test rather
        // than a branch: this recomputation genuinely goes both ways.
        // Rejects "decay always raises" (the over-correction from reading F-A
        // alone) and "decay returns `Score::ONE` for every survivor of a run
        // that removed anything", either of which passes the two tests above.
        //
        // Hand-derived from `fragility::score`, baseline 0.4, local_density
        // being the mean of the top three neighbour similarities:
        //   before: overlaps 0.75 (3 of A's 4 tokens) and 0.25 (1 of 4), mean
        //     0.5 -> 0.5 - 0.5*(0.5-0.4)/0.6 = 0.5 - 1/12 = 0.4166667
        //   after: the 0.25 neighbour is gone, leaving 0.75 alone -> 0.5 -
        //     0.5*(0.75-0.4)/0.6 = 0.5 - 0.2916667 = 0.2083333
        // Removing a BELOW-average neighbour raises the average of what is
        // left, which is why this is a fall and not a rise.
        let a = item("alpha beta gamma delta");
        let near = item("alpha beta gamma epsilon");
        let mut distant = item("alpha zeta eta theta");
        distant.created_at = now() - Duration::days(10);
        distant.ttl = Some(Duration::days(1));

        let (ca, cnear, cdistant) = (
            candidate_for(a.clone()),
            candidate_for(near.clone()),
            candidate_for(distant.clone()),
        );
        let st = stats(0.4);
        let before = recomputed_fragility(&ca, &[&cnear, &cdistant], &st);
        let after = recomputed_fragility(&ca, &[&cnear], &st);

        assert!(
            (before.get() - 0.4166667).abs() < 1e-6,
            "before={}",
            before.get()
        );
        assert!(
            (after.get() - 0.2083333).abs() < 1e-6,
            "after={}",
            after.get()
        );
        assert!(
            after < before,
            "losing a distant neighbour must not raise fragility"
        );

        // And nothing is granted a window off the back of a fall.
        let a_id = a.id.clone();
        let ds = decisions(
            &ctx_with_stats(vec![a, near, distant], 3, None, stats(0.4)),
            &BaselineConfig::default(),
            pid(),
        );
        assert!(
            about(&ds, &a_id).is_empty(),
            "a fall in fragility must not produce a decision"
        );
    }

    #[test]
    fn a_survivor_whose_fragility_does_not_move_is_left_alone() {
        // Rejects "every survivor of a run that removed anything is
        // reassessed into a protection window". Two equally-close neighbours;
        // one goes; the mean of what is left is unchanged, so the value does
        // not move and there is nothing to record.
        let a = item("alpha beta gamma delta");
        let stays = item("alpha beta epsilon zeta");
        let mut goes = item("alpha beta eta theta");
        goes.created_at = now() - Duration::days(10);
        goes.ttl = Some(Duration::days(1));
        let a_id = a.id.clone();

        let ds = decisions(
            &ctx_with_stats(vec![a, stays, goes], 3, None, stats(0.4)),
            &BaselineConfig::default(),
            pid(),
        );
        assert_eq!(
            ds.len(),
            1,
            "only the expiry should have been recorded: {ds:?}"
        );
        assert!(about(&ds, &a_id).is_empty());
    }

    #[test]
    fn decay_reads_its_protection_threshold_from_config() {
        // Rejects a hardcoded 0.85. Identical fixture, two configs, opposite
        // outcomes — and the raised config is set ABOVE the reachable
        // maximum, so no window can be granted however fragile the item got.
        let build = || {
            let a = item("the kernel scheduler uses cfs");
            let mut b = item("the kernel scheduler uses cfs on linux");
            b.created_at = now() - Duration::days(10);
            b.ttl = Some(Duration::days(1));
            let id = a.id.clone();
            (ctx_with_stats(vec![a, b], 2, None, stats(0.4)), id)
        };

        let (c, id) = build();
        assert_eq!(
            about(&decisions(&c, &BaselineConfig::default(), pid()), &id).len(),
            1
        );

        let unreachable = BaselineConfig {
            protection_fragile_threshold: 1.5,
            ..BaselineConfig::default()
        };
        let (c, id) = build();
        assert!(about(&decisions(&c, &unreachable, pid()), &id).is_empty());

        // The boundary is inclusive, as `protection_fragile_threshold`'s own
        // doc says ("fragility at or above this earns ... a protection
        // window"). The fixture's recomputed fragility is exactly 1.0 — an
        // emptied neighbourhood, which `fragility::score` answers with
        // `Score::ONE` — so setting the threshold to exactly 1.0 lands ON it
        // and a strict `>` comparison declines the window.
        let exactly_at_the_ceiling = BaselineConfig {
            protection_fragile_threshold: 1.0,
            ..BaselineConfig::default()
        };
        let (c, id) = build();
        let ds = decisions(&c, &exactly_at_the_ceiling, pid());
        assert_eq!(
            about(&ds, &id).len(),
            1,
            "fragility exactly at the threshold must earn a window: {ds:?}"
        );
        assert_eq!(
            evidence(
                reason(about(&ds, &id)[0], ReasonCode::ProtectedFragile),
                "fragility"
            ),
            1.0
        );
    }

    #[test]
    fn an_item_that_just_lost_its_only_neighbour_is_re_protected_rather_than_released() {
        // Rejects emitting BOTH a release and a grant for one item. Its
        // window has elapsed, so the release step wants to return it to
        // `Normal`; its only neighbour just expired, so the decay step wants
        // to grant it a fresh window. Two `Action::Retain` decisions naming
        // one subject with different protections is a state this policy does
        // not have, and the engine would apply whichever it saw last.
        let mut a = item("the kernel scheduler uses cfs");
        a.protection = Protection::Protected {
            until: now() - Duration::days(1),
        };
        let mut b = item("the kernel scheduler uses cfs on linux");
        b.created_at = now() - Duration::days(10);
        b.ttl = Some(Duration::days(1));
        let a_id = a.id.clone();

        let ds = decisions(
            &ctx_with_stats(vec![a, b], 2, None, stats(0.4)),
            &BaselineConfig::default(),
            pid(),
        );
        let about_a = about(&ds, &a_id);
        assert_eq!(
            about_a.len(),
            1,
            "one item, two contradictory decisions: {about_a:?}"
        );
        assert!(
            matches!(
                about_a[0].action,
                Action::Retain {
                    protection: Protection::Protected { .. }
                }
            ),
            "the fresh window must win over the release, got {:?}",
            about_a[0].action
        );
    }

    #[test]
    fn a_pinned_survivor_is_not_granted_a_protection_window() {
        // Rejects a decay step that reassesses pinned items. A pin already
        // outranks a protection window, so granting one is at best noise in
        // the audit trail and at worst a downgrade when the window ends.
        let mut a = item("the kernel scheduler uses cfs");
        a.protection = Protection::Pinned;
        let mut b = item("the kernel scheduler uses cfs on linux");
        b.created_at = now() - Duration::days(10);
        b.ttl = Some(Duration::days(1));
        let a_id = a.id.clone();

        let ds = decisions(
            &ctx_with_stats(vec![a, b], 2, None, stats(0.4)),
            &BaselineConfig::default(),
            pid(),
        );
        assert!(about(&ds, &a_id).is_empty(), "{:?}", about(&ds, &a_id));
    }

    // ------------------------------------------------- F-B: consolidation --

    #[test]
    fn two_near_duplicate_survivors_are_consolidated_into_one() {
        // F-B: both items already exist, so this is a decision to merge two
        // stored rows — not `admit`'s pre-admission merge, where the incoming
        // side has no id at all. The decision names both: `subject` is the
        // item being absorbed (and deleted by the engine), `into` is the one
        // that survives.
        let mut older = item("postgres runs on port 5432");
        older.created_at = now() - Duration::days(200);
        let mut newer = item("postgres runs on port 5432");
        newer.created_at = now() - Duration::days(100);
        let (older_id, newer_id) = (older.id.clone(), newer.id.clone());
        let cfg = BaselineConfig::default();

        let ds = decisions(
            &ctx_with_stats(vec![older, newer], 2, None, stats(0.4)),
            &cfg,
            pid(),
        );

        assert_eq!(ds.len(), 1, "expected exactly one merge: {ds:?}");
        assert_eq!(
            ds[0].subject.as_ref(),
            Some(&newer_id),
            "identical content is a tie on coverage; the newer is absorbed"
        );
        assert_eq!(
            ds[0].action,
            Action::Merge {
                into: older_id,
                strategy: MergeStrategy::AppendAndUnion,
            },
            "a maintenance merge must not discard the target's body"
        );
        assert!(
            ds[0].evictions.is_empty(),
            "the absorbed item leaves through the merge, not as a separate eviction"
        );
        let r = reason(&ds[0], ReasonCode::HighRedundancy);
        assert_eq!(evidence(r, "similarity"), 1.0);
        assert_eq!(evidence(r, "threshold"), cfg.merge_threshold as f64);
    }

    #[test]
    fn the_item_already_covered_by_the_other_is_the_one_absorbed() {
        // The direction rule, and the reason it is not "absorb the newer":
        // `contained`'s tokens are a strict subset of `extending`'s, so
        // absorbing it loses nothing, while absorbing `extending` would lose
        // four tokens of real content. `contained` is deliberately the OLDER
        // of the two, so an age-only rule gives the opposite answer and this
        // fixture tells the two apart.
        //   overlap(contained, extending) = 5/5 = 1.00  >= 0.93
        //   overlap(extending, contained) = 5/9 = 0.556  <  0.93
        let mut contained = item("postgres runs on port 5432");
        contained.created_at = now() - Duration::days(200);
        let mut extending = item("postgres runs on port 5432 in the staging cluster");
        extending.created_at = now() - Duration::days(100);
        let (contained_id, extending_id) = (contained.id.clone(), extending.id.clone());

        let ds = decisions(
            &ctx_with_stats(vec![contained, extending], 2, None, stats(0.4)),
            &BaselineConfig::default(),
            pid(),
        );

        assert_eq!(ds.len(), 1, "expected exactly one merge: {ds:?}");
        assert_eq!(ds[0].subject.as_ref(), Some(&contained_id));
        assert_eq!(
            ds[0].action,
            Action::Merge {
                into: extending_id,
                strategy: MergeStrategy::AppendAndUnion,
            }
        );
        assert_eq!(
            evidence(reason(&ds[0], ReasonCode::HighRedundancy), "similarity"),
            1.0
        );
    }

    #[test]
    fn consolidation_reads_its_threshold_from_config() {
        // Rejects a hardcoded 0.93, and `>` where the config field's own doc
        // says "at or above": the fixture's coverage is EXACTLY the lowered
        // threshold, so a strict comparison declines the merge.
        //   "alpha beta gamma delta" vs "alpha beta epsilon zeta":
        //   2 of each side's 4 distinct tokens are shared -> 0.5 both ways.
        let build = || {
            let mut older = item("alpha beta gamma delta");
            older.created_at = now() - Duration::days(200);
            let mut newer = item("alpha beta epsilon zeta");
            newer.created_at = now() - Duration::days(100);
            let ids = (older.id.clone(), newer.id.clone());
            (ctx_with_stats(vec![older, newer], 2, None, stats(0.4)), ids)
        };

        let (c, _) = build();
        assert!(
            decisions(&c, &BaselineConfig::default(), pid()).is_empty(),
            "0.5 coverage is nowhere near the default 0.93 threshold"
        );

        let lowered = BaselineConfig {
            merge_threshold: 0.5,
            ..BaselineConfig::default()
        };
        let (c, (older_id, newer_id)) = build();
        let ds = decisions(&c, &lowered, pid());
        assert_eq!(
            ds.len(),
            1,
            "coverage exactly at the threshold must merge: {ds:?}"
        );
        assert_eq!(ds[0].subject.as_ref(), Some(&newer_id));
        assert_eq!(
            ds[0].action,
            Action::Merge {
                into: older_id,
                strategy: MergeStrategy::AppendAndUnion,
            }
        );
    }

    #[test]
    fn a_pinned_item_is_never_the_one_absorbed() {
        // A merge deletes the absorbed row, so the absorbed side must be one
        // the policy is allowed to remove. The pinned item here is the NEWER
        // of the two, so the coverage tie-break picks it first and the
        // implementation has to fall back to the other direction rather than
        // simply landing on the right answer by luck.
        let mut plain = item("postgres runs on port 5432");
        plain.created_at = now() - Duration::days(200);
        let mut pinned = item("postgres runs on port 5432");
        pinned.created_at = now() - Duration::days(100);
        pinned.protection = Protection::Pinned;
        let (plain_id, pinned_id) = (plain.id.clone(), pinned.id.clone());

        let ds = decisions(
            &ctx_with_stats(vec![plain, pinned], 2, None, stats(0.4)),
            &BaselineConfig::default(),
            pid(),
        );

        assert_eq!(ds.len(), 1, "expected exactly one merge: {ds:?}");
        assert_eq!(
            ds[0].subject.as_ref(),
            Some(&plain_id),
            "the pinned item was absorbed, which deletes it"
        );
        assert_eq!(
            ds[0].action,
            Action::Merge {
                into: pinned_id,
                strategy: MergeStrategy::AppendAndUnion,
            }
        );
    }

    #[test]
    fn two_pinned_near_duplicates_are_left_alone() {
        // With no removable side there is no merge to make. Rejects a
        // fallback that absorbs the non-preferred item without re-checking
        // whether IT may be removed either.
        let mut a = item("postgres runs on port 5432");
        a.created_at = now() - Duration::days(200);
        a.protection = Protection::Pinned;
        let mut b = item("postgres runs on port 5432");
        b.created_at = now() - Duration::days(100);
        b.protection = Protection::Pinned;

        assert!(
            decisions(
                &ctx_with_stats(vec![a, b], 2, None, stats(0.4)),
                &BaselineConfig::default(),
                pid()
            )
            .is_empty()
        );
    }

    #[test]
    fn an_item_inside_an_unelapsed_protection_window_is_never_absorbed() {
        // Same rule as the pinned case, through the other arm of
        // `Protection::is_evictable`: a window that has not run out says the
        // item may not be removed yet, and a merge removes it.
        let mut plain = item("postgres runs on port 5432");
        plain.created_at = now() - Duration::days(200);
        let mut protected = item("postgres runs on port 5432");
        protected.created_at = now() - Duration::days(100);
        protected.protection = Protection::Protected {
            until: now() + Duration::days(1),
        };
        let (plain_id, protected_id) = (plain.id.clone(), protected.id.clone());

        let ds = decisions(
            &ctx_with_stats(vec![plain, protected], 2, None, stats(0.4)),
            &BaselineConfig::default(),
            pid(),
        );
        assert_eq!(ds.len(), 1, "expected exactly one merge: {ds:?}");
        assert_eq!(ds[0].subject.as_ref(), Some(&plain_id));
        assert_eq!(
            ds[0].action,
            Action::Merge {
                into: protected_id,
                strategy: MergeStrategy::AppendAndUnion,
            }
        );
    }

    #[test]
    fn an_expired_item_is_forgotten_rather_than_consolidated() {
        // F-B's "both have survived this run's expiries". Rejects
        // consolidation running over the whole batch: it would emit a merge
        // naming an item this same run deletes, and the engine would write
        // merged content into, or out of, a row that is going away.
        let survivor = item("postgres runs on port 5432");
        let mut expiring = item("postgres runs on port 5432");
        expiring.created_at = now() - Duration::days(10);
        expiring.ttl = Some(Duration::days(1));
        let expiring_id = expiring.id.clone();

        let ds = decisions(
            &ctx_with_stats(vec![survivor, expiring], 2, None, stats(0.4)),
            &BaselineConfig::default(),
            pid(),
        );

        assert!(
            !ds.iter().any(|d| matches!(d.action, Action::Merge { .. })),
            "an expiring item was consolidated: {ds:?}"
        );
        assert_eq!(about(&ds, &expiring_id).len(), 1);
        assert_eq!(
            about(&ds, &expiring_id)[0].evictions[0].reason.code,
            ReasonCode::TtlExpired
        );
        // The survivor loses its only neighbour and is re-protected — F-A
        // firing alongside, named here so the count below is not a mystery.
        assert_eq!(ds.len(), 2, "{ds:?}");
    }

    #[test]
    fn no_item_is_both_absorbed_and_a_merge_target_in_one_run() {
        // Three mutually identical items. A naive pairwise loop emits all
        // three merges, and two of them name an item the third deletes: the
        // engine would write merged content into a row it is dropping in the
        // same transaction. One merge per item per run; the rest waits for
        // the next one.
        let mut oldest = item("postgres runs on port 5432");
        oldest.created_at = now() - Duration::days(300);
        let mut middle = item("postgres runs on port 5432");
        middle.created_at = now() - Duration::days(200);
        let mut newest = item("postgres runs on port 5432");
        newest.created_at = now() - Duration::days(100);
        let (oldest_id, middle_id, newest_id) =
            (oldest.id.clone(), middle.id.clone(), newest.id.clone());

        let ds = decisions(
            &ctx_with_stats(vec![oldest, middle, newest], 3, None, stats(0.4)),
            &BaselineConfig::default(),
            pid(),
        );

        assert_eq!(ds.len(), 1, "chained merges: {ds:?}");
        assert_eq!(ds[0].subject.as_ref(), Some(&middle_id));
        assert_eq!(
            ds[0].action,
            Action::Merge {
                into: oldest_id,
                strategy: MergeStrategy::AppendAndUnion,
            }
        );
        assert!(
            about(&ds, &newest_id).is_empty(),
            "the third item must wait for the next run"
        );
    }

    #[test]
    fn an_item_released_from_protection_is_not_also_absorbed_in_the_same_run() {
        // One decision per subject. `released`'s window has elapsed, so it is
        // already the subject of a release; it is also the newer of two
        // identical items, so the coverage tie-break would otherwise absorb
        // it and leave the engine holding a `Retain` and a `Merge` for one
        // row. The implementation must fall back to the other direction.
        let mut other = item("postgres runs on port 5432");
        other.created_at = now() - Duration::days(200);
        let mut released = item("postgres runs on port 5432");
        released.created_at = now() - Duration::days(100);
        released.protection = Protection::Protected {
            until: now() - Duration::days(1),
        };
        let (other_id, released_id) = (other.id.clone(), released.id.clone());

        let ds = decisions(
            &ctx_with_stats(vec![other, released], 2, None, stats(0.4)),
            &BaselineConfig::default(),
            pid(),
        );

        assert_eq!(
            about(&ds, &released_id).len(),
            1,
            "two decisions name one row: {:?}",
            about(&ds, &released_id)
        );
        assert!(matches!(
            about(&ds, &released_id)[0].action,
            Action::Retain {
                protection: Protection::Normal
            }
        ));
        assert_eq!(about(&ds, &other_id).len(), 1);
        assert_eq!(
            about(&ds, &other_id)[0].action,
            Action::Merge {
                into: released_id,
                strategy: MergeStrategy::AppendAndUnion,
            }
        );
    }

    #[test]
    fn decay_sees_this_runs_reclaims_and_not_only_its_expiries() {
        // F-A says the neighbourhood is recomputed "after this run's expiries
        // AND RECLAIMS have removed items". Every other decay test above
        // removes its neighbour with a TTL, so all of them pass an
        // implementation that recomputes against expiries alone and ignores
        // what capacity reclaim took. Here the only removal is a reclaim.
        //
        // Two identical items against a budget of one: the older is
        // reclaimed, and the survivor — whose only neighbour has just gone —
        // must come out of the run maximally fragile and protected.
        let mut reclaimed = item("postgres runs on port 5432");
        reclaimed.created_at = now() - Duration::days(200);
        let mut survivor = item("postgres runs on port 5432");
        survivor.created_at = now() - Duration::days(100);
        let (reclaimed_id, survivor_id) = (reclaimed.id.clone(), survivor.id.clone());

        let ds = decisions(
            &ctx_with_stats(vec![reclaimed, survivor], 2, Some(1), stats(0.4)),
            &BaselineConfig::default(),
            pid(),
        );

        assert_eq!(about(&ds, &reclaimed_id).len(), 1, "{ds:?}");
        assert!(
            about(&ds, &reclaimed_id)[0].has_reason(ReasonCode::CapacityPressure),
            "{ds:?}"
        );

        let grants = about(&ds, &survivor_id);
        assert_eq!(
            grants.len(),
            1,
            "the reclaim took the survivor's only neighbour and decay did not notice: {ds:?}"
        );
        let r = reason(grants[0], ReasonCode::ProtectedFragile);
        assert_eq!(evidence(r, "fragility"), 1.0);
        assert_eq!(evidence(r, "fragility_before"), 0.0);
        assert_eq!(ds.len(), 2, "{ds:?}");
    }

    #[test]
    fn a_reclaimed_item_is_neither_absorbed_nor_a_merge_target() {
        // Why reclaim is COMPUTED before consolidation even though its
        // decisions are emitted last. Three identical items against a budget
        // of two: one is reclaimed and the two survivors consolidate. An
        // implementation that consolidated over the whole batch could name
        // the reclaimed item on either side of the merge, and the engine
        // would write merged content into — or out of — a row it is deleting
        // in the same run.
        //
        // Vacuous with only two items: one survivor leaves no pair to merge,
        // so the reclaimed item could not have been chosen wrongly anyway.
        let mut reclaimed = item("postgres runs on port 5432");
        reclaimed.created_at = now() - Duration::days(300);
        let mut target = item("postgres runs on port 5432");
        target.created_at = now() - Duration::days(200);
        let mut absorbed = item("postgres runs on port 5432");
        absorbed.created_at = now() - Duration::days(100);
        let (reclaimed_id, target_id, absorbed_id) =
            (reclaimed.id.clone(), target.id.clone(), absorbed.id.clone());

        let ds = decisions(
            &ctx_with_stats(vec![reclaimed, target, absorbed], 3, Some(2), stats(0.4)),
            &BaselineConfig::default(),
            pid(),
        );

        assert_eq!(ds.len(), 2, "{ds:?}");
        assert_eq!(about(&ds, &reclaimed_id).len(), 1);
        assert!(
            about(&ds, &reclaimed_id)[0].has_reason(ReasonCode::CapacityPressure),
            "the reclaimed item was consolidated instead: {ds:?}"
        );

        let merges = about(&ds, &absorbed_id);
        assert_eq!(merges.len(), 1, "{ds:?}");
        assert_eq!(
            merges[0].action,
            Action::Merge {
                into: target_id,
                strategy: MergeStrategy::AppendAndUnion,
            },
            "the merge names a row this run is reclaiming"
        );
    }

    // ------------------------------------------------------------- shape --

    #[test]
    fn every_maintenance_decision_names_its_subject_and_its_policy() {
        // `Decision::subject`'s own doc: "`maintain` MUST set it: a
        // maintenance decision with no subject names nobody, so
        // `Action::Retain { protection }` returned from `maintain` ... would
        // be counted and never applied." Rejects reaching for
        // `Decision::retain`/`Decision::reject`, whose constructors leave
        // `subject` as `None` — the shape `admit` uses and `maintain` cannot.
        let mut expiring = item("gone stale");
        expiring.created_at = now() - Duration::days(10);
        expiring.ttl = Some(Duration::days(1));
        let mut released = item("alpha beta gamma delta");
        released.created_at = now() - Duration::days(50);
        released.protection = Protection::Protected {
            until: now() - Duration::days(1),
        };
        let mut dup_a = item("postgres runs on port 5432");
        dup_a.created_at = now() - Duration::days(200);
        let mut dup_b = item("postgres runs on port 5432");
        dup_b.created_at = now() - Duration::days(100);
        // The oldest of the equal-cost survivors, so the single capacity
        // reclaim below takes THIS one rather than one of the three the other
        // assertions depend on.
        let mut filler = item("epsilon zeta eta theta iota");
        filler.created_at = now() - Duration::days(300);

        let ds = decisions(
            &ctx_with_stats(
                vec![expiring, released, dup_a, dup_b, filler],
                5,
                Some(3),
                stats(0.4),
            ),
            &BaselineConfig::default(),
            pid(),
        );

        // Expiry, release, merge and reclaim all represented, so this is not
        // vacuously true of a one-shape result.
        assert!(ds.iter().any(|d| d.has_reason(ReasonCode::TtlExpired)));
        assert!(ds.iter().any(|d| matches!(
            d.action,
            Action::Retain {
                protection: Protection::Normal
            }
        )));
        assert!(ds.iter().any(|d| matches!(d.action, Action::Merge { .. })));
        assert!(
            ds.iter()
                .any(|d| d.has_reason(ReasonCode::CapacityPressure))
        );

        for d in &ds {
            assert!(d.subject.is_some(), "decision names nobody: {d:?}");
            assert_eq!(d.policy, pid(), "decision is unattributed: {d:?}");
        }

        let mut subjects: Vec<&ItemId> = ds.iter().filter_map(|d| d.subject.as_ref()).collect();
        let before_dedup = subjects.len();
        subjects.sort();
        subjects.dedup();
        assert_eq!(
            subjects.len(),
            before_dedup,
            "two decisions name one row: {ds:?}"
        );
    }
}
