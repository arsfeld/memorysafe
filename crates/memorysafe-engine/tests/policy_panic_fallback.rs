//! `write.rs`'s `run_assess`/`run_admit` wrap every policy call in
//! `validate::call_policy` (`catch_unwind`) and, on failure, branch on
//! `self.stance`. `validate.rs`'s own tests exercise `call_policy` in
//! isolation; nothing before this file proved `remember()` actually routes a
//! panicking policy call through that wrapper end to end, or that
//! `FailSafe` really falls back to `fallback_policy` rather than, say,
//! propagating the panic and taking the process down with it.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    Action, AdmitContext, Assessed, Assessment, Candidate, Decision, PolicyError, PolicyId, Scope,
};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, EngineError, FailureStance, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

/// `assess` delegates to a real baseline so the pipeline can proceed; `admit`
/// always panics, the way a closed-source policy exploding mid-decision
/// would.
struct PanicsOnAdmitPolicy {
    baseline: BaselinePolicy,
}

impl memorysafe_core::GovernancePolicy for PanicsOnAdmitPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::new("panics-on-admit", "0.0.1")
    }

    fn assess(
        &self,
        cand: &Candidate,
        ctx: &memorysafe_core::AssessContext,
    ) -> Result<Assessment, PolicyError> {
        self.baseline.assess(cand, ctx)
    }

    fn admit(&self, _assessed: &Assessed, _ctx: &AdmitContext) -> Result<Decision, PolicyError> {
        panic!("the closed policy exploded during admit")
    }

    fn compose(
        &self,
        _req: &memorysafe_core::RecallRequest,
        _candidates: &[memorysafe_core::ScoredCandidate],
        _ctx: &memorysafe_core::ComposeContext,
    ) -> Result<memorysafe_core::WorkingSet, PolicyError> {
        unimplemented!("remember() never calls compose")
    }

    fn maintain(
        &self,
        _ctx: &memorysafe_core::MaintainContext,
    ) -> Result<Vec<Decision>, PolicyError> {
        unimplemented!("remember() never calls maintain")
    }
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "coding-agent").unwrap()
}

fn engine_with(stance: FailureStance) -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cfg = EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(PanicsOnAdmitPolicy {
            baseline: BaselinePolicy::default(),
        }),
    );
    cfg.stance = stance;
    Engine::new(cfg)
}

#[tokio::test]
async fn fail_safe_falls_back_to_the_fallback_policy_rather_than_propagating_the_panic() {
    let e = engine_with(FailureStance::FailSafe);
    // The panic must be caught inside `remember`, not unwind out of it and
    // take the test process down with it.
    let out = e
        .remember(RememberRequest::new(
            scope(),
            "content the rogue admit panics on",
        ))
        .await
        .expect("FailSafe must recover via the fallback policy, not error");

    // The default `fallback_policy` is `BaselinePolicy::default()`, which
    // admits novel content normally.
    assert!(matches!(out.action, Action::Retain { .. }));
    assert!(out.item_id.is_some());
}

#[tokio::test]
async fn fail_closed_surfaces_the_panic_as_a_policy_refused_error() {
    let e = engine_with(FailureStance::FailClosed);
    let err = e
        .remember(RememberRequest::new(
            scope(),
            "content the rogue admit panics on",
        ))
        .await
        .expect_err("FailClosed must refuse rather than fall back");

    match err {
        EngineError::PolicyRefused(msg) => {
            assert!(
                msg.contains("panicked") && msg.contains("exploded during admit"),
                "expected the panic message to be preserved, got: {msg}"
            );
        }
        other => panic!("expected EngineError::PolicyRefused, got {other:?}"),
    }
}
