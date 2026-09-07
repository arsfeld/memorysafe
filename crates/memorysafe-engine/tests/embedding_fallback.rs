//! `remember`'s embedding fallback (`write.rs`'s doc comment: "A missing or
//! failed model must never cost a user their memory") had no covering test:
//! `DeterministicEmbedder` never fails on non-empty input, so every other
//! test in this crate takes the `Ok` branch, and `pending_embedding` could be
//! hardcoded to either `true` or `false` with the whole suite staying green.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Action, EmbedderId, Embedding, Scope};
use memorysafe_embed::{DeterministicEmbedder, EmbedError, Embedder};
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

/// Always fails, the way an unavailable model would.
struct AlwaysFailsEmbedder;

impl Embedder for AlwaysFailsEmbedder {
    fn id(&self) -> EmbedderId {
        EmbedderId::new("always-fails")
    }
    fn dim(&self) -> u16 {
        256
    }
    fn embed(&self, _text: &str) -> Result<Embedding, EmbedError> {
        Err(EmbedError::Unavailable(
            "model deliberately offline for this test".into(),
        ))
    }
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "coding-agent").unwrap()
}

#[tokio::test]
async fn a_failed_embedding_still_admits_the_memory_marked_pending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let e = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(AlwaysFailsEmbedder),
        Arc::new(BaselinePolicy::default()),
    ));

    let out = e
        .remember(RememberRequest::new(
            scope(),
            "content the embedder cannot vectorise",
        ))
        .await
        .expect("an embedder failure must not cost the user their memory");

    assert!(matches!(out.action, Action::Retain { .. }));
    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert!(
        stored[0].pending_embedding,
        "an item written with no embedding must be marked pending_embedding"
    );
}

#[tokio::test]
async fn a_successful_embedding_is_not_marked_pending() {
    // The complementary case: nothing above forces `pending_embedding` to be
    // computed from the real embedding result rather than hardcoded `true`.
    let dir = tempfile::tempdir().expect("tempdir");
    let e = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ));

    e.remember(RememberRequest::new(
        scope(),
        "content the embedder vectorises fine",
    ))
    .await
    .unwrap();

    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert!(
        !stored[0].pending_embedding,
        "a successfully embedded item must not be marked pending"
    );
}
