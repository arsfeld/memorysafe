//! Orchestration. The engine performs all I/O, hands pure data to the policy,
//! validates everything the policy returns, and applies writes atomically with
//! an audit record.

pub mod error;
pub mod gather;
pub mod outcome;
pub mod read;
pub mod validate;
pub mod write;

pub use error::EngineError;
pub use outcome::{ForgetOutcome, PurgeOutcome, WriteOutcome};
pub use validate::FailureStance;
pub use write::RememberRequest;

use memorysafe_backend::{Backend, Page};
use memorysafe_core::{AuditFilter, AuditRecord, Budget, GovernancePolicy, MemoryItem, Scope};
use memorysafe_embed::Embedder;
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

pub struct EngineConfig {
    pub backend: Arc<dyn Backend>,
    pub embedder: Arc<dyn Embedder>,
    pub policy: Arc<dyn GovernancePolicy>,
    /// Used when `policy` misbehaves and the stance is `FailSafe`.
    pub fallback_policy: Arc<dyn GovernancePolicy>,
    pub stance: FailureStance,
    /// How many neighbours to fetch for assessment.
    pub neighbour_k: usize,
    /// How many items to offer the policy as eviction candidates.
    pub eviction_candidates: usize,
}

impl EngineConfig {
    pub fn new(
        backend: Arc<dyn Backend>,
        embedder: Arc<dyn Embedder>,
        policy: Arc<dyn GovernancePolicy>,
    ) -> Self {
        Self {
            backend,
            embedder,
            policy,
            fallback_policy: Arc::new(BaselinePolicy::default()),
            stance: FailureStance::FailSafe,
            neighbour_k: 16,
            eviction_candidates: 128,
        }
    }
}

pub struct Engine {
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) embedder: Arc<dyn Embedder>,
    pub(crate) policy: Arc<dyn GovernancePolicy>,
    pub(crate) fallback_policy: Arc<dyn GovernancePolicy>,
    pub(crate) stance: FailureStance,
    pub(crate) neighbour_k: usize,
    pub(crate) eviction_candidates: usize,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        Self {
            backend: config.backend,
            embedder: config.embedder,
            policy: config.policy,
            fallback_policy: config.fallback_policy,
            stance: config.stance,
            neighbour_k: config.neighbour_k,
            eviction_candidates: config.eviction_candidates,
        }
    }

    pub async fn review(&self, scope: &Scope, page: &Page) -> Result<Vec<MemoryItem>, EngineError> {
        Ok(self.backend.list(scope, page).await?)
    }

    pub async fn audit(
        &self,
        scope: &Scope,
        filter: &AuditFilter,
    ) -> Result<Vec<AuditRecord>, EngineError> {
        Ok(self.backend.audit(scope, filter).await?)
    }

    pub async fn set_budget(&self, scope: &Scope, budget: Budget) -> Result<(), EngineError> {
        Ok(self.backend.set_budget(scope, budget).await?)
    }
}
