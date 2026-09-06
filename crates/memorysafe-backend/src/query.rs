use memorysafe_core::Embedding;
use memorysafe_core::SensitivityLevel;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub const MAX_PAGE_LIMIT: usize = 1000;

/// Filters that MUST be applied inside the backend's own query. A policy may
/// narrow a candidate set further but may never widen it, so anything
/// security-relevant belongs here rather than in `compose`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardFilters {
    pub tags_any: Vec<String>,
    pub kinds: Vec<String>,
    /// Bound results by when the remembered thing happened.
    ///
    /// **Both bounds are inclusive**: an item whose `occurred_at` falls exactly
    /// on either edge matches. That is `>=` and `<=` in SQL, and it matches
    /// every other range bound in this workspace — `Backend::audit`'s
    /// `since`/`until`, `AuditAggregateFilter`'s day window, `Protection`'s
    /// expiry boundary, and `RecallBudget`'s limits all say so explicitly.
    /// These two were the only range bounds that did not, which made them the
    /// odd pair out of a settled convention rather than an open question.
    ///
    /// **An item whose `occurred_at` is `None` matches neither bound.** Set
    /// either field and every item with no occurrence time is excluded. That
    /// is the intended reading — a filter asking what happened after a date
    /// cannot be satisfied by an item that does not say when it happened, and
    /// it fails closed, like `sensitivity_ceiling` above it.
    ///
    /// It is stated because it is otherwise an accident rather than a choice.
    /// `occurred_at` is nullable, `NULL >= x` is `NULL`, and SQL drops the
    /// row — so a backend gets this behaviour by writing the obvious
    /// predicate and never deciding anything. And it is invisible in testing
    /// by default: `conformance::fx::item` pins `occurred_at: None`, so under
    /// any time filter the entire standard fixture corpus disappears, and a
    /// test asserting an empty result would pass without the filter working
    /// at all. A test of these fields must set `occurred_at` explicitly on
    /// every item it expects back, and must include one item with `None` to
    /// pin the exclusion.
    #[serde(with = "time::serde::timestamp::option")]
    pub occurred_after: Option<OffsetDateTime>,
    #[serde(with = "time::serde::timestamp::option")]
    pub occurred_before: Option<OffsetDateTime>,
    /// Items strictly above this level are excluded in SQL.
    pub sensitivity_ceiling: SensitivityLevel,
    /// Exclude items whose embedding has not been backfilled yet.
    pub exclude_pending_embedding: bool,
}

impl Default for HardFilters {
    fn default() -> Self {
        Self {
            tags_any: vec![],
            kinds: vec![],
            occurred_after: None,
            occurred_before: None,
            // Fail closed.
            sensitivity_ceiling: SensitivityLevel::Internal,
            exclude_pending_embedding: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateQuery {
    pub embedding: Option<Embedding>,
    pub text: Option<String>,
    pub filters: HardFilters,
    /// Over-fetch limit. The engine typically sets 5–10x the recall budget.
    pub limit: usize,
}

impl CandidateQuery {
    pub fn is_valid(&self) -> bool {
        self.embedding.is_some() || self.text.as_ref().is_some_and(|t| !t.trim().is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub offset: usize,
    pub limit: usize,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: 50,
        }
    }
}

impl Page {
    pub fn effective_limit(&self) -> usize {
        self.limit.min(MAX_PAGE_LIMIT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::SensitivityLevel;

    #[test]
    fn default_filters_admit_nothing_above_internal() {
        // Fail closed: a caller that forgets to set a ceiling gets the
        // conservative one, not Restricted.
        let f = HardFilters::default();
        assert_eq!(f.sensitivity_ceiling, SensitivityLevel::Internal);
        assert!(f.tags_any.is_empty());
        assert!(f.kinds.is_empty());
    }

    #[test]
    fn a_query_must_carry_a_vector_or_text_or_both() {
        let empty = CandidateQuery {
            embedding: None,
            text: None,
            filters: HardFilters::default(),
            limit: 10,
        };
        assert!(!empty.is_valid());

        let text_only = CandidateQuery {
            embedding: None,
            text: Some("cats".into()),
            filters: HardFilters::default(),
            limit: 10,
        };
        assert!(text_only.is_valid());

        // Whitespace-only text carries no query signal either; it must not
        // be treated as "text was supplied".
        let whitespace_only = CandidateQuery {
            embedding: None,
            text: Some("   ".into()),
            filters: HardFilters::default(),
            limit: 10,
        };
        assert!(!whitespace_only.is_valid());
    }

    #[test]
    fn pages_clamp_to_a_sane_maximum() {
        assert_eq!(
            Page {
                offset: 0,
                limit: 100_000
            }
            .effective_limit(),
            MAX_PAGE_LIMIT
        );
        assert_eq!(
            Page {
                offset: 0,
                limit: 25
            }
            .effective_limit(),
            25
        );
        assert_eq!(Page::default().limit, 50);
    }
}
