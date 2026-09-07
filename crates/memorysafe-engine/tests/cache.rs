use memorysafe_core::Scope;
use memorysafe_embed::{DeterministicEmbedder, Embedder};
use memorysafe_engine::cache::{CacheConfig, EngineCache};

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

#[tokio::test]
async fn an_embedding_is_returned_from_cache_on_the_second_ask() {
    let c = EngineCache::new(CacheConfig::default());
    let e = DeterministicEmbedder::new(256);
    let v = e.embed("some memory text").unwrap();

    assert!(c.embedding("some memory text").await.is_none());
    c.put_embedding("some memory text", v.clone()).await;
    assert_eq!(
        c.embedding("some memory text").await.unwrap().vector,
        v.vector
    );
}

#[tokio::test]
async fn different_text_does_not_collide() {
    let c = EngineCache::new(CacheConfig::default());
    let e = DeterministicEmbedder::new(256);
    c.put_embedding("first", e.embed("first").unwrap()).await;
    assert!(c.embedding("second").await.is_none());
}

#[tokio::test]
async fn scope_stats_are_cached_and_invalidated_together() {
    let c = EngineCache::new(CacheConfig::default());
    let stats = memorysafe_core::ScopeStats {
        item_count: 7,
        ..Default::default()
    };

    c.put_stats(&scope(), stats.clone()).await;
    assert_eq!(c.stats(&scope()).await.unwrap().item_count, 7);

    c.invalidate_scope(&scope()).await;
    assert!(
        c.stats(&scope()).await.is_none(),
        "a write must invalidate its scope"
    );
}

#[tokio::test]
async fn invalidating_one_scope_leaves_another_alone() {
    let c = EngineCache::new(CacheConfig::default());
    let other = Scope::new("acme", "user-99", "agent").unwrap();
    let stats = memorysafe_core::ScopeStats {
        item_count: 3,
        ..Default::default()
    };

    c.put_stats(&scope(), stats.clone()).await;
    c.put_stats(&other, stats).await;
    c.invalidate_scope(&scope()).await;

    assert!(c.stats(&scope()).await.is_none());
    assert!(
        c.stats(&other).await.is_some(),
        "invalidation crossed scopes"
    );
}

#[tokio::test]
async fn embeddings_survive_scope_invalidation() {
    // Embeddings are content-addressed, so a write cannot make one stale.
    let c = EngineCache::new(CacheConfig::default());
    let e = DeterministicEmbedder::new(256);
    c.put_embedding("durable text", e.embed("durable text").unwrap())
        .await;
    c.invalidate_scope(&scope()).await;
    assert!(c.embedding("durable text").await.is_some());
}

/// Beyond the brief's literal five: the brief's own prose gives `stats_ttl` a
/// reason ("statistics feed fragility scoring, and a stale corpus mean skews
/// every assessment"), but none of the five mandated tests ever waits for a
/// TTL to elapse — every one of them tests only explicit `invalidate_scope`.
/// A mutation dropping `.time_to_live(config.stats_ttl)` from `EngineCache`'s
/// stats cache builder entirely (see the task report's mutation log) passes
/// all five mandated tests and would ship a cache that never expires on its
/// own. A short, config-supplied TTL exercises the real mechanism without
/// waiting on the 30-second default.
#[tokio::test]
async fn stats_expire_on_their_own_after_the_configured_ttl() {
    let c = EngineCache::new(CacheConfig {
        stats_ttl: std::time::Duration::from_millis(20),
        ..CacheConfig::default()
    });
    let stats = memorysafe_core::ScopeStats {
        item_count: 5,
        ..Default::default()
    };
    c.put_stats(&scope(), stats).await;
    assert!(
        c.stats(&scope()).await.is_some(),
        "premise: the entry is cached"
    );

    // `tokio`'s `time` feature is not enabled for this crate (see Cargo.toml:
    // only `rt`, `rt-multi-thread`, `macros`, `sync`), so a blocking sleep
    // stands in for `tokio::time::sleep`. Harmless here: nothing else is
    // scheduled on this test's runtime concurrently.
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        c.stats(&scope()).await.is_none(),
        "a scope's cached stats must expire on their own once the TTL elapses, \
         with no explicit invalidate_scope call"
    );
}
