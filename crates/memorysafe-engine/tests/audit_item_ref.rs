//! `remember`'s `refs` construction (`write.rs`: `item.as_ref().map(|i|
//! vec![ItemRef::from_item(i)]).unwrap_or_default()`) had no covering test:
//! nothing checked `AuditRecord::items` actually names the item the audit row
//! is about. Hardcoding an empty `Vec<ItemRef>` survived the whole suite —
//! which would sever every audit row from the item it describes, defeating
//! the audit trail's purpose ("what happened to which item") without ever
//! failing a test.

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
async fn a_retained_writes_audit_row_names_the_item_it_created() {
    let e = engine();
    let out = e
        .remember(RememberRequest::new(
            scope(),
            "an item the audit row must name",
        ))
        .await
        .unwrap();
    let item_id = out
        .item_id
        .expect("a retained write always creates an item");

    let mut audit = e.audit(&scope(), &Default::default()).await.unwrap();
    let newest = audit.remove(0);
    assert_eq!(
        newest.items.len(),
        1,
        "the audit row for a retained write must reference exactly the one item"
    );
    assert_eq!(*newest.items[0].id(), item_id);
}
