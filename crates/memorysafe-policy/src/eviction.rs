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
    candidate.value.get() * (1.0 - candidate.fragility.get())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::item;
    use memorysafe_core::Score;

    fn fixture(value: f32, fragility: f32) -> MaintenanceCandidate {
        MaintenanceCandidate {
            item: item("x"),
            value: Score::clamped(value),
            fragility: Score::clamped(fragility),
        }
    }

    #[test]
    fn cost_multiplies_value_by_the_room_left_by_fragility() {
        assert_eq!(cost(&fixture(0.8, 0.5)), 0.4);
    }

    #[test]
    fn zero_fragility_costs_exactly_the_value() {
        assert_eq!(cost(&fixture(0.6, 0.0)), 0.6);
    }

    #[test]
    fn maximum_fragility_costs_zero_regardless_of_value() {
        // An irreplaceable item is never the "cheapest" by this formula, no
        // matter how little content signal it carries.
        assert_eq!(cost(&fixture(1.0, 1.0)), 0.0);
    }

    #[test]
    fn cost_reflects_the_product_not_either_factor_alone() {
        // P has BOTH lower value and lower fragility than Q, so ranking by
        // value alone (ascending) or by fragility alone (ascending) would
        // also put P first here — those simpler rankings agree with the
        // product on this pair by coincidence, not because they are
        // equivalent to it. Q's extreme fragility crushes its cost even
        // though Q sits above P on both individual axes, so only the actual
        // product predicts Q as the cheaper one to lose.
        let p = fixture(0.5, 0.1); // cost = 0.45
        let q = fixture(0.6, 0.95); // cost = 0.03
        assert!(
            cost(&q) < cost(&p),
            "Q must rank cheaper despite higher value AND higher fragility \
             than P: cost(p)={}, cost(q)={}",
            cost(&p),
            cost(&q)
        );
    }
}
