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
