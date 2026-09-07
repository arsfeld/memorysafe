//! `Engine::embed_cached` is the only mechanism Task 37 places on a
//! production path: `write.rs::remember` and `read.rs::recall` both call it
//! in place of the direct `self.embedder.embed(...)` call they replaced.
//! `tests/cache.rs` exercises `EngineCache` in isolation and never
//! constructs an `Engine` at all, so nothing anywhere proves the cache is
//! actually consulted from either call site — deleting the cache-hit early
//! return in `embed_cached`, or reverting either call site back to a direct
//! `self.embedder.embed(...)` call, changes no observable in any other test
//! in the crate. `SpyEmbedder` (the same double `read_embedder_reach.rs` and
//! `protect_embedder_reach.rs` use) counts calls, so repeating an identical
//! body/query and asserting the count does not move directly observes
//! whether the cache was actually hit, not just whether the write/read
//! succeeded.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    EmbedderId, Embedding, RecallBudget, RecallMode, RecallRequest, Scope, SensitivityLevel,
};
use memorysafe_embed::{EmbedError, Embedder};
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Ignores its input text and always returns the same unit vector; counts
/// calls so a cache hit (no call) is distinguishable from a cache miss (a
/// call) directly, rather than inferred from the write/read outcome alone.
struct SpyEmbedder {
    dim: u16,
    id: EmbedderId,
    calls: AtomicUsize,
}

impl SpyEmbedder {
    fn new(dim: u16) -> Self {
        Self {
            dim,
            id: EmbedderId::new("spy-embedder"),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Embedder for SpyEmbedder {
    fn id(&self) -> EmbedderId {
        self.id.clone()
    }

    fn dim(&self) -> u16 {
        self.dim
    }

    fn embed(&self, _text: &str) -> Result<Embedding, EmbedError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut v = vec![0.0f32; self.dim as usize];
        v[0] = 1.0;
        Ok(Embedding::new(v, self.id.clone()))
    }
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

fn recall_req(query: &str) -> RecallRequest {
    RecallRequest {
        scope: scope(),
        query: Some(query.into()),
        tags_any: vec![],
        kinds: vec![],
        occurred_after: None,
        occurred_before: None,
        mode: RecallMode::WorkingSet,
        budget: RecallBudget {
            max_tokens: Some(4000),
            max_items: Some(5),
        },
        sensitivity_ceiling: SensitivityLevel::Restricted,
    }
}

/// `write.rs::remember` calls `self.embed_cached(&req.body)` unconditionally,
/// before any admission decision is made. Remembering the identical body
/// twice must embed it once: the second call is a cache hit by content hash,
/// regardless of what the second write's own admission decision turns out to
/// be (near-duplicate handling is irrelevant here — only the embedder call
/// count is being observed).
#[tokio::test]
async fn remembering_the_same_body_twice_embeds_it_only_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spy = Arc::new(SpyEmbedder::new(8));
    let engine = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        spy.clone(),
        Arc::new(BaselinePolicy::default()),
    ));

    engine
        .remember(RememberRequest::new(
            scope(),
            "an identical body remembered twice",
        ))
        .await
        .unwrap();
    let calls_after_first = spy.calls();
    assert!(
        calls_after_first > 0,
        "the embedder must be invoked on the first, cache-missing call"
    );

    engine
        .remember(RememberRequest::new(
            scope(),
            "an identical body remembered twice",
        ))
        .await
        .unwrap();
    assert_eq!(
        spy.calls(),
        calls_after_first,
        "a repeated body must hit the embedding cache, not re-embed"
    );
}

/// `read.rs::recall` calls `self.embed_cached(q)` for any non-empty query,
/// before ever touching the backend. Recalling with the identical query text
/// twice must embed it once.
#[tokio::test]
async fn recalling_the_same_query_twice_embeds_it_only_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spy = Arc::new(SpyEmbedder::new(8));
    let engine = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        spy.clone(),
        Arc::new(BaselinePolicy::default()),
    ));

    engine
        .recall(recall_req("an identical query asked twice"))
        .await
        .unwrap();
    let calls_after_first = spy.calls();
    assert!(
        calls_after_first > 0,
        "the embedder must be invoked on the first, cache-missing call"
    );

    engine
        .recall(recall_req("an identical query asked twice"))
        .await
        .unwrap();
    assert_eq!(
        spy.calls(),
        calls_after_first,
        "a repeated query must hit the embedding cache, not re-embed"
    );
}
