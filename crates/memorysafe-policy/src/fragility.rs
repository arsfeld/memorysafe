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
/// A neighbourhood exactly as dense as `stats.mean_neighbour_similarity`
/// scores exactly `0.5`; sparser-than-typical scores above `0.5` and
/// denser-than-typical scores below it. Downstream policy code reads that
/// midpoint directly (a fixed `0.5`/`0.8` threshold means "at least as sparse
/// as typical" / "much sparser than typical"), so it is this function's
/// contract, not an implementation detail.
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
    let local_density: f32 = sims[..k].iter().sum::<f32>() / k as f32;

    let baseline = stats.mean_neighbour_similarity.clamp(0.0, 1.0);
    // How much sparser than typical this neighbourhood is, normalised by the
    // headroom above the corpus mean.
    let headroom = (1.0 - baseline).max(1e-3);
    let relative_sparsity = ((baseline - local_density) / headroom + 1.0) / 2.0;

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
