use memorysafe_core::{ScopeStats, Score, ScoredCandidate};

/// How costly this memory would be to lose.
///
/// An item with no near neighbours is irreplaceable: nothing else in the
/// corpus carries the same information. An item sitting in a dense cluster is
/// cheap to lose because its neighbours still say most of what it said.
///
/// The comparison is against the corpus's own mean similarity rather than an
/// absolute — 0.6 similarity is unusual in a tightly clustered corpus and
/// unremarkable in a diffuse one. This is the "rare class" notion from the
/// continual-learning lineage, expressed in embedding space.
///
/// `0.5` means corpus-typical density: a neighbourhood exactly as dense as
/// `stats.mean_neighbour_similarity` scores exactly `0.5`. Each side of that
/// midpoint is normalised by its OWN room, not by a shared denominator —
/// sparser-than-typical by the room below the baseline (`baseline` itself),
/// denser-than-typical by the room above it (`1.0 - baseline`). That keeps
/// both `0.0` and `1.0` reachable at every baseline: normalising both sides
/// by the same (upward) room would shrink the reachable ceiling as the
/// baseline moves down — an item with literally no similar neighbours could
/// then never reach maximum fragility in a diffuse corpus — so a fixed
/// downstream threshold like "`>= 0.8` = much sparser than typical" would
/// mean different things, or be unreachable outright, in different corpora.
/// Do not fold the two branches back into one symmetric-looking expression;
/// that shared-denominator shape was the actual defect this one fixes.
///
/// Downstream policy code reads the `0.5` midpoint and higher thresholds
/// directly, so this mapping is this function's contract, not an
/// implementation detail.
///
/// **Precondition:** `stats.mean_neighbour_similarity` must already be a
/// meaningful corpus-level baseline in the sense of `ScopeStats`'s own doc
/// comment on that field — this function reads it as-is and cannot verify
/// that on its own, since `stats` and `neighbours` are independent parameters
/// with no enforced relationship between them. Supplying it unmet (in
/// particular, the field's `0.0`/`Default` "no data" value while
/// `item_count < 2`) is a caller error, not a case this function detects.
pub fn score(neighbours: &[ScoredCandidate], stats: &ScopeStats) -> Score {
    if neighbours.is_empty() {
        return Score::ONE;
    }

    // Mean of the three closest neighbours: robust to a single outlier while
    // still local.
    let mut sims: Vec<f32> = neighbours.iter().map(|n| n.relevance).collect();
    sims.sort_by(|a, b| b.total_cmp(a));
    let k = sims.len().min(3);
    // Cosine can be negative; clamping alongside `baseline` below is what
    // makes each branch's division safe BY CONSTRUCTION rather than by an
    // epsilon floor (see the branch comments). This discards no real signal:
    // neighbours anti-correlated with the item are exactly the "as sparse as
    // it gets" case, which should map to `local_density = 0.0` regardless of
    // how negative the raw cosine got.
    let local_density: f32 = (sims[..k].iter().sum::<f32>() / k as f32).clamp(0.0, 1.0);

    let baseline = stats.mean_neighbour_similarity.clamp(0.0, 1.0);

    // Each side of the `0.5` (corpus-typical) midpoint is normalised by its
    // OWN room — see the doc comment above for why.
    let relative_sparsity = if local_density < baseline {
        // Sparser than typical, normalised by the room below the baseline.
        // Reachable only when `baseline > 0` (`local_density >= 0` and
        // `local_density < baseline`), so this division is never by zero.
        0.5 + 0.5 * (baseline - local_density) / baseline
    } else if local_density > baseline {
        // Denser than typical, normalised by the room above it. Reachable
        // only when `baseline < 1` (`local_density <= 1` and
        // `local_density > baseline`), so this division is never by zero.
        0.5 - 0.5 * (local_density - baseline) / (1.0 - baseline)
    } else {
        // Exactly typical. Kept as its own case rather than falling into
        // either branch above: `local_density == baseline` at the extremes
        // (both `0.0` or both `1.0`) would otherwise divide by zero there.
        0.5
    };

    Score::clamped(relative_sparsity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::candidate;
    use memorysafe_core::ScopeStats;

    fn stats(mean: f32) -> ScopeStats {
        ScopeStats {
            item_count: 100,
            mean_neighbour_similarity: mean,
            ..Default::default()
        }
    }

    #[test]
    fn an_item_with_no_neighbours_is_maximally_fragile() {
        // Nothing like it exists, so losing it loses the information entirely.
        assert_eq!(score(&[], &stats(0.5)), Score::ONE);
    }

    #[test]
    fn an_item_in_a_dense_neighbourhood_is_not_fragile() {
        let dense = [
            candidate("a", 0.95),
            candidate("b", 0.93),
            candidate("c", 0.91),
        ];
        assert!(score(&dense, &stats(0.5)).get() < 0.2);
    }

    #[test]
    fn an_atypical_item_is_more_fragile_than_a_typical_one() {
        let atypical = [candidate("a", 0.20), candidate("b", 0.15)];
        let typical = [candidate("a", 0.85), candidate("b", 0.80)];
        assert!(score(&atypical, &stats(0.5)) > score(&typical, &stats(0.5)));
    }

    #[test]
    fn density_at_the_corpus_mean_scores_exactly_one_half() {
        // 0.5 is the calibration anchor downstream policy code reads
        // directly (compose's fixed 0.5/0.8 thresholds mean "at least as
        // sparse as typical" / "much sparser than typical"): a neighbourhood
        // exactly as dense as the corpus's own mean similarity must land
        // exactly at the midpoint — for any baseline, not by coincidence at
        // one particular value.
        let baseline = 0.42;
        // A single neighbour at exactly the baseline: local_density = b / 1
        // = b with no averaging rounding, so this is exact by construction
        // rather than exact by coincidence (three neighbours all at 0.5
        // would also land exactly on 0.5, but only because 1.5 / 3 happens
        // to divide evenly).
        let at_baseline = [candidate("a", baseline)];
        assert_eq!(score(&at_baseline, &stats(baseline)).get(), 0.5);
    }

    #[test]
    fn a_neighbour_identical_to_the_item_scores_exactly_zero() {
        // local_density == 1.0 is the other calibration anchor: an exact
        // duplicate neighbour is never fragile, however diffuse or tight the
        // rest of the corpus is.
        let identical = [candidate("a", 1.0)];
        assert_eq!(score(&identical, &stats(0.3)).get(), 0.0);
    }

    #[test]
    fn sparser_than_typical_scores_above_half_denser_scores_below() {
        // The gradient either side of the 0.5 anchor: strictly sparser than
        // corpus-typical must land strictly above 0.5, and strictly denser
        // strictly below. This is the function's contract independent of
        // what any particular caller currently computes as `local_density`
        // — it must hold for every caller, including ones that only ever
        // produce values on one side of the anchor today.
        let baseline = 0.5;
        let sparser = [candidate("a", 0.2)];
        let denser = [candidate("a", 0.8)];
        assert!(score(&sparser, &stats(baseline)).get() > 0.5);
        assert!(score(&denser, &stats(baseline)).get() < 0.5);
    }

    #[test]
    fn an_isolated_item_reaches_maximum_fragility_even_in_a_diffuse_corpus() {
        // Regression guard: a shared-denominator rescale (normalising the
        // sparser-than-typical side by the *upward* room, `1.0 - baseline`,
        // instead of its own downward room) caps the reachable ceiling below
        // 1.0 at any baseline under 0.5 — an item with literally no similar
        // neighbours could never be judged maximally fragile in a diffuse
        // corpus, no matter how isolated it actually is. At baseline 0.2 that
        // broken shape returns 0.625; the correct shape must still reach the
        // true ceiling.
        let baseline = 0.2;
        let no_similar_neighbours = [candidate("a", 0.0)];
        assert_eq!(score(&no_similar_neighbours, &stats(baseline)).get(), 1.0);
    }

    #[test]
    fn the_sparser_branch_is_linear_between_the_baseline_and_zero() {
        // A point exactly halfway between corpus-typical (0.5) and total
        // dissimilarity (1.0, at `local_density = 0`) must itself land
        // exactly halfway, at 0.75. The ordering test above only proves
        // sparser-than-typical scores *somewhere* above 0.5; this pins the
        // branch's actual coefficient at an interior point, where an
        // over-scaled or mis-signed term is not masked by the final clamp.
        let baseline = 0.5;
        let midpoint = [candidate("a", baseline / 2.0)];
        assert_eq!(score(&midpoint, &stats(baseline)).get(), 0.75);
    }

    #[test]
    fn the_denser_branch_is_linear_between_the_baseline_and_one() {
        // The mirror image: halfway between corpus-typical (0.5) and a
        // perfect match (0.0, at `local_density = 1.0`) must land exactly
        // halfway, at 0.25.
        let baseline = 0.5;
        let midpoint = [candidate("a", (baseline + 1.0) / 2.0)];
        assert_eq!(score(&midpoint, &stats(baseline)).get(), 0.25);
    }

    #[test]
    fn typical_density_at_the_lower_extreme_scores_one_half_without_dividing_by_zero() {
        // `baseline == local_density == 0.0` is exactly the corner case the
        // third (`else`) branch exists for: folding this equality into
        // either neighbouring branch (`<=` instead of `<`, say) would divide
        // 0.0 by 0.0 there instead of taking this arm.
        let baseline = 0.0;
        let at_baseline = [candidate("a", 0.0)];
        assert_eq!(score(&at_baseline, &stats(baseline)).get(), 0.5);
    }

    #[test]
    fn typical_density_at_the_upper_extreme_scores_one_half_without_dividing_by_zero() {
        // The mirror image at the other extreme: `baseline == local_density
        // == 1.0` must not fall into the denser branch, which would divide
        // 0.0 by 0.0 there.
        let baseline = 1.0;
        let at_baseline = [candidate("a", 1.0)];
        assert_eq!(score(&at_baseline, &stats(baseline)).get(), 0.5);
    }

    #[test]
    fn fragility_is_calibrated_against_the_corpus_not_an_absolute() {
        // The same neighbours mean different things in a tight corpus versus
        // a diffuse one.
        let neighbours = [candidate("a", 0.60), candidate("b", 0.55)];
        let in_tight_corpus = score(&neighbours, &stats(0.85));
        let in_diffuse_corpus = score(&neighbours, &stats(0.20));
        assert!(
            in_tight_corpus > in_diffuse_corpus,
            "0.6 similarity is unusual in a tight corpus and ordinary in a diffuse one"
        );
    }
}
