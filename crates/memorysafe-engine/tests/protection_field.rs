//! `remember`'s `protection: *protection` (`write.rs`, inside the
//! `Action::Retain { protection }` arm) had no covering test: every other
//! test in the crate either does not inspect the stored item's `protection`
//! field, or happens to land on `Protection::Normal` — hardcoding `Normal`
//! there survived the whole suite.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Protection, Scope};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

fn scope() -> Scope {
    Scope::new("acme", "user-42", "coding-agent").unwrap()
}

#[tokio::test]
async fn a_maximally_fragile_first_write_is_stored_with_the_decisions_protection_window() {
    // The very first write into an empty scope has no neighbours, so
    // `BaselinePolicy` scores it maximally fragile and returns
    // `Action::Retain { protection: Protection::Protected { until } }`
    // (see `fragility::score`'s "no neighbours" branch and `admit::decide`).
    // The stored item must carry that same window, not a hardcoded `Normal`.
    let dir = tempfile::tempdir().expect("tempdir");
    let e = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ));

    let out = e
        .remember(RememberRequest::new(
            scope(),
            "the first thing anyone ever told it",
        ))
        .await
        .unwrap();
    assert!(
        matches!(
            out.action,
            memorysafe_core::Action::Retain {
                protection: Protection::Protected { .. }
            }
        ),
        "expected a Protected outcome for a maximally fragile first write, got {:?}",
        out.action
    );

    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert!(
        matches!(stored[0].protection, Protection::Protected { .. }),
        "the stored item must carry the decision's protection window, got {:?}",
        stored[0].protection
    );
}
