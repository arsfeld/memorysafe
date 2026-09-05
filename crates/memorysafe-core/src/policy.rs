use crate::assessment::{Assessment, SensitivityLevel};
use crate::capacity::{CapacityState, ScopeStats};
use crate::decision::{Decision, PolicyId};
use crate::embedding::Embedding;
use crate::ids::Scope;
use crate::item::MemoryItem;
use crate::recall::{RecallRequest, ScoredCandidate, WorkingSet};
use crate::score::Score;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use time::OffsetDateTime;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("policy misconfigured: {0}")]
    Config(String),
    #[error("policy could not score candidate: {0}")]
    Scoring(String),
    #[error("policy requires an embedding but none was supplied")]
    MissingEmbedding,
}

/// A write candidate before it becomes a `MemoryItem`. Carries no id, no
/// timestamps, and no protection — those are engine-assigned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub body: String,
    pub kind: String,
    pub tags: Vec<String>,
    pub attrs: BTreeMap<String, serde_json::Value>,
    pub sensitivity_hint: Option<SensitivityLevel>,
    /// `None` when the embedder was unavailable. `assess` must still succeed.
    pub embedding: Option<Embedding>,
    pub byte_size: u64,
}

/// A candidate paired with the assessment `assess` produced for it.
///
/// Borrows rather than owns, and deliberately does NOT derive `Clone`: a
/// derived `Clone` here would copy the references, not the values, which is
/// not what a reader expects from every other `Clone` in this module. The
/// engine clones the owned `Candidate` and `Assessment` and rebuilds this
/// struct inside its `catch_unwind` boundary, so nothing needs to clone the
/// pair itself.
#[derive(Debug, PartialEq)]
pub struct Assessed<'a> {
    pub candidate: &'a Candidate,
    pub assessment: &'a Assessment,
}

