//! `validate::decision` is unit-tested thoroughly in `memorysafe-engine`'s own
//! `validate` module, but nothing before this file proved `remember()` in
//! `write.rs` actually *calls* it. `BaselinePolicy` — the only policy every
//! other integration test in this crate uses — never returns a decision that
//! fails validation, so deleting the `if let Err(invalid) =
//! validate::decision(...)` guard from `remember()` wholesale left **every**
//! test in `tests/write.rs` and in `tests/merge_target_check.rs` green — the
//! whole of both files, however many each holds today. (An earlier version of
//! this sentence counted them, and the count went stale as soon as a test was
//! added; the property is "all of them", and the number was never the point.)
//! That is a genuine gap, not an equivalent mutant: without the guard,
//! a policy returning a `Decision` with no reason at all is silently written
//! and audited rather than refused — `WriteTransaction::is_valid` has no
//! opinion on `Decision::reasons`, so nothing else in the pipeline would have
//! caught it. These tests pin the guard by proving the write is refused, not
//! merely erroring, in both failure stances.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    Action, AdmitContext, Assessed, Assessment, Candidate, Decision, PolicyError, PolicyId,
    Protection, ReasonCode, Scope,
};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, FailureStance, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

/// Always retains with no reason at all — a decision `validate::decision`
/// must refuse (`Invalid::NoReason`).
struct NoReasonPolicy {
    baseline: BaselinePolicy,
}

impl memorysafe_core::GovernancePolicy for NoReasonPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::new("rogue-no-reason", "0.0.1")
    }

    fn assess(
        &self,
        cand: &Candidate,
        ctx: &memorysafe_core::AssessContext,
    ) -> Result<Assessment, PolicyError> {
        self.baseline.assess(cand, ctx)
    }

    fn admit(&self, _assessed: &Assessed, _ctx: &AdmitContext) -> Result<Decision, PolicyError> {
        Ok(Decision {
            subject: None,
            action: Action::Retain {
                protection: Protection::Normal,
            },
            evictions: vec![],
            reasons: vec![],
            policy: self.id(),
        })
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

fn engine_with(policy: NoReasonPolicy, stance: FailureStance) -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cfg = EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(policy),
    );
    cfg.stance = stance;
    Engine::new(cfg)
}

#[tokio::test]
async fn fail_safe_a_reasonless_decision_is_rejected_and_nothing_is_written() {
    let policy = NoReasonPolicy {
        baseline: BaselinePolicy::default(),
    };
    let e = engine_with(policy, FailureStance::FailSafe);
    let out = e
        .remember(RememberRequest::new(
            scope(),
            "content a rogue policy mishandles",
        ))
        .await
        .expect("FailSafe must not surface this as an error");

    assert!(
        matches!(out.action, Action::Reject),
        "expected Reject, got {:?}",
        out.action
    );
    assert!(
        out.reasons
            .iter()
            .any(|r| r.code == ReasonCode::PolicyInvalid),
        "expected a PolicyInvalid reason, got {:?}",
        out.reasons
    );

    // The write must never have happened: a decision with no reason is not a
    // legitimate governance outcome, so nothing should be stored under it.
    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert!(
        stored.is_empty(),
        "a reasonless decision must not create any row, found {stored:?}"
    );
}

#[tokio::test]
async fn fail_closed_a_reasonless_decision_is_a_policy_refused_error() {
    let policy = NoReasonPolicy {
        baseline: BaselinePolicy::default(),
    };
    let e = engine_with(policy, FailureStance::FailClosed);
    let err = e
        .remember(RememberRequest::new(
            scope(),
            "content a rogue policy mishandles",
        ))
        .await
        .expect_err("FailClosed must surface this as an error");

    match err {
        memorysafe_engine::EngineError::PolicyRefused(msg) => {
            assert!(
                msg.contains("no reason"),
                "expected the no-reason message, got: {msg}"
            );
        }
        other => panic!("expected EngineError::PolicyRefused, got {other:?}"),
    }
}
