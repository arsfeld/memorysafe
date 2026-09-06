//! `remember`'s `event` match (`write.rs`, mapping `decision.action` to an
//! `AuditEvent`) had no covering test: nothing in the suite ever inspected
//! `AuditRecord::event`. Hardcoding `AuditEvent::Admitted` unconditionally
//! survived every other test in the crate, including the merge and rejection
//! ones — they check the `WriteOutcome`'s `action`, never the audit record's
//! `event`.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Action, AuditEvent, Scope};
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
    Scope::new("acme", "user-42", "coding-agent").unwrap()
}

async fn last_event(e: &Engine) -> AuditEvent {
    let mut audit = e.audit(&scope(), &Default::default()).await.unwrap();
    // Newest first (`Backend::audit`'s own ordering contract).
    audit.remove(0).event
}

#[tokio::test]
async fn a_retained_write_is_audited_as_admitted() {
    let e = engine();
    e.remember(RememberRequest::new(
        scope(),
        "first ever memory in this scope",
    ))
    .await
    .unwrap();
    assert_eq!(last_event(&e).await, AuditEvent::Admitted);
}

#[tokio::test]
async fn a_rejected_write_is_audited_as_rejected() {
    let e = engine();
    let body = "the deploy key rotates every ninety days";
    e.remember(RememberRequest::new(scope(), body))
        .await
        .unwrap();
    let second = e
        .remember(RememberRequest::new(scope(), body))
        .await
        .unwrap();
    assert!(matches!(second.action, Action::Reject));
    assert_eq!(last_event(&e).await, AuditEvent::Rejected);
}

#[tokio::test]
async fn a_merged_write_is_audited_as_merged() {
    let e = engine();
    e.remember(RememberRequest::new(scope(), "the cat sat on the mat"))
        .await
        .unwrap();
    let second = e
        .remember(RememberRequest::new(
            scope(),
            "the cat sat on the mat today",
        ))
        .await
        .unwrap();
    assert!(
        matches!(second.action, Action::Merge { .. }),
        "expected a merge, got {:?}",
        second.action
    );
    assert_eq!(last_event(&e).await, AuditEvent::Merged);
}
