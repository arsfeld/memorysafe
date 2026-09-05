use serde::{Deserialize, Serialize};

/// A namespace's capacity budget. `None` means unbounded on that dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Budget {
    pub max_items: Option<u64>,
    pub max_bytes: Option<u64>,
}

impl Budget {
    pub const UNBOUNDED: Budget = Budget {
        max_items: None,
        max_bytes: None,
    };

    pub fn is_bounded(&self) -> bool {
        self.max_items.is_some() || self.max_bytes.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityState {
    pub budget: Budget,
    pub used_items: u64,
    pub used_bytes: u64,
}

fn ratio(used: u64, max: u64) -> f32 {
    if max == 0 {
        return 1.0;
    }
    (used as f32 / max as f32).min(1.0)
}

impl CapacityState {
    /// `0.0` when unbounded or empty, `1.0` at or over budget. The maximum
    /// across bounded dimensions — the tightest constraint governs.
    pub fn pressure(&self) -> f32 {
        let mut p: f32 = 0.0;
        if let Some(max) = self.budget.max_items {
            p = p.max(ratio(self.used_items, max));
        }
        if let Some(max) = self.budget.max_bytes {
            p = p.max(ratio(self.used_bytes, max));
        }
        p
    }

    /// True when admitting `items`/`bytes` more would break the budget.
    ///
    /// Argument order is (items, bytes). Both are `u64`, so a transposition
    /// compiles silently and checks each quantity against the wrong budget.
    /// There is no compiler-enforced guard against that today; the intended
    /// caller shape is `would_exceed(1, item.byte_size())`, admitting one item
    /// at a time. If a caller ever needs a variable item count, replace these
    /// positional parameters with a named-field argument before doing so.
    pub fn would_exceed(&self, items: u64, bytes: u64) -> bool {
        let items_over = self
            .budget
            .max_items
            .is_some_and(|max| self.used_items.saturating_add(items) > max);
        let bytes_over = self
            .budget
            .max_bytes
            .is_some_and(|max| self.used_bytes.saturating_add(bytes) > max);
        items_over || bytes_over
    }
}

/// Corpus statistics a policy needs but must not query for itself.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ScopeStats {
    pub item_count: u64,
    pub total_bytes: u64,
    /// Mean pairwise cosine similarity of a sample, used to calibrate what
    /// "atypical" means in this particular corpus.
    ///
    /// **Meaningless below `item_count >= 2`** — there are no pairs to average,
    /// and the `Default` of `0.0` is "no data", not "a corpus whose baseline
    /// similarity sits at the floor". The engine recomputes this from the
    /// neighbours it just fetched before handing it to a policy, so the default
    /// only survives when a scope has no neighbours at all — the case where
    /// fragility scoring already short-circuits to maximum. Any future consumer
    /// that reads this directly must check `item_count` first.
    pub mean_neighbour_similarity: f32,
    pub median_item_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(used_items: u64, max_items: Option<u64>) -> CapacityState {
        CapacityState {
            budget: Budget {
                max_items,
                max_bytes: None,
            },
            used_items,
            used_bytes: 0,
        }
    }

    #[test]
    fn unbounded_budget_has_no_pressure() {
        assert_eq!(state(1_000_000, None).pressure(), 0.0);
    }

    #[test]
    fn pressure_is_the_max_of_the_bounded_dimensions() {
        assert_eq!(state(50, Some(100)).pressure(), 0.5);
        assert_eq!(state(100, Some(100)).pressure(), 1.0);
        // Over budget saturates rather than exceeding 1.0.
        assert_eq!(state(200, Some(100)).pressure(), 1.0);
    }

    #[test]
    fn byte_pressure_counts_too() {
        let s = CapacityState {
            budget: Budget {
                max_items: Some(100),
                max_bytes: Some(1000),
            },
            used_items: 10,  // 0.1
            used_bytes: 900, // 0.9  <- dominates
        };
        assert!((s.pressure() - 0.9).abs() < 1e-6);
    }

    #[test]
    fn would_exceed_detects_the_admission_boundary() {
        let s = state(99, Some(100));
        assert!(!s.would_exceed(1, 0));
        assert!(s.would_exceed(2, 0));
    }

    #[test]
    fn zero_budget_is_always_full() {
        assert_eq!(state(0, Some(0)).pressure(), 1.0);
        assert!(state(0, Some(0)).would_exceed(1, 0));
    }

    #[test]
    fn standing_check_already_overrun_state_pressure_saturates() {
        // Invariant: pressure() should never exceed 1.0, even if already overrun.
        let s = state(200, Some(100));
        assert_eq!(s.pressure(), 1.0);
        assert!(s.pressure() <= 1.0);
    }

    #[test]
    fn standing_check_already_overrun_state_would_exceed() {
        // Invariant: would_exceed should behave sanely when used already exceeds max.
        let s = state(200, Some(100));
        assert!(s.would_exceed(0, 0)); // Already over budget, any addition exceeds
        assert!(s.would_exceed(1, 0));
    }

    #[test]
    fn standing_check_both_dimensions_overrun() {
        // Both dimensions over budget: pressure should be 1.0
        let s = CapacityState {
            budget: Budget {
                max_items: Some(100),
                max_bytes: Some(1000),
            },
            used_items: 200,
            used_bytes: 2000,
        };
        assert_eq!(s.pressure(), 1.0);
        assert!(s.would_exceed(0, 0));
    }

    #[test]
    fn standing_check_saturating_add_overflow() {
        // Verify saturating_add prevents overflow: u64::MAX + 1 = u64::MAX
        let s = CapacityState {
            budget: Budget {
                max_items: Some(100),
                max_bytes: None,
            },
            used_items: u64::MAX - 50,
            used_bytes: 0,
        };
        assert!(s.would_exceed(100, 0)); // u64::MAX saturates, still exceeds 100
    }

    #[test]
    fn pressure_takes_the_max_in_both_directions() {
        // Every other dual-bounded case here has bytes >= items, so a mutant
        // that OVERWRITES with the byte ratio instead of taking the max would
        // pass. This is the case that catches last-writer-wins.
        let items_dominate = CapacityState {
            budget: Budget {
                max_items: Some(100),
                max_bytes: Some(1000),
            },
            used_items: 80,  // 0.8  <- dominates
            used_bytes: 200, // 0.2
        };
        assert!((items_dominate.pressure() - 0.8).abs() < 1e-6);
    }

    #[test]
    fn a_bytes_only_budget_is_still_enforced() {
        // No test fed this Budget shape to pressure()/would_exceed(), so a bug
        // nesting the byte check inside the item check would be invisible.
        let s = CapacityState {
            budget: Budget {
                max_items: None,
                max_bytes: Some(1000),
            },
            used_items: 999_999,
            used_bytes: 900,
        };
        assert!(
            (s.pressure() - 0.9).abs() < 1e-6,
            "byte budget inert without max_items"
        );
        assert!(!s.would_exceed(1_000_000, 50));
        assert!(s.would_exceed(0, 200));
    }

    #[test]
    fn an_items_only_budget_ignores_bytes() {
        let s = CapacityState {
            budget: Budget {
                max_items: Some(10),
                max_bytes: None,
            },
            used_items: 5,
            used_bytes: u64::MAX / 2,
        };
        assert!((s.pressure() - 0.5).abs() < 1e-6);
        assert!(!s.would_exceed(5, u64::MAX / 2));
        assert!(s.would_exceed(6, 0));
    }

    #[test]
    fn standing_check_byte_budget_enforcement() {
        // Verify byte budget is also enforced
        let s = CapacityState {
            budget: Budget {
                max_items: Some(1000),
                max_bytes: Some(500),
            },
            used_items: 10,
            used_bytes: 450,
        };
        assert!(!s.would_exceed(1, 49)); // 450 + 49 = 499, 499 > 500 is false
        assert!(!s.would_exceed(1, 50)); // 450 + 50 = 500, 500 > 500 is false
        assert!(s.would_exceed(1, 51)); // 450 + 51 = 501, 501 > 500 is true
    }

    #[test]
    fn standing_check_deserialize_with_overrun() {
        // Verify that deserializing an already-overrun state still behaves correctly
        // (This would be done in real code via serde, but we test the invariant here)
        let serialized =
            r#"{"budget":{"max_items":100,"max_bytes":null},"used_items":150,"used_bytes":0}"#;
        let s: CapacityState = serde_json::from_str(serialized).expect("deserialize");
        // Already overrun: pressure should still be <= 1.0
        assert_eq!(s.pressure(), 1.0);
        assert!(s.pressure() <= 1.0);
        // would_exceed should reject even 0 additional items
        assert!(s.would_exceed(0, 0));
    }

    #[test]
    fn standing_check_budget_is_bounded() {
        let unbounded = Budget::UNBOUNDED;
        assert!(!unbounded.is_bounded());

        let items_only = Budget {
            max_items: Some(100),
            max_bytes: None,
        };
        assert!(items_only.is_bounded());

        let bytes_only = Budget {
            max_items: None,
            max_bytes: Some(100),
        };
        assert!(bytes_only.is_bounded());
    }

    #[test]
    fn standing_check_scope_stats_default() {
        let stats = ScopeStats::default();
        assert_eq!(stats.item_count, 0);
        assert_eq!(stats.total_bytes, 0);
        assert_eq!(stats.mean_neighbour_similarity, 0.0);
        assert_eq!(stats.median_item_bytes, 0);
    }

    #[test]
    fn an_unbounded_budget_never_exceeds_and_has_no_pressure() {
        let s = CapacityState {
            budget: Budget::UNBOUNDED,
            used_items: u64::MAX,
            used_bytes: u64::MAX,
        };
        assert_eq!(s.pressure(), 0.0);
        assert!(!s.would_exceed(u64::MAX, u64::MAX));
        assert!(!s.budget.is_bounded());
    }

    #[test]
    fn is_bounded_is_true_when_both_dimensions_are_set() {
        // The one Budget shape no test exercised.
        let both = Budget {
            max_items: Some(1),
            max_bytes: Some(1),
        };
        assert!(both.is_bounded());
    }

    #[test]
    fn pressure_stays_accurate_at_multi_gigabyte_budgets() {
        // `ratio` casts u64 -> f32, which loses integer precision above 2^24.
        // Admission (`would_exceed`) is exact integer arithmetic and unaffected;
        // this pins how far the *pressure signal* may drift at realistic scale.
        let ten_gb: u64 = 10 * 1024 * 1024 * 1024;
        let s = CapacityState {
            budget: Budget {
                max_items: None,
                max_bytes: Some(ten_gb),
            },
            used_items: 0,
            used_bytes: ten_gb / 4,
        };
        assert!(
            (s.pressure() - 0.25).abs() < 1e-4,
            "pressure drifted: {}",
            s.pressure()
        );

        // The exact path stays exact at the same scale.
        let exact = CapacityState {
            budget: Budget {
                max_items: None,
                max_bytes: Some(ten_gb),
            },
            used_items: 0,
            used_bytes: ten_gb - 1,
        };
        assert!(!exact.would_exceed(0, 1), "exact fit rejected at 10 GiB");
        assert!(exact.would_exceed(0, 2), "overflow accepted at 10 GiB");
    }
}
