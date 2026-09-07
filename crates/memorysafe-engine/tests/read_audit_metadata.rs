//! `Engine::recall` stamps every `Recalled` audit row's `actor` as
//! `Actor { kind: ActorKind::Agent, id: None }`. Nothing in `tests/read.rs`
//! inspects `actor` at all (mandated test 6 checks only `items`), so
//! mutating that field to a different `ActorKind` survives every test there
//! — confirmed by mutation testing.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    ActorKind, AuditEvent, AuditFilter, RecallBudget, RecallMode, RecallRequest, Scope,
    SensitivityLevel,
};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
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

#[tokio::test]
async fn a_recall_audit_record_is_stamped_with_the_agent_actor_kind() {
    let e = engine();
    e.remember(RememberRequest::new(scope(), "a memory about cats"))
        .await
        .unwrap();

    let req = RecallRequest {
        scope: scope(),
        query: Some("cats".into()),
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
    };
    e.recall(req).await.unwrap();

    let audit = e
        .audit(
            &scope(),
            &AuditFilter {
                events: vec![AuditEvent::Recalled],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(audit.len(), 1);
    assert_eq!(
        audit[0].actor.kind,
        ActorKind::Agent,
        "a recall audit row must be stamped with the agent actor kind"
    );
    assert!(audit[0].actor.id.is_none());
}
