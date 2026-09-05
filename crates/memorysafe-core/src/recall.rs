use crate::assessment::SensitivityLevel;
use crate::decision::Reason;
use crate::ids::{AuditId, ItemId, Scope};
use crate::item::MemoryItem;
use crate::score::Score;
use serde::{Deserialize, Serialize};

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
    pub mode: RecallMode,
    pub budget: RecallBudget,
    /// The caller's clearance. Items above this level are excluded in SQL,
    /// below the policy.
    pub sensitivity_ceiling: SensitivityLevel,
}

/// A retrieval hit before composition. `relevance` fuses vector and keyword.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredCandidate {
    pub item: MemoryItem,
    pub relevance: f32,
    pub vector_score: Option<f32>,
    pub keyword_score: Option<f32>,
    pub value: Score,
    pub fragility: Score,
    pub estimated_tokens: u32,
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
    /// Truncated to `OMITTED_CAP`.
    pub omitted: Vec<OmittedItem>,
    /// Set by the engine after `record_recall`; the policy leaves it `None`.
    pub audit_id: Option<AuditId>,
}

impl WorkingSet {
    pub fn empty() -> Self {
        Self {
            items: vec![],
            tokens_used: 0,
            omitted: vec![],
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
