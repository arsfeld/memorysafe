use memorysafe_core::{Embedding, Scope, ScopeStats};
use moka::future::Cache;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct CacheConfig {
    pub embedding_capacity: u64,
    /// **Has no effect today.** The stats cache this bounds is fully built
    /// and invalidated (see `EngineCache`'s own doc comment), but nothing in
    /// production reads through it — `stats()`/`put_stats()` are called only
    /// by this crate's own tests. An operator who raises or lowers this and
    /// measures the result will see nothing change, because nothing
    /// currently consumes the cache it sizes.
    pub stats_capacity: u64,
    /// **Has no effect today**, for the same reason `stats_capacity` does
    /// not — see `EngineCache`'s own doc comment for the full account of why
    /// the stats cache has no reader yet, and what wiring one would cost.
    pub stats_ttl: Duration,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            embedding_capacity: 10_000,
            stats_capacity: 5_000,
            // Short: statistics feed fragility scoring, and a stale corpus mean
            // skews every assessment made against it.
            stats_ttl: Duration::from_secs(30),
        }
    }
}

/// In-process caches. Deliberately excludes composed working sets: those depend
/// on the corpus, the clock, and replay state, so a stale one would surface
/// memories that have since been forgotten.
///
/// **The stats half is fully wired — built, filled, invalidated on every
/// write — but deliberately has no reader in production.**
/// `gather::assess_context`, `read.rs::recall`, and `maintain.rs` all fetch
/// `ScopeStats` directly from `Backend::scope_stats` and never call
/// `stats()`; only this crate's own tests call `stats()`/`put_stats()`
/// today. That makes `CacheConfig::stats_capacity` and `CacheConfig::stats_ttl`
/// tunable knobs with no observable effect: raising, lowering, or disabling
/// either changes nothing an operator can measure, because nothing consumes
/// the cache they size.
///
/// This is a deliberate ruling, not an oversight left for a later task to
/// close casually. Reading `ScopeStats` through this cache would put a
/// policy's `assess`/`admit`/`maintain` decisions on a corpus statistic that
/// can be stale — a change to governed behaviour, not a repair of a plan
/// defect, and one this task does not have ratification to make on its own.
/// It is also not a small change mechanically: `gather::assess_context` is a
/// free function over `&dyn Backend` with no `self` and no cache handle, and
/// `ScopeStats::mean_neighbour_similarity` is recomputed per call from that
/// call's own fetched neighbours rather than stored verbatim on the backend
/// row — a read-through would have to cache the raw backend value and
/// re-derive the similarity separately, which is a design decision and a
/// signature change, not a two-line wiring edit.
///
/// **What leaving it unwired costs today: nothing, for a single-instance,
/// sole-writer deployment.** Invalidation is wired into all six
/// `Backend::apply`/`Backend::purge_subject` call sites in this engine
/// (`remember`, `forget`, `protect`, `purge_subject`, `maintain`'s
/// decision-application write, and `apply_merge` — see each one's own call
/// to `invalidate_scope`). Since every corpus change this engine can make
/// goes through one of those six surfaces, and each invalidates before the
/// write it guards can be observed, `Backend::scope_stats` read directly (as
/// every caller does today) can never be stale relative to *this engine's
/// own writes* — there is no window for `stats_ttl` to matter yet. The
/// exposure a read-through would start bounding is external to any single
/// `Engine`: a second engine instance, or a writer outside this engine
/// entirely, changing the same backend's data through a path that calls
/// none of the six surfaces above. Wiring `stats()`/`put_stats()` into a
/// read path would only turn that unbounded exposure into one bounded by
/// `stats_ttl` (30 seconds by default) — and that bound would come *entirely*
/// from those six surfaces staying wired. **Unwiring `invalidate_scope` from
/// any one of them would silently widen this bound back toward unbounded,
/// with no test able to catch the regression until the stats cache actually
/// has a reader to make it observable.**
pub struct EngineCache {
    embeddings: Cache<String, Embedding>,
    stats: Cache<String, ScopeStats>,
}

impl EngineCache {
    pub fn new(config: CacheConfig) -> Self {
        Self {
            embeddings: Cache::builder()
                .max_capacity(config.embedding_capacity)
                .build(),
            stats: Cache::builder()
                .max_capacity(config.stats_capacity)
                .time_to_live(config.stats_ttl)
                .build(),
        }
    }

    fn embedding_key(text: &str) -> String {
        blake3::hash(text.as_bytes()).to_hex().to_string()
    }

    pub async fn embedding(&self, text: &str) -> Option<Embedding> {
        self.embeddings.get(&Self::embedding_key(text)).await
    }

    pub async fn put_embedding(&self, text: &str, embedding: Embedding) {
        self.embeddings
            .insert(Self::embedding_key(text), embedding)
            .await;
    }

    pub async fn stats(&self, scope: &Scope) -> Option<ScopeStats> {
        self.stats.get(&scope.key()).await
    }

    pub async fn put_stats(&self, scope: &Scope, stats: ScopeStats) {
        self.stats.insert(scope.key(), stats).await;
    }

    /// Called after every write. Embeddings are content-addressed and are not
    /// affected.
    pub async fn invalidate_scope(&self, scope: &Scope) {
        self.stats.invalidate(&scope.key()).await;
    }
}

impl Default for EngineCache {
    fn default() -> Self {
        Self::new(CacheConfig::default())
    }
}
