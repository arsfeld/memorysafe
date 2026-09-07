//! `write.rs`'s `run_assess`/`run_admit` have `policy_panic_fallback.rs`
//! proving `remember()` routes a panicking policy call through
//! `validate::call_policy` and branches correctly on `self.stance`. Nothing
//! before this file proved the same for `recall()`'s `compose` call: the
//! mandated `tests/read.rs` suite only ever plugs in `BaselinePolicy`, which
//! never panics or errors, so the `FailClosed`/`FailSafe` branch in
//! `read.rs` around `validate::call_policy(call)` is never exercised.
//! Confirmed by mutation testing — collapsing that whole match arm to always
//! fall back (or always fail closed) survives every test in
//! `tests/read.rs` and `tests/policy_panic_fallback.rs` alike, since the
//! latter only reaches `admit`, never `compose`.

use memorysafe_backend::Backend;
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    AdmitContext, Assessed, Assessment, Candidate, ComposeContext, Decision, MaintainContext,
    PolicyError, PolicyId, RecallBudget, RecallMode, RecallRequest, Scope, ScoredCandidate,
    SensitivityLevel, WorkingSet,
};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, EngineError, FailureStance, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

/// `compose` always panics, the way a closed-source policy exploding mid
/// composition would. The other three methods are never reached by
/// `recall()`, so they simply say so.
struct PanicsOnComposePolicy;

impl memorysafe_core::GovernancePolicy for PanicsOnComposePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::new("panics-on-compose", "0.0.1")
    }

    fn assess(
        &self,
        _cand: &Candidate,
        _ctx: &memorysafe_core::AssessContext,
    ) -> Result<Assessment, PolicyError> {
        unimplemented!("recall() never calls assess")
    }

    fn admit(&self, _assessed: &Assessed, _ctx: &AdmitContext) -> Result<Decision, PolicyError> {
        unimplemented!("recall() never calls admit")
    }

    fn compose(
        &self,
        _req: &RecallRequest,
        _candidates: &[ScoredCandidate],
        _ctx: &ComposeContext,
    ) -> Result<WorkingSet, PolicyError> {
        panic!("the closed compose policy exploded")
    }

    fn maintain(&self, _ctx: &MaintainContext) -> Result<Vec<Decision>, PolicyError> {
        unimplemented!("recall() never calls maintain")
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

/// A well-behaved `BaselinePolicy` writer and a `PanicsOnComposePolicy`
/// reader sharing one backend — `remember()` needs a policy that can
/// actually admit content, and `PanicsOnComposePolicy::admit` is
/// deliberately `unimplemented!()`, so seeding through it would panic for an
/// unrelated reason before `recall()` is ever reached.
fn engines(stance: FailureStance) -> (Engine, Engine) {
    let dir = tempfile::tempdir().expect("tempdir");
    let backend: Arc<dyn Backend> = Arc::new(SqliteBackend::open(dir.keep()));
    let embedder = Arc::new(DeterministicEmbedder::new(256));
    let writer = Engine::new(EngineConfig::new(
        backend.clone(),
        embedder.clone(),
        Arc::new(BaselinePolicy::default()),
    ));
    let mut cfg = EngineConfig::new(backend, embedder, Arc::new(PanicsOnComposePolicy));
    cfg.stance = stance;
    let reader = Engine::new(cfg);
    (writer, reader)
}

#[tokio::test]
async fn fail_safe_falls_back_to_the_fallback_policy_rather_than_propagating_the_panic() {
    let (writer, reader) = engines(FailureStance::FailSafe);
    writer
        .remember(RememberRequest::new(scope(), "a genuine memory about cats"))
        .await
        .unwrap();

    // The panic must be caught inside `recall`, not unwind out of it and take
    // the test process down with it. The default `fallback_policy` is
    // `BaselinePolicy::default()`, which composes normally.
    let ws = reader
        .recall(recall_req("cats"))
        .await
        .expect("FailSafe must recover via the fallback policy, not error");
    assert!(!ws.items.is_empty());
    assert!(
        ws.audit_id.is_some(),
        "a fallback-composed recall must still be audited"
    );
}

#[tokio::test]
async fn fail_closed_surfaces_the_panic_as_a_policy_refused_error() {
    let (writer, reader) = engines(FailureStance::FailClosed);
    writer
        .remember(RememberRequest::new(scope(), "a genuine memory about cats"))
        .await
        .unwrap();

    let err = reader
        .recall(recall_req("cats"))
        .await
        .expect_err("FailClosed must refuse rather than fall back");
    match err {
        EngineError::PolicyRefused(msg) => {
            assert!(
                msg.contains("panicked") && msg.contains("exploded"),
                "expected the panic message to be preserved, got: {msg}"
            );
        }
        other => panic!("expected EngineError::PolicyRefused, got {other:?}"),
    }
}
