//! Orchestration. The engine performs all I/O, hands pure data to the policy,
//! validates everything the policy returns, and applies writes atomically with
//! an audit record.

pub mod cache;
pub mod error;
pub mod gather;
pub mod maintain;
pub mod mutate;
pub mod outcome;
pub mod portability;
pub mod read;
pub mod retention;
pub mod validate;
pub mod write;

pub use cache::{CacheConfig, EngineCache};
pub use error::EngineError;
pub use maintain::{MAINTAIN_BATCH, MaintainCursor, MaintainReport};
pub use mutate::ForgetSelector;
pub use outcome::{ForgetOutcome, PurgeOutcome, WriteOutcome};
pub use retention::{AuditRetention, PurgeCascade, RetentionProfile, RetentionSpan};
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
    pub cache: CacheConfig,
    /// Which named audit-retention policy governs `Engine::purge_subject`.
    /// See `retention` module doc.
    pub retention: RetentionProfile,
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
            cache: CacheConfig::default(),
            retention: RetentionProfile::default(),
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
    pub(crate) cache: cache::EngineCache,
    pub(crate) retention: RetentionProfile,
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
            cache: cache::EngineCache::new(config.cache),
            retention: config.retention,
        }
    }

    /// Embeds through the content-addressed cache: the same text always
    /// embeds identically, so a cache hit is free correctness, not a
    /// staleness risk. A missing or failed embedder must never cost a user
    /// their memory, so a failure here is folded into `None`, exactly as the
    /// direct `self.embedder.embed(...).ok()` call it replaces did.
    pub(crate) async fn embed_cached(&self, text: &str) -> Option<memorysafe_core::Embedding> {
        if let Some(hit) = self.cache.embedding(text).await {
            return Some(hit);
        }
        match self.embedder.embed(text) {
            Ok(v) => {
                self.cache.put_embedding(text, v.clone()).await;
                Some(v)
            }
            Err(_) => None,
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

    /// A namespace's budget and what it has used, read through to the backend.
    ///
    /// Deliberately a straight read-through with no caching. `CacheConfig`
    /// caches `ScopeStats`, never `CapacityState`, and even that stats cache
    /// is currently unreachable from any read path — `gather::assess_context`,
    /// `read::recall` and `maintain` all call `Backend::scope_stats` directly,
    /// and the only callers of `EngineCache::stats`/`put_stats` anywhere in
    /// the workspace are this crate's own test modules in `mutate.rs` and
    /// `maintain.rs` (`cache.rs` itself has no `#[cfg(test)]` module at all,
    /// so do not go looking there). So there is no cached capacity
    /// figure anywhere to go stale, and none should be introduced here: this
    /// is the accessor `capacity_is_never_exceeded` (`tests/invariants.rs`)
    /// cross-checks the stored item count against, and a cached answer would
    /// turn that cross-check into a comparison of the accounting with itself.
    pub async fn capacity_state(
        &self,
        scope: &Scope,
    ) -> Result<memorysafe_core::CapacityState, EngineError> {
        Ok(self.backend.capacity_state(scope).await?)
    }
}
