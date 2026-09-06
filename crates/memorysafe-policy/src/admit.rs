//! Admission decisions: reject an effective duplicate, merge a near-duplicate
//! into its closest neighbour, or retain — protecting fragile content and, if
//! capacity is tight, evicting the cheapest-to-lose existing items to make
//! room.

use crate::config::{BaselineConfig, Verdict};
use crate::eviction;
use memorysafe_core::{
    Action, AdmitContext, Assessed, Decision, Eviction, MaintenanceCandidate, MergeStrategy,
    PolicyId, Protection, Reason, ReasonCode, SensitivityLevel, features,
};
use time::Duration;

/// Pure: everything this needs arrives in `assessed`, `ctx`, and `cfg`. It
/// never queries the backend, so it cannot see anything the caller did not
/// already hand it.
pub fn decide(
    assessed: &Assessed,
    ctx: &AdmitContext,
    cfg: &BaselineConfig,
    policy: PolicyId,
) -> Decision {
    let a = assessed.assessment;
    let best = a.redundancy.score.get();

    match cfg.classify(best) {
        Verdict::NearDuplicate => {
            return Decision::reject(
                policy,
                Reason::new(
                    ReasonCode::NearDuplicate,
                    "an existing memory is effectively identical",
                    features! { "similarity" => best, "threshold" => cfg.duplicate_threshold },
                ),
            );
        }
        Verdict::Mergeable => {
            if let Some((target, similarity)) = a.redundancy.best() {
                return Decision {
                    subject: None,
                    action: Action::Merge {
                        into: target.clone(),
                        strategy: MergeStrategy::AppendAndUnion,
                    },
                    evictions: vec![],
                    reasons: vec![Reason::new(
                        ReasonCode::HighRedundancy,
                        "folded into a closely related existing memory",
                        features! {
                            "similarity" => similarity.get(),
                            "threshold" => cfg.merge_threshold,
                        },
                    )],
                    policy,
                };
            }
            // `near_duplicate_floor` set above `merge_threshold` in a custom
            // config could make this unreachable in practice, but nothing
            // enforces that relationship between the two independently
            // configurable fields — fall through to Retain rather than
            // panic or silently drop the write.
        }
        Verdict::Novel => {}
    }

    // Retain. Decide protection first, then make room.
    //
    // The gate below keys on fragility ALONE — value plays no part, unlike
    // `eviction::cost`, which weights fragility BY value. That is a
    // deliberate asymmetry, not an inconsistency to resolve by making this
    // gate value-aware too.
    //
    // Protection is a time-boxed hedge against value misestimation, not a
    // verdict on the item's worth. The value estimate `a.value` carries here
    // is the least trustworthy it will ever be: it was just computed from
    // ingest-time signal alone, before the item has had any chance to prove
    // itself through reuse. Gating protection on that number would let a
    // single bad early estimate permanently mis-sort an irreplaceable item
    // into the cheap-to-evict tier before its value ever had a chance to be
    // measured properly. Fragility, by contrast, IS trustworthy this early —
    // it is a fact about the corpus (how many near neighbours exist right
    // now), not a prediction about the future — so it is what the grace
    // window is keyed on.
    //
    // `eviction::cost`'s value-weighting is the opposite case, correctly:
    // once the protection window has expired, the item has had time in the
    // corpus for its value to be re-estimated on better information, and a
    // worthless-but-irreplaceable item ranking cheap to evict there is the
    // right answer, not a bug. You cannot get it back, but you also do not
    // want it — fragility alone would protect it forever regardless of
    // value, which is precisely the guarantee this gate's grace window is
    // not meant to extend past its own expiry.
    let fragile = a.fragility.get() >= cfg.protection_fragile_threshold;
    let sensitive = a.sensitivity.level >= SensitivityLevel::Sensitive;

    let mut reasons = Vec::new();
    let protection = if fragile {
        if sensitive {
            // Both axes fire and they disagree about what to do. Keep it and
            // say so, rather than resolving it invisibly.
            reasons.push(Reason::new(
                ReasonCode::SensitivityConflict,
                "fragile enough to protect and sensitive enough to question; retained \
                 and protected, flagged for review",
                features! {
                    "fragility" => a.fragility.get(),
                    "sensitivity_ordinal" => a.sensitivity.level.ordinal() as f64,
                },
            ));
        } else {
            reasons.push(Reason::new(
                ReasonCode::ProtectedFragile,
                "atypical content with few near neighbours; expensive to relearn",
                features! { "fragility" => a.fragility.get() },
            ));
        }
        Protection::Protected {
            until: ctx.now + Duration::days(cfg.protection_window_days),
        }
    } else {
        reasons.push(Reason::new(
            ReasonCode::NovelContent,
            "no sufficiently similar memory exists",
            features! { "best_similarity" => best },
        ));
        Protection::Normal
    };

    // Make room if needed.
    let mut evictions = Vec::new();
    if ctx.capacity.would_exceed(1, assessed.candidate.byte_size) {
        let mut ranked: Vec<&MaintenanceCandidate> = ctx.eviction_candidates.iter().collect();
        ranked.sort_by(|x, y| eviction::cost(x).total_cmp(&eviction::cost(y)));

        let mut freed_items = 0u64;
        let mut freed_bytes = 0u64;
        for c in ranked {
            let still_over = {
                let mut projected = ctx.capacity;
                projected.used_items = projected.used_items.saturating_sub(freed_items);
                projected.used_bytes = projected.used_bytes.saturating_sub(freed_bytes);
                projected.would_exceed(1, assessed.candidate.byte_size)
            };
            if !still_over {
                break;
            }
            evictions.push(Eviction {
                item: c.item.id.clone(),
                reason: Reason::new(
                    ReasonCode::CapacityPressure,
                    "evicted to make room; lowest value-weighted retention cost in scope",
                    features! {
                        "value" => c.value.get(),
                        "fragility" => c.fragility.get(),
                        "eviction_cost" => eviction::cost(c),
                    },
                ),
            });
            freed_items += 1;
            freed_bytes += c.item.byte_size();
        }

        let mut projected = ctx.capacity;
        projected.used_items = projected.used_items.saturating_sub(freed_items);
        projected.used_bytes = projected.used_bytes.saturating_sub(freed_bytes);
        if projected.would_exceed(1, assessed.candidate.byte_size) {
            // Never silently exceed the budget.
            return Decision::reject(
                policy,
                Reason::new(
                    ReasonCode::BudgetExhausted,
                    "the namespace is full and nothing in it is evictable",
                    features! {
                        "used_items" => ctx.capacity.used_items as f64,
                        "evictable" => ctx.eviction_candidates.len() as f64,
                        "pressure" => ctx.capacity.pressure(),
                    },
                ),
            );
        }
    }

    Decision {
        subject: None,
        action: Action::Retain { protection },
        evictions,
        reasons,
        policy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{candidate, candidate_from, scope};
    use memorysafe_core::{
        Assessment, AssessorId, Budget, Candidate, CapacityState, ReasonCode, ScopeStats, Score,
        ScoredCandidate, SensitivityAssessment,
    };
    use time::OffsetDateTime;

    fn ctx(used: u64, max: Option<u64>, evictable: Vec<MaintenanceCandidate>) -> AdmitContext {
        AdmitContext {
            scope: scope(),
            capacity: CapacityState {
                budget: Budget {
                    max_items: max,
                    max_bytes: None,
                },
                used_items: used,
                used_bytes: 0,
            },
            eviction_candidates: evictable,
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn assessed(
        similarity: f32,
        value: f32,
        fragility: f32,
        level: SensitivityLevel,
    ) -> (Candidate, Assessment) {
        let cand = candidate_from("a new memory", None);
        let neighbours: Vec<ScoredCandidate> = if similarity > 0.0 {
            vec![candidate("near", similarity)]
        } else {
            vec![]
        };
        let a = Assessment {
            value: Score::clamped(value),
            fragility: Score::clamped(fragility),
            sensitivity: SensitivityAssessment {
                level,
                categories: vec![],
                confidence: Score::clamped(0.8),
            },
            redundancy: crate::redundancy::assess(&neighbours, &BaselineConfig::default()),
            features: Default::default(),
            assessor: AssessorId::new("baseline", "0.1.0"),
        };
        (cand, a)
    }

    fn pid() -> PolicyId {
        PolicyId::new("baseline", "0.1.0")
    }

    /// A `MaintenanceCandidate` fixture for the eviction path. NOT
    /// `testkit::candidate` — that returns a `ScoredCandidate`, and
    /// `AdmitContext::eviction_candidates` is `Vec<MaintenanceCandidate>`
    /// (see the discrepancy note in the task report). Delegates to
    /// `testkit::maintenance_candidate` so a future field addition to
    /// `MaintenanceCandidate` costs one edit there, not one per call site.
    fn evictable(body: &str, value: f32, fragility: f32) -> MaintenanceCandidate {
        crate::testkit::maintenance_candidate(body, value, fragility)
    }

    #[test]
    fn an_exact_duplicate_is_rejected() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(0.99, 0.8, 0.2, SensitivityLevel::Internal);
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &ctx(0, None, vec![]),
            &cfg,
            pid(),
        );
        assert!(matches!(d.action, Action::Reject));
        assert!(d.has_reason(ReasonCode::NearDuplicate));
    }

    #[test]
    fn a_near_duplicate_merges_into_its_closest_neighbour() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(0.95, 0.8, 0.2, SensitivityLevel::Internal);
        let expected = a.redundancy.near_duplicates[0].0.clone();
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &ctx(0, None, vec![]),
            &cfg,
            pid(),
        );
        match &d.action {
            Action::Merge { into, .. } => assert_eq!(*into, expected),
            other => panic!("expected a merge, got {other:?}"),
        }
        assert!(d.has_reason(ReasonCode::HighRedundancy));
    }

    #[test]
    fn novel_content_is_retained_with_no_evictions() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(0.1, 0.8, 0.2, SensitivityLevel::Internal);
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &ctx(0, Some(100), vec![]),
            &cfg,
            pid(),
        );
        assert!(matches!(
            d.action,
            Action::Retain {
                protection: Protection::Normal
            }
        ));
        assert!(d.evictions.is_empty());
        assert!(d.has_reason(ReasonCode::NovelContent));
    }

    #[test]
    fn under_pressure_the_cheapest_items_are_evicted_first() {
        let cfg = BaselineConfig::default();
        // Deliberate departure from the brief's verbatim fixture (a
        // coordinator-approved fix, plan to be amended elsewhere).
        // `eviction::cost` is `value * fragility` (low on either factor is
        // cheap to lose; the brief's own inverted formula was the actual
        // defect, not this test). The brief's pair (0.1/0.1 vs 0.9/0.9) is
        // lower/higher on BOTH factors at once, so ranking by `value` alone
        // or by `fragility` alone (ascending, either one) would evict the
        // same item as the real product — this pair moves the factors in
        // OPPOSITE directions so a single-factor stand-in gets it wrong:
        //
        //   cheap:    value 0.9, fragility 0.2  -> cost = 0.9 * 0.2 = 0.18
        //   precious: value 0.3, fragility 0.9  -> cost = 0.3 * 0.9 = 0.27
        //
        // cost(cheap) = 0.18 < cost(precious) = 0.27: `cheap` is evicted
        // despite its HIGHER value, because it is common enough in the
        // corpus (low fragility) that losing it costs almost nothing, while
        // `precious` — lower value but essentially irreplaceable — is worth
        // more to keep. A value-only ranking would keep `cheap` (it has the
        // higher value) and evict `precious` instead — the wrong answer.
        let cheap = evictable("cheap to lose", 0.9, 0.2);
        let precious = evictable("expensive to lose", 0.3, 0.9);
        let cheap_id = cheap.item.id.clone();

        let (c, a) = assessed(0.1, 0.8, 0.2, SensitivityLevel::Internal);
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &ctx(10, Some(10), vec![precious, cheap]),
            &cfg,
            pid(),
        );

        assert!(matches!(d.action, Action::Retain { .. }));
        assert_eq!(
            d.evictions.len(),
            1,
            "exactly one eviction makes exactly enough room"
        );
        assert_eq!(
            d.evictions[0].item, cheap_id,
            "must evict the more-replaceable item despite its higher value — \
             a value-only ranking would evict `precious` instead"
        );
        assert_eq!(d.evictions[0].reason.code, ReasonCode::CapacityPressure);
    }

    #[test]
    fn a_byte_only_budget_frees_enough_room_after_one_eviction() {
        // Every other capacity test uses `ctx()`, which hardcodes
        // `max_bytes: None` — so nothing above exercises `freed_bytes`
        // actually accumulating across the eviction loop. Mutation testing
        // found exactly this: `freed_bytes += ...` regressed to `*=` (which
        // leaves it stuck at zero from its `0` initial value) survived every
        // test in this module, because none of them constrain on bytes. This
        // sets up a bytes-only budget where one eviction frees enough bytes
        // to fit — under the `*=` regression, `freed_bytes` never leaves
        // zero, so the final check still sees the pre-eviction byte count
        // and rejects with `BudgetExhausted` instead of retaining.
        let cfg = BaselineConfig::default();
        let victim = evictable("something large enough to evict", 0.1, 0.1);
        let admit_ctx = AdmitContext {
            scope: scope(),
            capacity: CapacityState {
                budget: Budget {
                    max_items: None,
                    max_bytes: Some(100),
                },
                used_items: 0,
                used_bytes: 100,
            },
            eviction_candidates: vec![victim],
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        };
        let (c, a) = assessed(0.1, 0.8, 0.2, SensitivityLevel::Internal);
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &admit_ctx,
            &cfg,
            pid(),
        );
        assert!(
            matches!(d.action, Action::Retain { .. }),
            "one eviction should have freed enough bytes to fit; got {:?}",
            d.action
        );
        assert_eq!(d.evictions.len(), 1);
    }

    #[test]
    fn a_full_scope_with_nothing_evictable_rejects_rather_than_overflowing() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(0.1, 0.8, 0.2, SensitivityLevel::Internal);
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &ctx(10, Some(10), vec![]),
            &cfg,
            pid(),
        );
        assert!(matches!(d.action, Action::Reject));
        assert!(d.has_reason(ReasonCode::BudgetExhausted));
    }

    #[test]
    fn a_fragile_and_sensitive_item_is_protected_and_the_conflict_is_recorded() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(0.1, 0.8, 0.95, SensitivityLevel::Restricted);
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &ctx(0, Some(100), vec![]),
            &cfg,
            pid(),
        );
        assert!(
            matches!(
                d.action,
                Action::Retain {
                    protection: Protection::Protected { .. }
                }
            ),
            "got {:?}",
            d.action
        );
        assert!(
            d.has_reason(ReasonCode::SensitivityConflict),
            "the conflict must be visible in the audit trail"
        );
    }

    #[test]
    fn a_fragile_but_ordinary_item_is_protected_without_a_conflict() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(0.1, 0.8, 0.95, SensitivityLevel::Internal);
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &ctx(0, Some(100), vec![]),
            &cfg,
            pid(),
        );
        assert!(d.has_reason(ReasonCode::ProtectedFragile));
        assert!(!d.has_reason(ReasonCode::SensitivityConflict));
    }

    // Boundary discipline: `duplicate_threshold`, `merge_threshold`, and
    // `protection_fragile_threshold` are all compared with `>=`. Every test
    // above sits comfortably clear of a boundary; a `>=` regressed to `>` (or
    // vice versa) would pass every one of them. These pin the boundary itself.

    #[test]
    fn an_item_exactly_at_the_duplicate_threshold_is_rejected() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(
            cfg.duplicate_threshold,
            0.8,
            0.2,
            SensitivityLevel::Internal,
        );
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &ctx(0, None, vec![]),
            &cfg,
            pid(),
        );
        assert!(matches!(d.action, Action::Reject));
        assert!(d.has_reason(ReasonCode::NearDuplicate));
    }

    #[test]
    fn an_item_exactly_at_the_merge_threshold_merges() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(cfg.merge_threshold, 0.8, 0.2, SensitivityLevel::Internal);
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &ctx(0, None, vec![]),
            &cfg,
            pid(),
        );
        assert!(
            matches!(d.action, Action::Merge { .. }),
            "got {:?}",
            d.action
        );
        assert!(d.has_reason(ReasonCode::HighRedundancy));
    }

    #[test]
    fn fragility_exactly_at_the_threshold_is_protected() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(
            0.1,
            0.8,
            cfg.protection_fragile_threshold,
            SensitivityLevel::Internal,
        );
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &ctx(0, Some(100), vec![]),
            &cfg,
            pid(),
        );
        assert!(
            matches!(
                d.action,
                Action::Retain {
                    protection: Protection::Protected { .. }
                }
            ),
            "got {:?}",
            d.action
        );
        assert!(d.has_reason(ReasonCode::ProtectedFragile));
    }

    #[test]
    fn fragility_just_below_the_threshold_is_not_protected() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(
            0.1,
            0.8,
            cfg.protection_fragile_threshold - 0.01,
            SensitivityLevel::Internal,
        );
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &ctx(0, Some(100), vec![]),
            &cfg,
            pid(),
        );
        assert!(
            matches!(
                d.action,
                Action::Retain {
                    protection: Protection::Normal
                }
            ),
            "got {:?}",
            d.action
        );
        assert!(!d.has_reason(ReasonCode::ProtectedFragile));
    }

    // Security-relevant boundary: the fragile+sensitive conflict gate uses
    // `SensitivityLevel::Sensitive` as its own inclusive floor. A test only
    // exercising `Restricted` (above) and `Internal` (two levels below)
    // would miss a `>=` regressed to `>`, since `Restricted` still triggers
    // it either way and `Internal` triggers neither. These pin the actual
    // adjacent boundary.

    #[test]
    fn a_fragile_item_at_exactly_the_sensitive_level_is_flagged_as_a_conflict() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(0.1, 0.8, 0.95, SensitivityLevel::Sensitive);
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &ctx(0, Some(100), vec![]),
            &cfg,
            pid(),
        );
        assert!(
            d.has_reason(ReasonCode::SensitivityConflict),
            "Sensitive itself must trigger the conflict, not only levels above it"
        );
    }

    #[test]
    fn a_fragile_item_just_below_the_sensitive_level_is_not_flagged() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(0.1, 0.8, 0.95, SensitivityLevel::Personal);
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &ctx(0, Some(100), vec![]),
            &cfg,
            pid(),
        );
        assert!(d.has_reason(ReasonCode::ProtectedFragile));
        assert!(!d.has_reason(ReasonCode::SensitivityConflict));
    }

    // F3-style config-wiring tests (see `redundancy.rs`/`value.rs`): a
    // non-default config value, shown to change the observed behaviour —
    // proving the field is actually read, not merely decorative alongside a
    // still-hardcoded literal that happens to match the default.

    #[test]
    fn protection_fragile_threshold_is_configurable() {
        let cfg = BaselineConfig {
            protection_fragile_threshold: 0.5,
            ..BaselineConfig::default()
        };
        // 0.6 is below the crate default (0.85) but above this config's 0.5.
        let (c, a) = assessed(0.1, 0.8, 0.6, SensitivityLevel::Internal);
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &ctx(0, Some(100), vec![]),
            &cfg,
            pid(),
        );
        assert!(
            d.has_reason(ReasonCode::ProtectedFragile),
            "fragility above the CONFIGURED threshold must be protected"
        );
    }

    #[test]
    fn protection_window_length_is_configurable() {
        let cfg = BaselineConfig {
            protection_window_days: 5,
            ..BaselineConfig::default()
        };
        let (c, a) = assessed(0.1, 0.8, 0.95, SensitivityLevel::Internal);
        let admit_ctx = ctx(0, Some(100), vec![]);
        let d = decide(
            &Assessed {
                candidate: &c,
                assessment: &a,
            },
            &admit_ctx,
            &cfg,
            pid(),
        );
        match d.action {
            Action::Retain {
                protection: Protection::Protected { until },
            } => {
                assert_eq!(until, admit_ctx.now + time::Duration::days(5));
            }
            other => panic!("expected protected, got {other:?}"),
        }
    }
}
