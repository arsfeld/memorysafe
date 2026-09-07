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
pub mod settings;
pub mod validate;
pub mod write;

pub use cache::{CacheConfig, EngineCache};
pub use error::EngineError;
pub use maintain::{MAINTAIN_BATCH, MaintainCursor, MaintainReport};
pub use mutate::ForgetSelector;
pub use outcome::{ForgetOutcome, PurgeOutcome, WriteOutcome};
pub use reembed::{REEMBED_BATCH, ReembedCursor, ReembedReport};
pub use retention::{AuditRetention, PurgeCascade, RetentionProfile, RetentionSpan};
pub use settings::TenantSettings;
pub use validate::FailureStance;
pub use write::RememberRequest;

use memorysafe_backend::{Backend, ImportReport, Page, ScopeSelector, WriteTransaction};
use memorysafe_core::{
    Action, Actor, AuditEvent, AuditFilter, AuditId, AuditRecord, Budget, Decision,
    GovernancePolicy, MemoryItem, PolicyId, Reason, ReasonCode, Scope, TenantId, features,
};
use memorysafe_embed::Embedder;
use memorysafe_policy::{BaselineConfig, BaselinePolicy};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use time::OffsetDateTime;

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

/// A tenant's policy override: the `BaselineConfig` kept alongside the built
/// `Arc<dyn GovernancePolicy>` so `Engine::tenant_settings` can hand a
/// snapshot back without downcasting a trait object to recover it.
type TenantPolicyOverride = (BaselineConfig, Arc<dyn GovernancePolicy>);

