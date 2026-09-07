//! Task 33's added requirement (see the task report's provenance note):
//! `AdmitContext` carries no set of existing items, so `validate::decision`
//! structurally cannot check `Action::Merge { into, .. }` against anything.
//! The engine must refuse to build a `MergeWrite` naming a target that does
//! not exist, or that exists outside the request's scope, rather than
//! silently dropping the write or panicking.
//!
//! `SqliteBackend`'s own `merge` also refuses a missing/out-of-scope target
//! (`BackendError::MergeTargetMissing`), so a naive "does `remember` return
//! an error" test cannot tell "the engine's own check caught this" from "the
//! backend's own guard caught it after the engine tried to write it anyway."
//! These tests assert the *shape* the engine's own check produces — the
//! standard invalid-decision handling (`Action::Reject` with
//! `ReasonCode::PolicyInvalid` under `FailSafe`, `EngineError::PolicyRefused`
//! under `FailClosed`) — which only the engine-level check, not the
//! backend's, can produce.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    Action, AdmitContext, Assessed, Assessment, Candidate, Decision, MergeStrategy, PolicyError,
    PolicyId, Reason, ReasonCode, Scope, features,
};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, FailureStance, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

/// Always decides to merge into a freshly-minted id that was never written
/// anywhere — the shape a rogue or buggy policy produces.
struct MergeIntoNothingPolicy {
    baseline: BaselinePolicy,
}

impl memorysafe_core::GovernancePolicy for MergeIntoNothingPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::new("rogue-merge", "0.0.1")
    }

    fn assess(
        &self,
        cand: &Candidate,
        ctx: &memorysafe_core::AssessContext,
    ) -> Result<Assessment, PolicyError> {
        // Delegate: this test is about the merge-target check, not assessment.
        self.baseline.assess(cand, ctx)
    }

    fn admit(&self, _assessed: &Assessed, _ctx: &AdmitContext) -> Result<Decision, PolicyError> {
        Ok(Decision {
            subject: None,
            action: Action::Merge {
                into: memorysafe_core::ItemId::new(),
                strategy: MergeStrategy::AppendAndUnion,
            },
            evictions: vec![],
            reasons: vec![Reason::new(
                ReasonCode::HighRedundancy,
                "bogus merge target",
                features! {},
            )],
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

fn engine_with(policy: MergeIntoNothingPolicy, stance: FailureStance) -> Engine {
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
async fn fail_safe_a_merge_into_a_nonexistent_target_is_rejected_not_written() {
    let policy = MergeIntoNothingPolicy {
        baseline: BaselinePolicy::default(),
    };
    let e = engine_with(policy, FailureStance::FailSafe);
    let out = e
        .remember(RememberRequest::new(scope(), "some content to merge"))
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
    assert!(out.item_id.is_none());

    // Nothing was written: the scope stays empty.
    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert!(stored.is_empty(), "a bogus merge must not create any row");
}

#[tokio::test]
async fn fail_closed_a_merge_into_a_nonexistent_target_is_a_policy_refused_error() {
    let policy = MergeIntoNothingPolicy {
        baseline: BaselinePolicy::default(),
    };
    let e = engine_with(policy, FailureStance::FailClosed);
    let err = e
        .remember(RememberRequest::new(scope(), "some content to merge"))
        .await
        .expect_err("FailClosed must surface this as an error");

    match err {
        memorysafe_engine::EngineError::PolicyRefused(msg) => {
            assert!(
                msg.contains("does not exist"),
                "expected the merge-target-missing message, got: {msg}"
            );
        }
        other => panic!("expected EngineError::PolicyRefused, got {other:?}"),
    }
}
