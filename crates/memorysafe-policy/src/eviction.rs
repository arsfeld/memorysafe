//! Cost of losing an item — how eviction candidates are ranked whenever a
//! policy path needs to make room by discarding the cheapest thing in scope —
//! and, since Task 31's fix round 1, how many of them have to go.
//!
//! Kept as its own module, not a private helper on `admit`: `admit`'s
//! capacity path and `maintain`'s capacity reclaim are the same decision
//! ("what is cheapest to lose right now"), and a governance product that gave
//! two different answers to that question depending on which entry point
//! asked would be a real defect, not a style nit. Anything that ranks
//! eviction/reclaim candidates should call this rather than reimplementing
//! the formula.
//!
//! `evictions_needed` below extends that same reasoning one step further:
//! "what is cheapest to lose" and "how many of those does it take" are both
//! questions `admit`'s make-room loop and `maintain::capacity_reclaim`
//! answer, and a first draft of the byte-budget fix (Task 31) let the two
//! re-derive the second question independently — a ten-line loop, near
//! identical, in both files. Extracted here for the same reason `cost` was
//! already its own function rather than inlined twice.

use memorysafe_core::{CapacityState, MaintenanceCandidate};

/// Cost of losing this item. Low value and low fragility (replaceable,
/// unremarkable) rank lowest, so eviction takes these first; either factor
/// alone is not enough; a highly fragile item — no good substitute exists in
/// the corpus — is only cheap to lose if it is also nearly worthless, and a
/// highly valuable item is only cheap to lose if it is also easily
/// replaceable. It is the product of the two that decides.
pub fn cost(candidate: &MaintenanceCandidate) -> f32 {
    candidate.value.get() * candidate.fragility.get()
}