/// Everything `assess` may look at. Plain data; no handles, no closures.
///
/// Serialisable so a production context can be captured to a file and replayed
/// as a fixture against a new policy version — one of the reasons this trait is
/// pure. Every field already derives serde; the context did not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssessContext {
    pub scope: Scope,
    /// Nearest neighbours in the scope, descending by similarity. Empty when
    /// the candidate has no embedding.
    pub neighbours: Vec<ScoredCandidate>,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdmitContext {
    pub scope: Scope,
    pub capacity: CapacityState,
    /// Items the engine offers as evictable, cheapest-to-lose first. Already
    /// excludes pinned items and unexpired protection windows.
    ///
    /// `MaintenanceCandidate`, not `ScoredCandidate`, for the same reason
    /// `maintain` uses it: there is no recall query when the engine offers
    /// eviction candidates, so `relevance` could only be zero and a policy
    /// sorting by it would evict in arbitrary backend order.
    pub eviction_candidates: Vec<MaintenanceCandidate>,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComposeContext {
    pub scope: Scope,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

/// An item offered to `maintain`, carrying the same `value`/`fragility` signal
/// `AdmitContext::eviction_candidates` carries — capacity reclaim during
/// maintenance is the same decision `admit` makes under pressure, and a policy
/// handed bare `MemoryItem`s could only sort by age.
///
/// Deliberately NOT `ScoredCandidate`: that type carries `relevance`,
/// `vector_score` and `keyword_score`, which are relative to a recall query.
/// There is no query during maintenance, so the engine could only set them to
/// zero — and a policy that sorted by `relevance` would silently rank every
/// item identically. Omitting the fields makes that mistake unrepresentable
/// rather than merely documented.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaintenanceCandidate {
    pub item: MemoryItem,
    pub value: Score,
    pub fragility: Score,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaintainContext {
    pub scope: Scope,
    /// One page of the scope's items. Maintenance is a resumable job; the
    /// engine pages through and calls `maintain` per batch.
    pub batch: Vec<MaintenanceCandidate>,
    /// True when this page is the last one. Derived from the same fact the
    /// engine uses to stop paging (a short page means the end), NOT from
    /// arithmetic over the capacity count — an item forgotten mid-run would
    /// make such a count disagree with the paging position, and a policy would
    /// evict on the strength of a number that had quietly floored to zero.
    pub is_final_batch: bool,
    pub capacity: CapacityState,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

/// The seam. Pure: the engine performs all I/O and hands in everything above.
///
/// `Err` from any method means the CALL failed and the engine should fall back
/// to a baseline policy. It does not mean "skip this item": `compose` reports
/// per-candidate exclusions through `WorkingSet::omitted`, each with a reason,
/// and `maintain` simply returns no `Decision` for an item it declines to act
/// on. A policy that returns `Err` because one candidate of two hundred was
/// unscoreable would discard the other hundred and ninety-nine.
pub trait GovernancePolicy: Send + Sync {
    fn id(&self) -> PolicyId;

    fn assess(&self, cand: &Candidate, ctx: &AssessContext) -> Result<Assessment, PolicyError>;

    fn admit(&self, assessed: &Assessed, ctx: &AdmitContext) -> Result<Decision, PolicyError>;

    fn compose(
        &self,
        req: &RecallRequest,
        candidates: &[ScoredCandidate],
        ctx: &ComposeContext,
    ) -> Result<WorkingSet, PolicyError>;

    fn maintain(&self, ctx: &MaintainContext) -> Result<Vec<Decision>, PolicyError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assessment::{AssessorId, RedundancyAssessment, SensitivityAssessment};
    use crate::decision::{Reason, ReasonCode};
    use crate::features;
    use crate::ids::ItemId;
    use crate::item::{MemoryItem, Protection, Source, SourceKind};
    use crate::recall::{RecallBudget, RecallMode};
    use crate::score::Score;
    use crate::{Budget, CapacityState, Scope, ScopeStats};
    use time::OffsetDateTime;

    /// A policy that admits everything. Proves the trait is implementable with
    /// no I/O whatsoever.
    struct AlwaysAdmit;

    impl GovernancePolicy for AlwaysAdmit {
        fn id(&self) -> PolicyId {
            PolicyId::new("always-admit", "0.0.0")
        }
        fn assess(&self, _c: &Candidate, _ctx: &AssessContext) -> Result<Assessment, PolicyError> {
            Ok(Assessment {
                value: Score::ONE,
                fragility: Score::ZERO,
                sensitivity: SensitivityAssessment {
                    level: SensitivityLevel::Public,
                    categories: vec![],
                    confidence: Score::ONE,
                },
                redundancy: RedundancyAssessment {
                    score: Score::ZERO,
                    near_duplicates: vec![],
                },
                features: features! {},
                assessor: AssessorId::new("always-admit", "0.0.0"),
            })
        }
        fn admit(&self, _a: &Assessed, _ctx: &AdmitContext) -> Result<Decision, PolicyError> {
            Ok(Decision::retain(
                self.id(),
                Reason::new(ReasonCode::NovelContent, "always", features! {}),
            ))
        }
        fn compose(
            &self,
            _r: &RecallRequest,
            _c: &[ScoredCandidate],
            _ctx: &ComposeContext,
        ) -> Result<WorkingSet, PolicyError> {
            Ok(WorkingSet::empty())
        }
        fn maintain(&self, _ctx: &MaintainContext) -> Result<Vec<Decision>, PolicyError> {
            Ok(vec![])
        }
    }

    fn assess_ctx() -> AssessContext {
        AssessContext {
            scope: Scope::new("t", "s", "n").unwrap(),
            neighbours: vec![],
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn sample_item() -> MemoryItem {
        MemoryItem {
            id: ItemId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap(),
            scope: Scope::new("t", "s", "n").unwrap(),
            body: "sample".into(),
            kind: "test".into(),
            tags: vec![],
            attrs: Default::default(),
            sensitivity: SensitivityLevel::Public,
            occurred_at: None,
            created_at: OffsetDateTime::from_unix_timestamp(0).unwrap(),
            protection: Protection::Normal,
            ttl: None,
            pending_embedding: false,
            source: Source {
                kind: SourceKind::Agent,
                id: Some("test".into()),
            },
        }
    }

    #[test]
    fn the_trait_is_implementable_without_io() {
        let p = AlwaysAdmit;
        let c = Candidate {
            body: "hello".into(),
            kind: "fact".into(),
            tags: vec![],
            attrs: Default::default(),
            sensitivity_hint: None,
            embedding: None,
            byte_size: 5,
        };
        let a = p.assess(&c, &assess_ctx()).unwrap();
        assert_eq!(a.value, Score::ONE);
        assert_eq!(p.id().to_string(), "always-admit@0.0.0");
    }

    #[test]
    fn policies_are_object_safe_so_the_engine_can_hold_a_boxed_one() {
        let boxed: Box<dyn GovernancePolicy> = Box::new(AlwaysAdmit);
        assert_eq!(boxed.id().name, "always-admit");
    }

    #[test]
    fn admit_context_exposes_capacity_without_exposing_the_backend() {
        let ctx = AdmitContext {
            scope: Scope::new("t", "s", "n").unwrap(),
            capacity: CapacityState {
                budget: Budget {
                    max_items: Some(10),
                    max_bytes: None,
                },
                used_items: 10,
                used_bytes: 0,
            },
            eviction_candidates: vec![],
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        };
        assert_eq!(ctx.capacity.pressure(), 1.0);
    }

    #[test]
    fn contexts_can_be_cloned_and_compared() {
        let ctx = AssessContext {
            scope: Scope::new("t", "s", "n").unwrap(),
            neighbours: vec![],
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        };
        let cloned = ctx.clone();
        assert_eq!(ctx, cloned);
    }

    #[test]
    fn compose_and_maintain_are_reachable_through_the_trait() {
        // Half the trait's surface had no test and neither of these two context
        // types was ever constructed, so a wrong field or a non-Clone field
        // would have been caught only by the signature still compiling.
        let p: Box<dyn GovernancePolicy> = Box::new(AlwaysAdmit);

        let req = RecallRequest {
            scope: Scope::new("t", "s", "n").unwrap(),
            query: Some("anything".into()),
            tags_any: vec![],
            kinds: vec![],
            occurred_after: None,
            occurred_before: None,
            mode: RecallMode::WorkingSet,
            budget: RecallBudget::default(),
            sensitivity_ceiling: SensitivityLevel::Internal,
        };
        let compose_ctx = ComposeContext {
            scope: Scope::new("t", "s", "n").unwrap(),
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        };

        // Non-empty input: an empty slice proves only that the method returns
        // Ok, not that it can read its own parameters.
        let candidate = ScoredCandidate {
            item: sample_item(),
            relevance: 0.75,
            vector_score: Some(0.75),
            keyword_score: None,
            value: Score::clamped(0.6),
            fragility: Score::clamped(0.2),
            estimated_tokens: 12,
        };
        let ws = p
            .compose(&req, std::slice::from_ref(&candidate), &compose_ctx)
            .unwrap();
        assert!(
            ws.items.is_empty(),
            "AlwaysAdmit composes nothing by design"
        );
        assert_eq!(compose_ctx.clone(), compose_ctx);

        let maintain_ctx = MaintainContext {
            scope: Scope::new("t", "s", "n").unwrap(),
            batch: vec![MaintenanceCandidate {
                item: sample_item(),
                value: Score::clamped(0.3),
                fragility: Score::clamped(0.9),
            }],
            // Not the last page — a policy that only ever sees `true` here is
            // never tested on the partial-view case it must handle.
            is_final_batch: false,
            capacity: CapacityState {
                budget: Budget::UNBOUNDED,
                used_items: 0,
                used_bytes: 0,
            },
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        };
        assert!(p.maintain(&maintain_ctx).unwrap().is_empty());
        assert_eq!(maintain_ctx.clone(), maintain_ctx);
        assert_eq!(maintain_ctx.batch[0].fragility.get(), 0.9);
        assert!(!maintain_ctx.is_final_batch);
    }
}
