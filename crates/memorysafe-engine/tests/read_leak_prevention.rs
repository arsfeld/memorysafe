//! `validate::working_set` is the read path's security boundary: the
//! backend's hard filters narrow the candidate set in SQL, and a policy may
//! only narrow it further, never widen it. `tests/read.rs` plugs in only
//! `BaselinePolicy`, which always clones the offered candidate's own item
//! unmodified into `SelectedItem` — so nothing in that suite can tell a
//! `recall()` with the `validate::working_set` check wired in from one with
//! it deleted. Confirmed by mutation testing: deleting the
//! `if let Err(invalid) = validate::working_set(&composed, &candidates) { .. }`
//! block in `read.rs` survives every test in `tests/read.rs`. This file
//! plugs a rogue policy in as the primary policy and proves the engine
//! actually catches it — the exact scenario Task 32's brief called "what
//! stops a policy bug from becoming a data leak."

use memorysafe_backend::Backend;
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    AdmitContext, Assessed, Assessment, AuditEvent, AuditFilter, Candidate, ComposeContext,
    Decision, ItemId, MaintainContext, MemoryItem, PolicyError, PolicyId, Protection, Reason,
    ReasonCode, RecallBudget, RecallMode, RecallRequest, Scope, ScoredCandidate, SelectedItem,
    SensitivityLevel, Source, SourceKind, WorkingSet, features,
};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, EngineError, FailureStance, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

/// A memory the backend's `retrieve_candidates` never returned — the shape a
/// leak takes.
fn smuggled_item() -> MemoryItem {
    MemoryItem {
        id: ItemId::new(),
        scope: scope(),
        body: "a memory the backend never offered to the policy".into(),
        kind: "fact".into(),
        source: Source {
            kind: SourceKind::Agent,
            id: None,
        },
        occurred_at: None,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        tags: vec![],
        attrs: Default::default(),
        sensitivity: SensitivityLevel::Public,
        ttl: None,
        protection: Protection::Normal,
        pending_embedding: false,
    }
}

/// `compose` ignores whatever candidates it is actually handed and returns a
/// working set naming an item that was never among them — the shape of a
/// policy bug, or a compromised/misbehaving third-party policy, that would
/// leak data past the backend's hard filters if nothing checked the result.
struct SmugglesAnUnofferedItem;

impl memorysafe_core::GovernancePolicy for SmugglesAnUnofferedItem {
    fn id(&self) -> PolicyId {
        PolicyId::new("smuggler", "0.0.1")
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
        Ok(WorkingSet {
            items: vec![SelectedItem {
                item: smuggled_item(),
                relevance: 1.0,
                reason: Reason::new(
                    ReasonCode::HighValue,
                    "fabricated by a rogue policy",
                    features! {},
                ),
            }],
            tokens_used: 0,
            omitted: vec![],
            omitted_total: 0,
            audit_id: None,
        })
    }

    fn maintain(&self, _ctx: &MaintainContext) -> Result<Vec<Decision>, PolicyError> {
        unimplemented!("recall() never calls maintain")
    }
}

/// A separate rogue policy for the `omitted`-side check: `compose` reports a
/// real-looking `OmittedItem` whose id was never offered either.
struct SmugglesAnUnofferedOmission;

impl memorysafe_core::GovernancePolicy for SmugglesAnUnofferedOmission {
    fn id(&self) -> PolicyId {
        PolicyId::new("omission-smuggler", "0.0.1")
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
        Ok(WorkingSet {
            items: vec![],
            tokens_used: 0,
            omitted: vec![memorysafe_core::OmittedItem {
                id: ItemId::new(),
                reason: Reason::new(ReasonCode::BudgetExhausted, "fabricated", features! {}),
            }],
            omitted_total: 1,
            audit_id: None,
        })
    }