/// How many leading candidates of `ranked` must be evicted before `base`,
/// with candidates removed one at a time, no longer exceeds budget under an
/// admission of `admit_items`/`admit_bytes` more.
///
/// **The one mechanism `admit`'s make-room loop and `maintain`'s capacity
/// reclaim both need**, extracted so the two cannot independently drift the
/// way a copied `eviction::cost` formula once could have. It projects `base`
/// forward by what evicting the leading candidates would free, asks
/// `CapacityState::would_exceed` whether that projection is still over, and
/// stops the moment it is not. The two current callers differ only in:
///
/// * what `base` is — admission projects from `ctx.capacity` directly;
///   maintenance projects from a capacity already adjusted for this run's own
///   expiries (see `maintain::capacity_reclaim`'s `after_expiry`);
/// * the admission increment — `(1, candidate.byte_size)` for a pending
///   write that has not landed yet, `(0, 0)` to ask "is this state, with
///   nothing more admitted, already over" — a legitimate second shape for
///   `would_exceed`, since `saturating_add` with a zero contributes nothing
///   on either side of the comparison it makes.
///
/// Returns a count in `0..=ranked.len()`; the caller reads `&ranked[..n]` for
/// the candidates to evict, and — if it needs to know whether evicting all of
/// them was still not enough — re-checks `would_exceed` itself against the
/// fully-projected state (this function does not report that; "how many"
/// and "was it enough" are different questions, and only `admit` needs the
/// second one, to decide whether to reject rather than silently overrun).
///
/// Reads nothing from a candidate except `MemoryItem::byte_size()` — the
/// same accessor both callers already used, so there remains exactly one
/// place in this crate that turns "evict this item" into "this many bytes
/// freed". Does not sort or otherwise reorder `ranked`: ranking (`cost`
/// above, plus whatever tie-break a caller adds on top — `maintain` breaks
/// ties on age, `admit` does not) is the caller's job, not this one's.
pub fn evictions_needed(
    base: CapacityState,
    ranked: &[&MaintenanceCandidate],
    admit_items: u64,
    admit_bytes: u64,
) -> usize {
    let mut freed_bytes = 0u64;
    let mut needed = 0usize;
    for (freed_items, candidate) in ranked.iter().enumerate() {
        let freed_items = freed_items as u64;
        let still_over = {
            let mut projected = base;
            projected.used_items = projected.used_items.saturating_sub(freed_items);
            projected.used_bytes = projected.used_bytes.saturating_sub(freed_bytes);
            projected.would_exceed(admit_items, admit_bytes)
        };
        if !still_over {
            break;
        }
        needed += 1;
        freed_bytes += candidate.item.byte_size();
    }
    needed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Delegates to `testkit::maintenance_candidate` (body fixed at `"x"`,
    /// irrelevant to a cost that only reads `value`/`fragility`) so a future
    /// field addition to `MaintenanceCandidate` costs one edit there, not one
    /// per call site.
    fn fixture(value: f32, fragility: f32) -> MaintenanceCandidate {
        crate::testkit::maintenance_candidate("x", value, fragility)
    }

    #[test]
    fn cost_is_the_product_of_value_and_fragility() {
        // NOT fragility 0.5: 0.5 is the fixed point of `x -> 1-x`, so
        // `0.6*0.5` and `0.6*(1-0.5)` both equal `0.3` — a `0.5` fixture
        // cannot tell `value * fragility` from a `value * (1 - fragility)`
        // regression. 0.25 is not fixed by that map (`1-0.25 = 0.75`), so
        // the two formulas disagree (`0.75*0.25 = 0.1875` vs
        // `0.75*0.75 = 0.5625`), and both factors stay dyadic so the
        // equality holds exactly by construction. Do not round this back to
        // 0.5.
        assert_eq!(cost(&fixture(0.75, 0.25)), 0.1875);
    }

    #[test]
    fn zero_fragility_costs_nothing_no_matter_the_value() {
        // Fully replaceable content is cheap to lose regardless of how
        // valuable it looked at write time — matches the doc's "a highly
        // valuable item is only cheap to lose if it is also easily
        // replaceable" at fragility's floor.
        assert_eq!(cost(&fixture(1.0, 0.0)), 0.0);
        assert_eq!(cost(&fixture(0.3, 0.0)), 0.0);
    }

    #[test]
    fn zero_value_costs_nothing_no_matter_the_fragility() {
        // Worthless content is cheap to lose regardless of how irreplaceable
        // it is — matches the doc's "a highly fragile item ... is only cheap
        // to lose if it is also nearly worthless" at value's floor.
        assert_eq!(cost(&fixture(0.0, 1.0)), 0.0);
        assert_eq!(cost(&fixture(0.0, 0.4)), 0.0);
    }

    #[test]
    fn maximum_value_and_fragility_together_cost_the_most() {
        // The mirror of "low value and low fragility rank lowest": an item
        // that is both maximally valuable and maximally irreplaceable is the
        // single most expensive thing in scope to lose.
        assert_eq!(cost(&fixture(1.0, 1.0)), 1.0);
    }

    #[test]
    fn cost_reflects_the_product_not_either_factor_alone() {
        // A true product of two non-negative factors can never disagree with
        // BOTH single-factor rankings on one pair at once: if X is smaller
        // than Y on both value and fragility, X's product is smaller too, by
        // construction. So each half below uses its own pair, moving value
        // and fragility in OPPOSITE directions, to show that factor alone
        // gives the wrong answer — together they cover both of the doc's
        // "only cheap if also ..." claims.

        // Value alone is not enough: X is far MORE valuable than Y, so a
        // value-only ranking would keep X and evict Y first. The product
        // says the opposite: X is nearly worthless to keep because it is so
        // replaceable (low fragility), while Y, though less valuable, is
        // nearly irreplaceable.
        let x = fixture(0.9, 0.2); // cost = 0.9 * 0.2 = 0.18
        let y = fixture(0.3, 0.9); // cost = 0.3 * 0.9 = 0.27
        assert!(
            cost(&x) < cost(&y),
            "X must be cheaper despite its higher value: cost(x)={}, cost(y)={}",
            cost(&x),
            cost(&y)
        );

        // Fragility alone is not enough: M is far MORE fragile than N, so a
        // fragility-only ranking would keep M and evict N first. The product
        // says the opposite: M is nearly worthless regardless of how
        // irreplaceable it is, while N carries enough value to outweigh its
        // comparatively ordinary fragility.
        let m = fixture(0.1, 0.9); // cost = 0.1 * 0.9 = 0.09
        let n = fixture(0.5, 0.5); // cost = 0.5 * 0.5 = 0.25
        assert!(
            cost(&m) < cost(&n),
            "M must be cheaper despite its higher fragility: cost(m)={}, cost(n)={}",
            cost(&m),
            cost(&n)
        );
    }

    // --------------------------------------------------- evictions_needed --

    use memorysafe_core::Budget;

    /// `testkit::item`'s fixed overhead: `kind` ("fact", 4) + `attrs`
    /// (`serde_json::to_string` of an empty map is `"{}"`, 2) + `subject`
    /// ("s", 1) + `namespace` ("n", 1) + `MemoryItem::charge`'s 64-byte
    /// `FIXED_OVERHEAD` = 72; `byte_size()` adds `body.len()` on top. A
    /// 4-character body is therefore 76 bytes — matches
    /// `maintain::tests`' own derivation for the same fixture.
    fn candidates(bodies: &[&str]) -> Vec<MaintenanceCandidate> {
        bodies
            .iter()
            .map(|b| crate::testkit::maintenance_candidate(b, 0.5, 0.5))
            .collect()
    }

    fn refs(cs: &[MaintenanceCandidate]) -> Vec<&MaintenanceCandidate> {
        cs.iter().collect()
    }

    #[test]
    fn evictions_needed_is_zero_when_the_base_is_not_over_budget() {
        let base = CapacityState {
            budget: Budget {
                max_items: Some(10),
                max_bytes: None,
            },
            used_items: 5,
            used_bytes: 0,
        };
        let cs = candidates(&["a", "b", "c"]);
        assert_eq!(evictions_needed(base, &refs(&cs), 0, 0), 0);
    }

    #[test]
    fn evictions_needed_counts_items_needed_to_clear_an_item_budget() {
        let base = CapacityState {
            budget: Budget {
                max_items: Some(3),
                max_bytes: None,
            },
            used_items: 5,
            used_bytes: 0,
        };
        let cs = candidates(&["a", "b", "c", "d", "e"]);
        assert_eq!(
            evictions_needed(base, &refs(&cs), 0, 0),
            2,
            "5 items against a budget of 3 needs 2 evictions"
        );
    }

    #[test]
    fn evictions_needed_counts_items_needed_to_clear_a_byte_budget() {
        // Three 76-byte items (`"aaaa"`, 4 chars, see the derivation above)
        // against a 150-byte budget: evicting one leaves 152 (still over),
        // evicting a second leaves 76 (clear). Mirrors
        // `maintain::tests::a_byte_only_budget_needing_two_evictions_reclaims_exactly_two`,
        // which needs exactly this shape to distinguish a correct
        // `freed_bytes` accumulation from one that over- or under-counts.
        let base = CapacityState {
            budget: Budget {
                max_items: None,
                max_bytes: Some(150),
            },
            used_items: 3,
            used_bytes: 228,
        };
        let cs = candidates(&["aaaa", "bbbb", "cccc"]);
        assert_eq!(evictions_needed(base, &refs(&cs), 0, 0), 2);
    }

    #[test]
    fn evictions_needed_honours_the_admission_increment_not_only_the_standing_state() {
        // The base state alone is within budget (9 <= 10), but the pending
        // admission of 2 more items would break it (11 > 10) — the
        // `(admit_items, admit_bytes)` shape `admit` uses for a not-yet-landed
        // write, as opposed to maintenance's `(0, 0)` standing check.
        let base = CapacityState {
            budget: Budget {
                max_items: Some(10),
                max_bytes: None,
            },
            used_items: 9,
            used_bytes: 0,
        };
        let cs = candidates(&["a"]);
        assert_eq!(
            evictions_needed(base, &refs(&cs), 2, 0),
            1,
            "9 + 2 pending > 10, so the one candidate must go to admit the write"
        );
    }

    #[test]
    fn evictions_needed_returns_the_full_length_when_even_evicting_everything_falls_short() {
        // Only 2 candidates are offered but the budget needs more than that:
        // the function cannot invent a third candidate, so it returns
        // `ranked.len()` — the caller (`admit`) is the one that re-checks
        // whether that was actually enough and rejects if not.
        let base = CapacityState {
            budget: Budget {
                max_items: Some(1),
                max_bytes: None,
            },
            used_items: 5,
            used_bytes: 0,
        };
        let cs = candidates(&["a", "b"]);
        assert_eq!(evictions_needed(base, &refs(&cs), 0, 0), 2);
    }
}
