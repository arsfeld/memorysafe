//! `remember`'s `sensitivity: assessment.sensitivity.level.raised_by(req.sensitivity_hint)`
//! (`write.rs`) is the engine's OWN enforcement of the caller's sensitivity
//! floor — deliberately not trusted from the policy (`write.rs`'s own
//! comment: "A closed scorer that forgot to call `raised_by` would silently
//! downgrade a caller's declared `Restricted`"). `BaselinePolicy` happens to
//! apply the same hint internally (`sensitivity::assess` folds
//! `cand.sensitivity_hint` in on its own), so a test built only against
//! `BaselinePolicy` cannot tell "the engine enforces this" from "the policy
//! already did, and the engine's own call is a no-op" — deleting the
//! engine's `.raised_by(...)` call left every other test in this crate
//! green. `NaiveSensitivityPolicy` below deliberately does NOT apply the
//! hint in its own `assess`, isolating the engine's enforcement from the
//! policy's redundant one.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    Action, AdmitContext, Assessed, Assessment, AssessorId, Candidate, Decision, PolicyError,
    PolicyId, Protection, Reason, ReasonCode, RedundancyAssessment, Scope, Score,
    SensitivityAssessment, SensitivityLevel, features,
};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

/// Always assesses `Internal` sensitivity, ignoring `cand.sensitivity_hint`
/// entirely — the shape of a closed-source scorer that forgot the hint
/// exists.
struct NaiveSensitivityPolicy;

impl memorysafe_core::GovernancePolicy for NaiveSensitivityPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::new("naive-sensitivity", "0.0.1")
    }

    fn assess(
        &self,
        _cand: &Candidate,
        _ctx: &memorysafe_core::AssessContext,
    ) -> Result<Assessment, PolicyError> {
        Ok(Assessment {
            value: Score::clamped(0.5),
            fragility: Score::clamped(0.5),
            sensitivity: SensitivityAssessment {
                level: SensitivityLevel::Internal,
                categories: vec![],
                confidence: Score::clamped(1.0),
            },
            redundancy: RedundancyAssessment {
                score: Score::clamped(0.0),
                near_duplicates: vec![],
            },
            features: features! {},
            assessor: AssessorId::new("naive-sensitivity", "0.0.1"),
        })
    }

    fn admit(&self, _assessed: &Assessed, _ctx: &AdmitContext) -> Result<Decision, PolicyError> {
        Ok(Decision {
            subject: None,
            action: Action::Retain {
                protection: Protection::Normal,
            },
            evictions: vec![],
            reasons: vec![Reason::new(ReasonCode::NovelContent, "novel", features! {})],
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

#[tokio::test]
async fn the_engine_raises_sensitivity_even_when_the_policy_does_not() {
    let dir = tempfile::tempdir().expect("tempdir");
    let e = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(NaiveSensitivityPolicy),
    ));

    let mut req = RememberRequest::new(scope(), "the team meets every Tuesday at noon");
    req.sensitivity_hint = Some(SensitivityLevel::Restricted);
    e.remember(req).await.unwrap();

    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].sensitivity,
        SensitivityLevel::Restricted,
        "the engine must enforce the caller's hint even when the policy ignores it"
    );
}

#[tokio::test]
async fn a_callers_restricted_hint_raises_ordinary_content_to_restricted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let e = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ));
    let mut req = RememberRequest::new(scope(), "the team meets every Tuesday at noon");
    req.sensitivity_hint = Some(SensitivityLevel::Restricted);

    e.remember(req).await.unwrap();

    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].sensitivity, SensitivityLevel::Restricted);
}

#[tokio::test]
async fn a_low_hint_never_lowers_a_detected_sensitivity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let e = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ));
    let mut req = RememberRequest::new(scope(), "the api key is sk-abc123def456ghi789jkl012");
    req.sensitivity_hint = Some(SensitivityLevel::Public);

    e.remember(req).await.unwrap();

    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].sensitivity,
        SensitivityLevel::Restricted,
        "a low hint must not downgrade a detected credential"
    );
}