pub struct Engine {
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) embedder: Arc<dyn Embedder>,
    pub(crate) default_policy: Arc<dyn GovernancePolicy>,
    pub(crate) fallback_policy: Arc<dyn GovernancePolicy>,
    pub(crate) stance: FailureStance,
    pub(crate) neighbour_k: usize,
    pub(crate) eviction_candidates: usize,
    pub(crate) cache: cache::EngineCache,
    pub(crate) default_retention: RetentionProfile,
    /// Per-tenant policy overrides, keyed by tenant. A tenant with no entry
    /// here uses `default_policy`.
    pub(crate) policies: RwLock<HashMap<TenantId, TenantPolicyOverride>>,
    /// Per-tenant retention overrides, keyed by tenant. A tenant with no entry
    /// here uses `default_retention`.
    pub(crate) retentions: RwLock<HashMap<TenantId, RetentionProfile>>,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        Self {
            backend: config.backend,
            embedder: config.embedder,
            default_policy: config.policy,
            fallback_policy: config.fallback_policy,
            stance: config.stance,
            neighbour_k: config.neighbour_k,
            eviction_candidates: config.eviction_candidates,
            cache: cache::EngineCache::new(config.cache),
            default_retention: config.retention,
            policies: RwLock::new(HashMap::new()),
            retentions: RwLock::new(HashMap::new()),
        }
    }

    /// The policy that governs `tenant`. Every pipeline calls this instead of
    /// reading a field directly, so a tenant override cannot be missed on one
    /// path.
    pub fn policy_for(&self, tenant: &TenantId) -> Arc<dyn GovernancePolicy> {
        self.policies
            .read()
            .expect("policy registry lock poisoned")
            .get(tenant)
            .map(|(_, p)| p.clone())
            .unwrap_or_else(|| self.default_policy.clone())
    }

    /// The `RetentionProfile` that governs `tenant`'s audit retention and
    /// `purge_subject` cascade.
    pub fn retention_for(&self, tenant: &TenantId) -> RetentionProfile {
        self.retentions
            .read()
            .expect("retention registry lock poisoned")
            .get(tenant)
            .copied()
            .unwrap_or(self.default_retention)
    }

    /// A snapshot of everything `tenant` has configured. Returned by value so
    /// a caller never holds the registries' locks.
    ///
    /// **`retention` genuinely falls back to the engine's default**
    /// (`retention_for`, which reads `self.default_retention`) — the two can
    /// never disagree.
    ///
    /// **`policy_config` does not, and the two accessors below can disagree
    /// under a custom policy.** With no override on record, this falls back
    /// to `BaselineConfig::default()` — a hardcoded literal, not
    /// `self.default_policy`'s own configuration — because `default_policy`
    /// is an opaque `Arc<dyn GovernancePolicy>` with no way to ask it for the
    /// `BaselineConfig` it was built from (and, for anything other than
    /// `BaselinePolicy`, no `BaselineConfig` to ask for at all: this whole
    /// struct is a fiction under a custom policy). So `tenant_settings(t)`
    /// and `policy_for(t)` agree only when the engine happens to have been
    /// constructed with `EngineConfig { policy: Arc::new(BaselinePolicy::default()), .. }`.
    /// A deployment built with a tuned `BaselinePolicy::new(cfg)` as its
    /// engine-wide default gets `policy_for(t)` returning that tuned policy
    /// while `tenant_settings(t).policy_config` reports stock defaults for
    /// any tenant with no override of its own — the two disagree about what
    /// actually governs the tenant's writes until something calls
    /// `set_tenant_policy_config` for it. There is no fix available at this
    /// layer: closing the gap needs `EngineConfig`/`GovernancePolicy` to
    /// expose a policy's own configuration, which is out of this method's
    /// reach.
    pub fn tenant_settings(&self, tenant: &TenantId) -> TenantSettings {
        let policy_config = self
            .policies
            .read()
            .expect("policy registry lock poisoned")
            .get(tenant)
            .map(|(c, _)| c.clone())
            .unwrap_or_default();
        TenantSettings {
            policy_config,
            retention: self.retention_for(tenant),
        }
    }

    /// Replaces `tenant`'s policy configuration, and audits the change under
    /// the tenant's reserved admin scope.
    ///
    /// Validated before anything observable happens: a refused change must
    /// leave neither a new policy installed nor an audit row claiming one
    /// happened. `PolicyChanged` is written once, after validation and before
    /// the new policy is installed, so a crash between the two leaves the
    /// audited transition ahead of the registry rather than the reverse — the
    /// same ordering `set_budget` documents for its own two-transaction gap.
    pub async fn set_tenant_policy_config(
        &self,
        tenant: &TenantId,
        cfg: BaselineConfig,
        actor: &Actor,
    ) -> Result<AuditId, EngineError> {
        settings::validate(&cfg)?;

        let before = self.policy_for(tenant).id();
        let policy: Arc<dyn GovernancePolicy> = Arc::new(BaselinePolicy::new(cfg.clone()));
        let after = policy.id();

        // The `detail` string alone only says *that* the policy changed, not
        // *to what* — `BaselinePolicy::id()` is `"baseline"`@`BASELINE_VERSION`
        // unconditionally, so it does not vary with `cfg` and `before`/`after`
        // above are textually identical for every `BaselinePolicy` transition.
        // The incoming thresholds go into `evidence` instead, which the
        // constraint that audit rows carry "ids, content digests, and feature
        // numbers" exists to allow: an operator reading this row can now see
        // what actually changed, not just that something did.
        let audit_id = self
            .record_admin_event(
                tenant,
                AuditEvent::PolicyChanged,
                actor,
                Reason::new(
                    ReasonCode::PolicyInvalid,
                    &format!("policy configuration replaced: {before} -> {after}"),
                    features! {
                        "duplicate_threshold" => cfg.duplicate_threshold,
                        "merge_threshold" => cfg.merge_threshold,
                        "near_duplicate_floor" => cfg.near_duplicate_floor,
                        "replay_quota" => cfg.replay_quota,
                        "mmr_lambda" => cfg.mmr_lambda,
                    },
                ),
                after.clone(),
            )
            .await?;

        self.policies
            .write()
            .expect("policy registry lock poisoned")
            .insert(tenant.clone(), (cfg, policy));
        Ok(audit_id)
    }

    /// Replaces `tenant`'s audit-retention profile, and audits the change
    /// under the tenant's reserved admin scope.
    pub async fn set_tenant_retention(
        &self,
        tenant: &TenantId,
        profile: RetentionProfile,
        actor: &Actor,
    ) -> Result<AuditId, EngineError> {
        let before = self.retention_for(tenant);
        let audit_id = self
            .record_admin_event(
                tenant,
                AuditEvent::PolicyChanged,
                actor,
                Reason::new(
                    ReasonCode::PolicyInvalid,
                    &format!("audit retention changed: {before:?} -> {profile:?}"),
                    features! {},
                ),
                self.policy_for(tenant).id(),
            )
            .await?;

        self.retentions
            .write()
            .expect("retention registry lock poisoned")
            .insert(tenant.clone(), profile);
        Ok(audit_id)
    }

    /// `Engine::export_ndjson` (`portability.rs`) with the export itself
    /// audited as a governance event, naming the actor who asked for it.
    /// `Exported` is filed under the *selector's* tenant, which is always the
    /// caller's own — nothing here constructs a `Scope` for any other
    /// tenant.
    pub async fn export_ndjson_as(
        &self,
        sel: &ScopeSelector,
        actor: &Actor,
    ) -> Result<String, EngineError> {
        let ndjson = self.export_ndjson(sel).await?;
        self.record_admin_event(
            &sel.tenant,
            AuditEvent::Exported,
            actor,
            Reason::new(
                ReasonCode::PolicyInvalid,
                &format!(
                    "exported subject={} namespace={} include_audit={}",
                    sel.subject.as_ref().map_or("*", |s| s.as_str()),
                    sel.namespace.as_ref().map_or("*", |n| n.as_str()),
                    sel.include_audit
                ),
                features! {},
            ),
            self.policy_for(&sel.tenant).id(),
        )
        .await?;
        Ok(ndjson)
    }

    /// `Engine::import_ndjson` (`portability.rs`) with the `Imported` row
    /// naming `actor` rather than `Actor::system()`.
    ///
    /// **Not** implemented as `self.import_ndjson(...)` followed by a second,
    /// `record_admin_event`-built `Imported` row: `import` already writes one
    /// `Imported` row internally (see its own doc), and layering a second on
    /// top would leave every actor-attributed import with two rows for one
    /// event — caught by
    /// `an_import_is_audited_with_the_actor_who_asked_for_it`
    /// (`tests/settings.rs`), which counts exactly one. Instead this calls
    /// straight through to `import_ndjson_with_actor`, the same parse-and-import
    /// path `import_ndjson` itself uses, so there is exactly one `Imported`
    /// construction site (`import_as`, `portability.rs`) no matter which
    /// public entry point a caller used.
    ///
    /// `Engine::import_ndjson` takes the destination tenant explicitly (Plan
    /// 1 Task 39); this wrapper already has the authorised one in hand, so
    /// it passes it rather than letting the payload name its own target.
    pub async fn import_ndjson_as(
        &self,
        ndjson: &str,
        tenant: &TenantId,
        actor: &Actor,
    ) -> Result<ImportReport, EngineError> {
        self.import_ndjson_with_actor(ndjson, tenant, actor).await
    }

    /// One audit row in the tenant's reserved admin scope, carrying no
    /// items — these events are about the tenant, not about any memory.
    ///
    /// `Action::Reject` on an administrative decision is deliberate:
    /// `Action` describes what happened to a *memory*, and a policy change
    /// happens to no memory, so `Reject` is the variant that admits nothing.
    /// A fifth `Action` variant would change a serialised wire format Plan 1
    /// froze in audit rows, for the sake of three administrative events; the
    /// event discriminates, the action does not. `ReasonCode` has no
    /// administrative variant for the same reason — it too is a wire format
    /// where renaming or adding is tracked deliberately — so
    /// `ReasonCode::PolicyInvalid` is the closest existing code and the
    /// `detail` string carries the actual transition.
    async fn record_admin_event(
        &self,
        tenant: &TenantId,
        event: AuditEvent,
        actor: &Actor,
        reason: Reason,
        policy: PolicyId,
    ) -> Result<AuditId, EngineError> {
        let scope = Scope::admin(tenant);
        let decision = Decision {
            subject: None,
            action: Action::Reject,
            evictions: vec![],
            reasons: vec![reason],
            policy,
        };
        let record = AuditRecord::new(
            scope.clone(),
            event,
            vec![],
            actor.clone(),
            OffsetDateTime::now_utc(),
        )
        .with_decision(decision);

        let txn = WriteTransaction::new(scope, record);
        Ok(self.backend.apply(txn).await?.audit_id)
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
    /// `AuditEvent::PolicyChanged` is the variant for it. **No longer this
    /// method's only construction site**: `set_tenant_policy_config` and
    /// `set_tenant_retention` (both above, Plan 3's Task 2) build the same
    /// variant for their own admin-scope rows.
    ///
    /// `Actor::system()`, unlike those two: actors have landed for the
    /// per-tenant policy/retention setters, `export_ndjson_as`,
    /// `import_ndjson_as` and `purge_subject` (`mutate.rs`), but `set_budget`
    /// was not in that task's scope and remains unattributed. It is worth
    /// more here than anywhere else — "who raised this tenant's ceiling" is
    /// the question the row exists to answer — so this is still the site to
    /// revisit first, next.
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
