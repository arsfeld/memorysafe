use crate::assessment::{Assessment, SensitivityLevel};
use crate::capacity::{CapacityState, ScopeStats};
use crate::decision::{Decision, PolicyId};
use crate::embedding::Embedding;
use crate::ids::Scope;
use crate::item::MemoryItem;
use crate::recall::{RecallRequest, ScoredCandidate, WorkingSet};
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
#[derive(Debug, Clone, PartialEq)]
pub struct Assessed<'a> {
    pub candidate: &'a Candidate,
    pub assessment: &'a Assessment,
}

/// Everything `assess` may look at. Plain data; no handles, no closures.
#[derive(Debug, Clone, PartialEq)]
pub struct AssessContext {
    pub scope: Scope,
    /// Nearest neighbours in the scope, descending by similarity. Empty when
    /// the candidate has no embedding.
    pub neighbours: Vec<ScoredCandidate>,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdmitContext {
    pub scope: Scope,
    pub capacity: CapacityState,
    /// Items the engine offers as evictable, cheapest-to-lose first. Already
    /// excludes pinned items and unexpired protection windows.
    pub eviction_candidates: Vec<ScoredCandidate>,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComposeContext {
    pub scope: Scope,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MaintainContext {
    pub scope: Scope,
    /// One page of the scope's items. Maintenance is a resumable job; the
    /// engine pages through and calls `maintain` per batch.
    pub batch: Vec<MemoryItem>,
    pub capacity: CapacityState,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

/// The seam. Pure: the engine performs all I/O and hands in everything above.
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
}
