//! `remember`'s `MAX_BODY_BYTES` guard (`write.rs`) had no covering test
//! anywhere in the suite until this file: mutation testing found it could be
//! deleted wholesale with every other test in the crate staying green, since
//! none of them ever construct a body anywhere near 64KiB.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::Scope;
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, EngineError, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

fn engine() -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ))
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "coding-agent").unwrap()
}

#[tokio::test]
async fn a_body_over_the_byte_limit_is_a_validation_error_not_a_stored_memory() {
    let e = engine();
    // One byte over 64 * 1024.
    let oversized = "a".repeat(64 * 1024 + 1);
    let err = e
        .remember(RememberRequest::new(scope(), &oversized))
        .await
        .expect_err("an oversized body must be rejected");
    assert!(
        matches!(err, EngineError::Validation(ref msg) if msg.contains("exceeds") && msg.contains("65536")),
        "expected a Validation error naming the limit, got {err:?}"
    );
    assert!(
        e.review(&scope(), &Default::default())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_body_at_exactly_the_byte_limit_is_accepted() {
    // The boundary itself: `len() > MAX_BODY_BYTES` must not reject an equal
    // length, only a greater one — an off-by-one in either direction is
    // otherwise invisible.
    let e = engine();
    let at_limit = "a".repeat(64 * 1024);
    let out = e
        .remember(RememberRequest::new(scope(), &at_limit))
        .await
        .expect("a body at exactly the limit must be accepted");
    assert!(out.item_id.is_some());
}
