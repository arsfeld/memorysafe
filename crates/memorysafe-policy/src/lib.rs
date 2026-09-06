//! `BaselinePolicy` — the open-source governance policy.
//!
//! Simple and documented, not deliberately crippled. The proprietary policy
//! earns its keep with learned scorers and cross-tenant calibration, not by
//! this one being bad.

pub mod config;
pub mod fragility;
pub mod redundancy;
pub mod sensitivity;
pub mod value;

#[cfg(test)]
pub(crate) mod testkit;

pub use config::{BaselineConfig, Verdict};

use memorysafe_core::{
    AdmitContext, AssessContext, Assessed, Assessment, AssessorId, Candidate, ComposeContext,
    Decision, GovernancePolicy, MaintainContext, PolicyError, PolicyId, RecallRequest,
    ScoredCandidate, WorkingSet, features,
};

pub const BASELINE_VERSION: &str = "0.1.0";

pub struct BaselinePolicy {
    pub config: BaselineConfig,
}

impl BaselinePolicy {
    pub fn new(config: BaselineConfig) -> Self {
        Self { config }
    }

    pub fn policy_id(&self) -> PolicyId {
        PolicyId::new("baseline", BASELINE_VERSION)
    }
}

impl Default for BaselinePolicy {
    fn default() -> Self {
        Self::new(BaselineConfig::default())
    }
}

impl GovernancePolicy for BaselinePolicy {
    fn id(&self) -> PolicyId {
        self.policy_id()
    }

    fn assess(&self, cand: &Candidate, ctx: &AssessContext) -> Result<Assessment, PolicyError> {
        let redundancy = redundancy::assess(&ctx.neighbours, &self.config);
        let fragility = fragility::score(&ctx.neighbours, &ctx.stats);
        let value = value::score(cand, &ctx.stats, &self.config);
        let sensitivity = sensitivity::assess(cand);

        Ok(Assessment {
            features: features! {
                "neighbour_count" => ctx.neighbours.len() as f64,
                "best_similarity" => redundancy.score.get(),
                "corpus_mean_similarity" => ctx.stats.mean_neighbour_similarity,
                "corpus_item_count" => ctx.stats.item_count as f64,
                "byte_size" => cand.byte_size as f64,
                "has_embedding" => if cand.embedding.is_some() { 1.0 } else { 0.0 },
            },
            value,
            fragility,
            sensitivity,
            redundancy,
            assessor: AssessorId::new("baseline", BASELINE_VERSION),
        })
    }

    // Implemented in Tasks 27-29.
    fn admit(&self, _assessed: &Assessed, _ctx: &AdmitContext) -> Result<Decision, PolicyError> {
        unimplemented!("Task 27")
    }
    fn compose(
        &self,
        _req: &RecallRequest,
        _candidates: &[ScoredCandidate],
        _ctx: &ComposeContext,
    ) -> Result<WorkingSet, PolicyError> {
        unimplemented!("Task 28")
    }
    fn maintain(&self, _ctx: &MaintainContext) -> Result<Vec<Decision>, PolicyError> {
        unimplemented!("Task 29")
    }
}
