//! Two value-deciding lines in `Engine::recall` had no covering test of
//! either mutation class before this file: the embedding computation itself
//! (`let embedding = req.query...and_then(|q| self.embedder.embed(q).ok());`),
//! and the hardcoded `exclude_pending_embedding: false` on `HardFilters`.
//!
//! Neither can be tested with `DeterministicEmbedder`: it is, by its own doc
//! comment, "monotone in token overlap" — two bodies sharing no tokens also
//! get no vector similarity from it, so a fixture built on it cannot
//! separate "found via keyword search" from "found via vector search". This
//! file uses `SpyEmbedder`, a double that ignores its input text entirely
//! and always returns the same unit vector (or, when configured to fail,
//! always errors), which decouples "shares a keyword with the query" from
//! "is vector-similar to the query" and lets each mechanism be exercised in
//! isolation. It carries its own test double, the same reason
//! `read_policy_failure.rs` and `read_leak_prevention.rs` each earn their
//! own binary rather than folding into a shared file.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    EmbedderId, Embedding, RecallBudget, RecallMode, RecallRequest, Scope, SensitivityLevel,
};
use memorysafe_embed::{EmbedError, Embedder};
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Ignores its input text: every successful call returns the identical unit
/// vector regardless of what text is embedded, and every call while
/// `set_failing(true)` returns the same error regardless of what text is
/// embedded. Also counts calls, since a mutation that skips calling the
/// embedder entirely (rather than merely discarding its result) is a
/// distinct failure mode worth being able to detect directly.
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

/// Mutating `read.rs`'s embedding computation to a hardcoded `let embedding
/// = None;` compiles and passes every other test in the crate: every
/// existing fixture's query is a literal substring of its seeded bodies, so
/// the keyword arm alone always finds the same rows whether or not an
/// embedding was ever computed — the vector arm was never proven to be
/// reached through `recall` at all. This seeds a body and queries with text
/// that share zero tokens, so keyword search (FTS5, exact-term OR-matching —
/// see `keyword.rs`'s own doc comment) must return nothing for it, while
/// `SpyEmbedder` gives the body and the query the identical constant vector
/// at both write and read time, so vector search alone can find it. A
/// recall that returns this item at all proves the embedding actually
/// reached the backend query and the vector arm actually ran.
#[tokio::test]
async fn recall_finds_an_item_via_the_vector_arm_when_keyword_search_would_find_nothing() {
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
            "zzyyxx111 wwvvuu222 ttssrr333",
        ))
        .await
        .unwrap();
    assert!(
        spy.calls() > 0,
        "the embedder must be invoked at write time"
    );

    let ws = engine
        .recall(recall_req("aabbcc444 ddeeff555 gghhii666"))
        .await
        .unwrap();

    assert!(
        !ws.items.is_empty(),
        "the item shares no keyword with the query, so it can only have been \
         found through the vector arm — recall's embedding computation was \
         never threaded through to the backend query"
    );
}

/// `read.rs` hardcodes `exclude_pending_embedding: false` on `HardFilters` —
/// a mutation to `true` survives every other test in the crate, because no
/// fixture anywhere in `tests/` recalls a memory whose embedding failed at
/// write time. `write.rs`'s own stated contract ("A missing or failed model
/// must never cost a user their memory") already has a write-side test,
/// `tests/embedding_fallback.rs::a_failed_embedding_still_admits_the_memory_marked_pending`;
/// this is its read-side counterpart. A pending-embedding item has no row
/// in the `vectors` table at all — `write.rs` only builds a vector when
/// embedding succeeded — so it can only ever be found through keyword
/// search, which is exactly what `exclude_pending_embedding: false` is
/// supposed to still allow.
#[tokio::test]
async fn recall_still_finds_a_pending_embedding_item_via_keyword_search() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spy = Arc::new(SpyEmbedder::new(8));
    spy.set_failing(true);
    let engine = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        spy.clone(),
        Arc::new(BaselinePolicy::default()),
    ));

    let outcome = engine
        .remember(RememberRequest::new(
            scope(),
            "a pendingprobe memory with no embedding at all",
        ))
        .await
        .unwrap();
    assert!(
        outcome.item_id.is_some(),
        "the write must still succeed with a failed embedder"
    );

    let ws = engine.recall(recall_req("pendingprobe")).await.unwrap();

    assert!(
        !ws.items.is_empty(),
        "a pending-embedding item must still be reachable through keyword search"
    );
}
