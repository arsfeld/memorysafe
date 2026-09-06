//! `remember`'s `MergeWrite` construction (`write.rs`, the
//! `Action::Merge { into, .. }` arm) had no test checking the merged item's
//! resulting *content* — `tests/write.rs`'s merge test only checks
//! `second.merged_into` and the reason code, never the stored body. Setting
//! `body: String::new()` instead of `req.body.clone()` survived the whole
//! suite. This pins that the target's body is actually replaced with the
//! new content, not dropped, ignored, or left as the old body.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Action, Scope};
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
async fn a_merged_items_stored_body_reflects_the_new_content() {
    let e = engine();
    let first = e
        .remember(RememberRequest::new(scope(), "the cat sat on the mat"))
        .await
        .unwrap();
    let second = e
        .remember(RememberRequest::new(
            scope(),
            "the cat sat on the mat today",
        ))
        .await
        .unwrap();

    // This particular pair reliably merges under `DeterministicEmbedder` +
    // `BaselinePolicy`'s default thresholds — verified by running it, not
    // assumed. If that ever changes, this test's premise breaks loudly here
    // rather than silently passing on the wrong branch.
    assert!(
        matches!(second.action, Action::Merge { .. }),
        "expected this pair to merge, got {:?}",
        second.action
    );

    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1, "the merge target survives, no second row");
    assert_eq!(stored[0].id, first.item_id.unwrap());
    assert_eq!(
        stored[0].body, "the cat sat on the mat today",
        "the merge target's body must reflect the new write's content"
    );
}
