use crate::assessment::SensitivityLevel;
use crate::decision::Reason;
use crate::ids::{AuditId, ItemId, Scope};
use crate::item::MemoryItem;
use crate::score::Score;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Cap on `WorkingSet::omitted`. A recall over a large corpus considers many
/// candidates; the response must stay bounded.
pub const OMITTED_CAP: usize = 50;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallMode {
    /// Governed composition: relevance, value, fragility, replay, diversity.
    #[default]
    WorkingSet,
    /// Raw ranked list. Still scope-filtered, still sensitivity-capped, still
    /// audited — it bypasses composition, never governance.
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallBudget {
    pub max_tokens: Option<u32>,
    pub max_items: Option<usize>,
}

impl Default for RecallBudget {
    fn default() -> Self {
        Self {
            max_tokens: Some(2000),
            max_items: Some(20),
        }
    }
}

impl RecallBudget {
    pub fn fits(&self, tokens: u32, items: usize) -> bool {
        self.max_tokens.is_none_or(|m| tokens <= m) && self.max_items.is_none_or(|m| items <= m)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallRequest {
    pub scope: Scope,
    pub query: Option<String>,
    pub tags_any: Vec<String>,
    pub kinds: Vec<String>,
    /// Bound recall to when the remembered thing happened. Both are pushed
    /// into SQL as hard filters, below the policy, like every other filter
    /// here — without them the backend's time-filter machinery would exist
    /// but be unreachable from any caller.
    pub occurred_after: Option<OffsetDateTime>,
    pub occurred_before: Option<OffsetDateTime>,
    pub mode: RecallMode,
    pub budget: RecallBudget,
    /// The caller's clearance. Items above this level are excluded in SQL,
    /// below the policy.
    pub sensitivity_ceiling: SensitivityLevel,
}

/// A retrieval hit before composition. `relevance` fuses vector and keyword.
///
/// `relevance`, `vector_score` and `keyword_score` are bare `f32`, deliberately
/// breaking the crate's "never a bare `f32`" rule. `Score` is `[0,1]`; cosine
/// spans `[-1,1]` and BM25 is unbounded, so wrapping these would make
/// `Score::new` reject valid retrieval signal and `Score::clamped` destroy the
/// magnitude a ranker needs. Contrast `RedundancyAssessment::near_duplicates`,
/// which *can* use `Score` only because it is pre-filtered above a floor.
/// `value` and `fragility` are genuine governance scores and stay `Score`.
///
/// Because these are `f32`, any sort on them must use `total_cmp` — a
/// degenerate zero-vector cosine can produce NaN.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredCandidate {
    pub item: MemoryItem,
    pub relevance: f32,
    pub vector_score: Option<f32>,
    pub keyword_score: Option<f32>,
    pub value: Score,
    pub fragility: Score,
    pub estimated_tokens: u32,
    /// When this item was last recalled. `Backend::retrieve_candidates` and
    /// `Backend::neighbours` populate it; `Backend::record_recall` advances it.
    ///
    /// **A never-recalled item is `None`, never `Some(created_at)`.** The two
    /// must stay distinguishable: `value` weighs recency and `fragility`
    /// weighs access-recovery cost, and `compose`'s replay quota is reserved
    /// for high-fragility *or long-unaccessed* items — a feature that exists
    /// to resurface what is never recalled. Defaulting to `created_at` makes
    /// an old item recalled yesterday look identical to one never recalled at
    /// all, which is precisely backwards. It would also be undetectable in
    /// this workspace's own tests: `fx::item` pins `created_at` to
    /// `UNIX_EPOCH`, so in every conformance fixture `Some(created_at)` and
    /// "never accessed" would be the same value.
    ///
    /// These two fields live here and on `MaintenanceCandidate`, and
    /// deliberately **not** on `MemoryItem`: that type is serialised into
    /// exports and digested into identity records, so a per-read counter on it
    /// would change an item's serialisation on every read. The ranking structs
    /// are ephemeral — never digested, never exported.
    #[serde(with = "time::serde::timestamp::option")]
    pub last_accessed_at: Option<OffsetDateTime>,
    /// How many times this item has been recalled. `0` for an item never
    /// recalled, which pairs with `last_accessed_at: None`.
    pub access_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectedItem {
    pub item: MemoryItem,
    pub relevance: f32,
    pub reason: Reason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OmittedItem {
    pub id: ItemId,
    pub reason: Reason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkingSet {
    pub items: Vec<SelectedItem>,
    pub tokens_used: u32,
    /// A **sample** of what was considered and cut, truncated to
    /// `OMITTED_CAP`. Read `omitted_total`, never `omitted.len()`, for how
    /// many items were actually omitted.
    pub omitted: Vec<OmittedItem>,
    /// How many items were omitted **before** the sample above was truncated.
    ///
    /// `omitted.len()` is the size of the reported sample and is bounded by
    /// `OMITTED_CAP`; this is the true count and is not. The two are equal
    /// exactly when the count is at or below `OMITTED_CAP`, and differ
    /// whenever it exceeds it — which is precisely the case a caller needs to
    /// detect and the one `omitted.len()` cannot report, since it reads 50
    /// for 50 omissions and for 5000 alike.
    ///
    /// The distinction is not cosmetic: it is the difference between "your
    /// budget cut a couple of near-misses" and "your budget discarded most of
    /// the corpus", and a caller tuning `RecallBudget` is asking exactly that
    /// question. Truncation without a count is a silent loss of the number
    /// that would have answered it.
    pub omitted_total: usize,
    /// Set by the engine after `record_recall`; the policy leaves it `None`.
    /// `None` means "not yet audited", and the type cannot distinguish that
    /// from "audited" — so nothing stops an unaudited working set reaching a
    /// caller. "Every recall is audited" is a product claim, so the engine
    /// asserts this is `Some` at its public boundary (see the read-path task).
    pub audit_id: Option<AuditId>,
}

impl WorkingSet {
    pub fn empty() -> Self {
        Self {
            items: vec![],
            tokens_used: 0,
            omitted: vec![],
            omitted_total: 0,
            audit_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recall_defaults_to_the_governed_working_set() {
        assert_eq!(RecallMode::default(), RecallMode::WorkingSet);
    }

    #[test]
    fn budget_tracks_both_tokens_and_items() {
        let b = RecallBudget {
            max_tokens: Some(2000),
            max_items: Some(10),
        };
        assert!(b.fits(1999, 9));
        assert!(!b.fits(2001, 9));
        assert!(!b.fits(1999, 11));
    }

    #[test]
    fn an_unbounded_budget_fits_anything() {
        let b = RecallBudget {
            max_tokens: None,
            max_items: None,
        };
        assert!(b.fits(u32::MAX, usize::MAX));
    }

    #[test]
    fn omitted_list_is_capped_so_a_wide_recall_cannot_blow_up_the_response() {
        assert_eq!(OMITTED_CAP, 50);
    }

    /// The truncated sample and the true count are separate fields, and the
    /// separation survives serialisation.
    ///
    /// **The implementation this rejects:** one that keeps only
    /// `omitted: Vec<OmittedItem>` and lets a caller read `omitted.len()` as
    /// "how many were cut". Above `OMITTED_CAP` that number is a constant —
    /// 50 omitted and 5000 omitted are the same observation — so a caller
    /// deciding "was my budget far too small?" cannot tell a near-miss from a
    /// catastrophe. The serde half rejects a second, quieter variant: a
    /// `#[serde(skip)]` on the new field, which keeps the distinction inside
    /// the process and drops it at exactly the boundary — the MCP and HTTP
    /// responses — where the caller who needs it lives.
    ///
    /// **Vacuous if** the fixture's `omitted_total` is ever set to
    /// `omitted.len()`, or if the sample is built shorter than `OMITTED_CAP`:
    /// either makes the two fields agree, and a type that discarded the count
    /// and recomputed it from the sample would pass. The construction below
    /// therefore fills the sample to exactly `OMITTED_CAP` and sets the total
    /// two orders of magnitude above it, and asserts the two differ.
    #[test]
    fn the_omitted_sample_and_the_omitted_total_are_different_numbers() {
        use crate::decision::{Reason, ReasonCode};
        use crate::features;

        let considered = 5_000;
        let omitted: Vec<OmittedItem> = (0..OMITTED_CAP)
            .map(|_| OmittedItem {
                id: ItemId::new(),
                reason: Reason::new(
                    ReasonCode::BudgetExhausted,
                    "considered but did not fit the budget",
                    features! {},
                ),
            })
            .collect();
        let ws = WorkingSet {
            items: vec![],
            tokens_used: 0,
            omitted,
            omitted_total: considered,
            audit_id: None,
        };

        assert_eq!(
            ws.omitted.len(),
            OMITTED_CAP,
            "the reported sample is bounded by OMITTED_CAP"
        );
        assert_eq!(
            ws.omitted_total, considered,
            "the total is the number considered and cut, not the sample size"
        );
        assert_ne!(
            ws.omitted.len(),
            ws.omitted_total,
            "omitted.len() must not be readable as the omission count: that is \
             the whole reason omitted_total exists"
        );

        // The distinction has to reach a caller, not just exist in memory.
        let json = serde_json::to_string(&ws).unwrap();
        let back: WorkingSet = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.omitted_total, considered,
            "omitted_total did not survive serialisation"
        );
        assert_eq!(back.omitted.len(), OMITTED_CAP);

        // And an empty working set claims no omissions at all — the one case
        // where the two numbers legitimately agree.
        let empty = WorkingSet::empty();
        assert_eq!(empty.omitted_total, 0);
        assert!(empty.omitted.is_empty());
    }

    #[test]
    fn a_budget_is_inclusive_at_its_exact_limit() {
        // `<=`, not `<`. With `<` the last slot of every budget is unusable:
        // `compose` packs while `fits(tokens + next, len + 1)` holds, so an item
        // that exactly fills the budget would be silently dropped and the caller
        // would get fewer memories than they asked for, with no error.
        let b = RecallBudget {
            max_tokens: Some(2000),
            max_items: Some(10),
        };
        assert!(b.fits(2000, 10), "a budget must include its own limit");
        assert!(!b.fits(2001, 10));
        assert!(!b.fits(2000, 11));

        // And each limit is inclusive independently.
        let tokens_only = RecallBudget {
            max_tokens: Some(100),
            max_items: None,
        };
        assert!(tokens_only.fits(100, usize::MAX));
        assert!(!tokens_only.fits(101, 0));
        let items_only = RecallBudget {
            max_tokens: None,
            max_items: Some(3),
        };
        assert!(items_only.fits(u32::MAX, 3));
        assert!(!items_only.fits(0, 4));
    }
}
