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
pub mod reembed;
pub mod retention;
pub mod validate;
pub mod write;

pub use cache::{CacheConfig, EngineCache};
pub use error::EngineError;
pub use maintain::{MAINTAIN_BATCH, MaintainCursor, MaintainReport};
pub use mutate::ForgetSelector;
pub use outcome::{ForgetOutcome, PurgeOutcome, WriteOutcome};
pub use reembed::{REEMBED_BATCH, ReembedCursor, ReembedReport};
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

    /// Sets a namespace's capacity ceiling, and **audits the change**.
    ///
    /// A budget is governance state, not a tuning knob: `maintain` reclaims a
    /// namespace that is over its ceiling, and `admit` decides against the
    /// capacity pressure the ceiling defines — so lowering one causes
    /// evictions, and raising one stops them. Neither the change nor its
    /// author was recorded anywhere before this, which made "why did these
    /// memories disappear on Tuesday" unanswerable from a trail that records
    /// the evictions themselves in full detail.
    ///
    /// `AuditEvent::PolicyChanged` is the variant for it, and this is its
    /// first and only construction site in the workspace.
    ///
    /// `Actor::system()`, with the same engine-wide gap `purge_subject`'s doc
    /// comment (`mutate.rs`) records in full: no engine method below the
    /// boundary takes an actor yet, and threading one is Plan 3's Task 2. It
    /// is worth more here than anywhere else — "who raised this tenant's
    /// ceiling" is the question the row exists to answer — so this is the
    /// site to revisit first when actors land.
    ///
    /// **The new ceiling itself is not in the row, only the fact that it
    /// changed.** `AuditRecord` has no field free to carry two numbers:
    /// `decision` keys the row's `audit_aggregates` bucket on its `PolicyId`
    /// and `assessment` feeds that bucket's score histograms, so either would
    /// corrupt the aggregates to smuggle a budget through. Same gap, and same
    /// ledger entry, as `Engine::import`'s record counts.
    ///
    /// **Not atomic with the change.** `Backend::set_budget` takes no audit
    /// record (unlike `purge_subject`, which does), so this is a second
    /// transaction: a crash between them leaves a changed ceiling with no
    /// record. Written *after* the change so a failed change is not audited
    /// as having happened; closing the window is a `Backend` signature change
    /// for the next contract batch.
    pub async fn set_budget(&self, scope: &Scope, budget: Budget) -> Result<(), EngineError> {
        self.backend.set_budget(scope, budget).await?;
        let audit = AuditRecord::new(
            scope.clone(),
            memorysafe_core::AuditEvent::PolicyChanged,
            vec![],
            memorysafe_core::Actor::system(),
            time::OffsetDateTime::now_utc(),
        );
        self.backend
            .apply(memorysafe_backend::WriteTransaction::new(
                scope.clone(),
                audit,
            ))
            .await?;
        Ok(())
    }

    /// A namespace's budget and what it has used, read through to the backend.
    ///
    /// Deliberately a straight read-through with no caching. `CacheConfig`
    /// caches `ScopeStats`, never `CapacityState`, and even that stats cache
    /// is currently unreachable from any read path — `gather::assess_context`,
    /// `read::recall` and `maintain` all call `Backend::scope_stats` directly,
    /// and the only callers of `EngineCache::stats`/`put_stats` anywhere in
    /// the workspace are this crate's own tests — the `#[cfg(test)]` modules
    /// in `mutate.rs`, `maintain.rs` and `reembed.rs`, and the integration
    /// test `tests/cache.rs`, which is where the cache's own unit coverage
    /// lives because `src/cache.rs` has no `#[cfg(test)]` module of its own.
    /// (An earlier revision of this sentence named only the three `src`
    /// modules and told the reader not to look for cache tests. It was
    /// written from a grep over `src/*.rs` alone while claiming workspace
    /// scope, and it pointed away from `tests/cache.rs` at exactly the moment
    /// that file is what a reader wants.) So there is no cached capacity
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