    fn maintain(&self, _ctx: &MaintainContext) -> Result<Vec<Decision>, PolicyError> {
        unimplemented!("recall() never calls maintain")
    }
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

/// A well-behaved `BaselinePolicy` writer and a rogue reader sharing one
/// backend, for the same reason `read_policy_failure.rs` needs the split:
/// the rogue policies' `admit` is `unimplemented!()`.
fn engines_with_reader(
    reader_policy: Arc<dyn memorysafe_core::GovernancePolicy>,
) -> (Engine, Engine) {
    let dir = tempfile::tempdir().expect("tempdir");
    let backend: Arc<dyn Backend> = Arc::new(SqliteBackend::open(dir.keep()));
    let embedder = Arc::new(DeterministicEmbedder::new(256));
    let writer = Engine::new(EngineConfig::new(
        backend.clone(),
        embedder.clone(),
        Arc::new(BaselinePolicy::default()),
    ));
    let reader = Engine::new(EngineConfig::new(backend, embedder, reader_policy));
    (writer, reader)
}

#[tokio::test]
async fn a_policy_smuggling_an_unoffered_item_is_refused_not_returned() {
    let (writer, reader) = engines_with_reader(Arc::new(SmugglesAnUnofferedItem));
    writer
        .remember(RememberRequest::new(scope(), "a genuine memory about cats"))
        .await
        .unwrap();

    let err = reader
        .recall(recall_req("cats"))
        .await
        .expect_err("a smuggled item must be refused, not returned to the caller");
    match err {
        EngineError::PolicyRefused(msg) => {
            assert!(
                msg.contains("never offered"),
                "expected the unoffered-item message, got: {msg}"
            );
        }
        other => panic!("expected EngineError::PolicyRefused, got {other:?}"),
    }

    // A policy caught trying to leak is a security event; it must leave an
    // audit trail even though nothing was returned to the caller. Before
    // this test, refusing here wrote no audit row at all — the same
    // `AuditRecord` fields `record_recall` is given for a successful
    // `Recalled` row, just under `AuditEvent::Rejected`, the way
    // `write.rs`'s `handle_invalid_decision` audits an invalid write
    // decision.
    let audit = reader
        .audit(
            &scope(),
            &AuditFilter {
                events: vec![AuditEvent::Rejected],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        audit.len(),
        1,
        "a refused recall must leave exactly one Rejected audit row"
    );
    assert!(
        audit[0].items.is_empty(),
        "the refusal's audit row must not name the smuggled item"
    );
}

#[tokio::test]
async fn a_policys_omitted_list_naming_an_unoffered_item_is_also_refused() {
    let (writer, reader) = engines_with_reader(Arc::new(SmugglesAnUnofferedOmission));
    writer
        .remember(RememberRequest::new(scope(), "a genuine memory about cats"))
        .await
        .unwrap();

    let err = reader
        .recall(recall_req("cats"))
        .await
        .expect_err("an unoffered id in `omitted` must be refused too, not just `items`");
    match err {
        EngineError::PolicyRefused(msg) => {
            assert!(
                msg.contains("never offered"),
                "expected the unoffered-item message, got: {msg}"
            );
        }
        other => panic!("expected EngineError::PolicyRefused, got {other:?}"),
    }
}

/// `read.rs`'s own doc comment on this check states it fails closed
/// unconditionally, regardless of `self.stance` — unlike `remember`'s
/// analogous `handle_invalid_decision`, which does branch on it. Every test
/// above builds its reader through `EngineConfig::new`, whose stance
/// defaults to `FailSafe`, so they already exercise that claim implicitly;
/// this test sets the stance explicitly so the guarantee does not quietly
/// depend on what that default happens to be. `FailSafe` must not turn a
/// smuggled item into a substituted-but-safe-looking working set — it must
/// still refuse, exactly as `FailClosed` would.
#[tokio::test]
async fn a_smuggled_item_is_refused_even_under_the_failsafe_stance() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backend: Arc<dyn Backend> = Arc::new(SqliteBackend::open(dir.keep()));
    let embedder = Arc::new(DeterministicEmbedder::new(256));
    let writer = Engine::new(EngineConfig::new(
        backend.clone(),
        embedder.clone(),
        Arc::new(BaselinePolicy::default()),
    ));
    let mut reader_config = EngineConfig::new(backend, embedder, Arc::new(SmugglesAnUnofferedItem));
    reader_config.stance = FailureStance::FailSafe;
    let reader = Engine::new(reader_config);

    writer
        .remember(RememberRequest::new(scope(), "a genuine memory about cats"))
        .await
        .unwrap();

    let err = reader.recall(recall_req("cats")).await.expect_err(
        "FailSafe must not change this outcome: a smuggled item is refused regardless of stance",
    );
    assert!(matches!(err, EngineError::PolicyRefused(_)));
}
