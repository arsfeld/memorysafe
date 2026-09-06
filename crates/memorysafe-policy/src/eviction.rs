//! Cost of losing an item — how eviction candidates are ranked whenever a
//! policy path needs to make room by discarding the cheapest thing in scope.
//!
//! Kept as its own module, not a private helper on `admit`: `admit`'s
//! capacity path and `maintain`'s capacity reclaim are the same decision
//! ("what is cheapest to lose right now"), and a governance product that gave
//! two different answers to that question depending on which entry point
//! asked would be a real defect, not a style nit. Anything that ranks
//! eviction/reclaim candidates should call this rather than reimplementing
//! the formula.

use memorysafe_core::MaintenanceCandidate;

/// Cost of losing this item. Low value and low fragility (replaceable,
/// unremarkable) rank lowest, so eviction takes these first; either factor
/// alone is not enough; a highly fragile item — no good substitute exists in
/// the corpus — is only cheap to lose if it is also nearly worthless, and a
/// highly valuable item is only cheap to lose if it is also easily
/// replaceable. It is the product of the two that decides.
pub fn cost(candidate: &MaintenanceCandidate) -> f32 {
    candidate.value.get() * candidate.fragility.get()
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
}
