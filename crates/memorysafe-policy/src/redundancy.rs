use crate::config::BaselineConfig;
use memorysafe_core::{RedundancyAssessment, Score, ScoredCandidate};

pub use crate::config::Verdict;

/// Redundancy is the similarity of the closest existing memory. Neighbours
/// arrive sorted from the engine, but the sort is repeated here so the
/// function is correct in isolation and testable with hand-built fixtures.
pub fn assess(neighbours: &[ScoredCandidate], cfg: &BaselineConfig) -> RedundancyAssessment {
    let mut near: Vec<(memorysafe_core::ItemId, Score)> = neighbours
        .iter()
        .filter(|n| n.relevance >= cfg.near_duplicate_floor)
        // Clamped, not `new`: the floor already excludes negatives, and a
        // relevance marginally above 1.0 from f32 rounding must not error.
        .map(|n| (n.item.id.clone(), Score::clamped(n.relevance)))
        .collect();
    // Contract (`RedundancyAssessment::near_duplicates`'s own doc):
    // "Descending by similarity, ties broken by ascending `ItemId`... anchor
    // to this rule, not to 'whatever order the backend happened to return
    // neighbours in'." `sort_by_key` is stable, so the `ItemId` must be part
    // of the key itself — relying on input order to break a tie would anchor
    // to the backend's order, exactly what the contract forbids.
    near.sort_by_key(|n| (std::cmp::Reverse(n.1), n.0.clone()));

    let best = neighbours
        .iter()
        .map(|n| n.relevance)
        .fold(0.0f32, f32::max);
    RedundancyAssessment {
        score: Score::clamped(best),
        near_duplicates: near,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BaselineConfig;
    use crate::testkit::candidate;
    use memorysafe_core::ItemId;

    #[test]
    fn no_neighbours_means_no_redundancy() {
        let a = assess(&[], &BaselineConfig::default());
        assert_eq!(a.score, Score::ZERO);
        assert!(a.near_duplicates.is_empty());
    }

    #[test]
    fn redundancy_is_the_best_neighbour_similarity() {
        let cfg = BaselineConfig::default();
        let a = assess(
            &[
                candidate("a", 0.42),
                candidate("b", 0.81),
                candidate("c", 0.10),
            ],
            &cfg,
        );
        assert!((a.score.get() - 0.81).abs() < 1e-6);
    }

    #[test]
    fn near_duplicates_come_back_sorted_descending() {
        let cfg = BaselineConfig::default();
        let a = assess(&[candidate("a", 0.42), candidate("b", 0.81)], &cfg);
        assert_eq!(a.near_duplicates.len(), 2);
        assert!(a.near_duplicates[0].1 >= a.near_duplicates[1].1);
        assert!((a.near_duplicates[0].1.get() - 0.81).abs() < 1e-6);
    }

    #[test]
    fn only_neighbours_above_the_floor_are_listed() {
        let cfg = BaselineConfig {
            near_duplicate_floor: 0.5,
            ..Default::default()
        };
        let a = assess(&[candidate("a", 0.81), candidate("b", 0.10)], &cfg);
        assert_eq!(
            a.near_duplicates.len(),
            1,
            "the 0.10 neighbour is not near-duplicate"
        );
    }

    #[test]
    fn ties_break_on_ascending_item_id_so_best_is_deterministic() {
        // Contract (`RedundancyAssessment::near_duplicates`'s own doc):
        // "Descending by similarity, ties broken by ascending `ItemId`" —
        // this is what makes `best()` a deterministic merge target
        // regardless of the order the backend happened to return neighbours
        // in. The LARGER id is placed FIRST in the input, with an identical
        // score: a stable sort that merely preserves input order on a tie
        // would then report the wrong (larger-id-first) answer, so this
        // fixture actually discriminates "reads ItemId" from "reads nothing,
        // relies on stability" rather than passing either way.
        let cfg = BaselineConfig::default();
        let small_id = ItemId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap();
        let large_id = ItemId::parse("01BX5ZZKBKACTAV9WEVGEMMVRZ").unwrap();

        let mut arrives_first = candidate("a", 0.5);
        arrives_first.item.id = large_id.clone();
        let mut arrives_second = candidate("b", 0.5);
        arrives_second.item.id = small_id.clone();

        let a = assess(&[arrives_first, arrives_second], &cfg);

        assert_eq!(a.near_duplicates.len(), 2);
        assert_eq!(
            a.near_duplicates[0].0, small_id,
            "a tie must break on ascending ItemId, not input order"
        );
        assert_eq!(a.near_duplicates[1].0, large_id);
        assert_eq!(
            a.best().unwrap().0,
            &small_id,
            "best() must name the ascending-ItemId winner on a tie"
        );
    }

    #[test]
    fn classification_matches_the_documented_thresholds() {
        let cfg = BaselineConfig::default();
        assert_eq!(cfg.classify(0.99), Verdict::NearDuplicate);
        assert_eq!(cfg.classify(0.95), Verdict::Mergeable);
        assert_eq!(cfg.classify(0.50), Verdict::Novel);
        // Boundaries are inclusive at the threshold.
        assert_eq!(
            cfg.classify(cfg.duplicate_threshold),
            Verdict::NearDuplicate
        );
        assert_eq!(cfg.classify(cfg.merge_threshold), Verdict::Mergeable);
    }
}
