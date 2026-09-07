//! `Engine::recall`'s guard against a queryless call
//! (`if embedding.is_none() && req.query....is_empty() { return Err(...) }`)
//! is untested by `tests/read.rs`: every mandated fixture supplies a
//! non-empty query, so `embedding` is always `Some` there and the guard never
//! fires. Confirmed by mutation testing — deleting the guard, or flipping its
//! `&&` to `||`, survives every test in `tests/read.rs`. This file plugs the
//! gap directly.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{RecallBudget, RecallMode, RecallRequest, Scope, SensitivityLevel};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, EngineError};
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
    Scope::new("acme", "user-42", "agent").unwrap()
}

fn req_with_query(query: Option<&str>) -> RecallRequest {
    RecallRequest {
        scope: scope(),
        query: query.map(|q| q.to_string()),
        tags_any: vec![],
        kinds: vec![],
        occurred_after: None,
        occurred_before: None,
        mode: RecallMode::WorkingSet,
        budget: RecallBudget::default(),
        sensitivity_ceiling: SensitivityLevel::Restricted,
    }
}

#[tokio::test]
async fn a_recall_with_no_query_is_a_validation_error() {
    let e = engine();
    let err = e
        .recall(req_with_query(None))
        .await
        .expect_err("a queryless recall must be rejected, not silently answered");
    assert!(
        matches!(err, EngineError::Validation(_)),
        "expected EngineError::Validation, got {err:?}"
    );
}

#[tokio::test]
async fn a_recall_with_a_whitespace_only_query_is_a_validation_error() {
    let e = engine();
    let err = e
        .recall(req_with_query(Some("   ")))
        .await
        .expect_err("a whitespace-only query carries no real query signal");
    assert!(
        matches!(err, EngineError::Validation(_)),
        "expected EngineError::Validation, got {err:?}"
    );
}
