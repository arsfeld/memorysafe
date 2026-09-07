//! `WriteOutcome::audit_id` (`write.rs`'s `remember`) had no covering test:
//! nothing checked it names a row that actually exists in the audit trail.
//! Substituting a freshly-minted, unrelated `AuditId` survived the whole
//! suite — a caller using the returned id to look up "what happened and why"
//! (the audit trail's whole purpose) would get nothing back.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::Scope;
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

#[tokio::test]
async fn the_returned_audit_id_names_a_real_row_in_the_trail() {
    let e = engine();
    let out = e
        .remember(RememberRequest::new(
            scope(),
            "a memory whose audit id must be real",
        ))
        .await
        .unwrap();

    let audit = e.audit(&scope(), &Default::default()).await.unwrap();
    assert!(
        audit.iter().any(|r| r.id == out.audit_id),
        "WriteOutcome::audit_id must name a row that actually exists in the trail; \
         got {:?}, trail has {:?}",
        out.audit_id,
        audit.iter().map(|r| &r.id).collect::<Vec<_>>()
    );
}
