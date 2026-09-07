//! `protect` deletes the item's row and reinserts it (eviction plus upsert
//! in one transaction), and its own comment states that the delete cascades
//! the item's vector away, so `protect` must recompute one before writing
//! the row back. Hardcoding that embedding to `None` compiles and passes
//! every test in `mutate.rs`: `DeterministicEmbedder`'s vectors are
//! "monotone in token overlap" (see its own doc comment), so a fixture built
//! on it cannot separate "found via keyword search" from "found via vector
//! search" — exactly the gap `read_embedder_reach.rs` closes for `recall`'s
//! own embedding computation. This file is that file's counterpart for
//! `protect`, reusing the same `SpyEmbedder` technique: a double that
//! ignores its input text and always returns the same unit vector, which
//! decouples "shares a keyword with the query" from "is vector-similar to
//! the query".

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    EmbedderId, Embedding, Protection, RecallBudget, RecallMode, RecallRequest, Scope,
    SensitivityLevel,
};
use memorysafe_embed::{EmbedError, Embedder};
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Ignores its input text: every successful call returns the identical unit
/// vector regardless of what text is embedded, and every call while
/// `set_failing(true)` returns the same error regardless of what text is
/// embedded (the same double `read_embedder_reach.rs` uses, for the same
/// reason). Counts calls, since a mutation that skips calling the embedder
/// entirely (rather than merely discarding its result) is a distinct failure
/// mode worth being able to detect directly.
struct SpyEmbedder {
    dim: u16,
    id: EmbedderId,
    calls: AtomicUsize,
    fail: AtomicBool,
}

impl SpyEmbedder {
    fn new(dim: u16) -> Self {
        Self {
            dim,
            id: EmbedderId::new("spy-embedder"),
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        }
    }

    fn set_failing(&self, fail: bool) {
        self.fail.store(fail, Ordering::SeqCst);
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
        if self.fail.load(Ordering::SeqCst) {
            return Err(EmbedError::Unavailable(
                "forced failure for a test double".into(),
            ));
        }
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

/// Seeds a body and later queries with text that share zero tokens, so
/// keyword search (FTS5, exact-term OR-matching) must return nothing for it
/// on its own — a nonempty recall after `protect` can only be explained by a
/// real vector row surviving the delete-then-reinsert. Also asserts the
/// embedder was actually invoked a second time (at `protect`, not just at
/// the original `remember`), so a mutation that skips the re-embed call
/// entirely — not just one that discards its result — is caught too.
#[tokio::test]
async fn protecting_preserves_vector_search_reachability() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spy = Arc::new(SpyEmbedder::new(8));
    let engine = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        spy.clone(),
        Arc::new(BaselinePolicy::default()),
    ));

    let id = engine
        .remember(RememberRequest::new(
            scope(),
            "zzyyxx111 wwvvuu222 ttssrr333",
        ))
        .await
        .unwrap()
        .item_id
        .unwrap();
    let calls_after_remember = spy.calls();
    assert!(
        calls_after_remember > 0,
        "the embedder must be invoked at write time"
    );

    engine
        .protect(&scope(), &id, Protection::Pinned)
        .await
        .unwrap();
    assert!(
        spy.calls() > calls_after_remember,
        "protect must re-embed the item rather than reuse (or skip) a vector"
    );

    let ws = engine
        .recall(recall_req("aabbcc444 ddeeff555 gghhii666"))
        .await
        .unwrap();

    assert!(
        !ws.items.is_empty(),
        "the item shares no keyword with the query, so it can only have been \
         found through the vector arm — protect's re-embed never reached the \
         backend, or its vector was dropped"
    );
}

/// `protect` is delete-then-insert: the eviction it opens with cascades the
/// item's vector row away, and the insert does not restore one on its own —
/// only the freshly computed `vector` does. If the re-embed attempted here
/// fails, the item must come back out the other side flagged
/// `pending_embedding: true`, mirroring `remember`'s rule (`write.rs`:
/// `pending_embedding = embedding.is_none()`). Without that, a failed
/// re-embed during `protect` leaves an item with NEITHER a vector row NOR the
/// flag that would let the backfill job find and repair it — invisible to
/// vector search and invisible to its own remedy.
#[tokio::test]
async fn a_failed_reembed_during_protect_marks_the_item_pending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spy = Arc::new(SpyEmbedder::new(8));
    let engine = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        spy.clone(),
        Arc::new(BaselinePolicy::default()),
    ));

    let id = engine
        .remember(RememberRequest::new(
            scope(),
            "content the embedder can vectorise",
        ))
        .await
        .unwrap()
        .item_id
        .unwrap();

    let stored = engine.review(&scope(), &Default::default()).await.unwrap();
    assert!(
        !stored
            .iter()
            .find(|i| i.id == id)
            .unwrap()
            .pending_embedding,
        "premise: the seeding remember succeeded and is not pending"
    );

    spy.set_failing(true);
    engine
        .protect(&scope(), &id, Protection::Pinned)
        .await
        .unwrap();

    let stored = engine.review(&scope(), &Default::default()).await.unwrap();
    let item = stored.iter().find(|i| i.id == id).unwrap();
    assert!(
        item.pending_embedding,
        "an embedder failure during protect must leave the item marked \
         pending_embedding, not silently drop the vector with no way to \
         find it again"
    );
}
