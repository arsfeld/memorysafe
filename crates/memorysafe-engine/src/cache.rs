use memorysafe_core::{Embedding, Scope, ScopeStats};
use moka::future::Cache;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct CacheConfig {
    pub embedding_capacity: u64,
    pub stats_capacity: u64,
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
