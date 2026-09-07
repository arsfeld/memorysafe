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

// Both tests below assert the guard's own wording
// (`"filter-only recall"`), not just the `EngineError::Validation` variant.
// Since fix round 1 of Task 7 (`memorysafe-engine/src/error.rs`'s
// `From<BackendError> for EngineError`), `BackendError::InvalidQuery` — which
// `CandidateQuery::is_valid()` also raises, further down in
// `SqliteBackend::retrieve_candidates`, for the exact same "no embedding, no
// text" condition — maps to `EngineError::Validation` too. A variant-only
// assertion can no longer tell "the engine's own pre-backend guard caught
// this" from "the guard was deleted and the backend's own validity check
// caught it instead, one layer down" — both now produce the same
// `EngineError` variant. This file's whole reason to exist (its own module
// doc: deleting the guard "survives every test in `tests/read.rs`") depends
// on that distinction staying visible, so the message text is the only
// remaining way to prove specifically the ENGINE's guard fired.
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
    assert!(
        err.to_string().contains("filter-only recall"),
        "expected the engine's own pre-backend guard message, got: {err}"
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
    assert!(
        err.to_string().contains("filter-only recall"),
        "expected the engine's own pre-backend guard message, got: {err}"
    );
}
