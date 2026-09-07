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
use time::Duration;

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

/// Final review, finding I1: `until` (`admit::decide`, from `AdmitContext.now`)
/// and `created_at` (`write.rs`) used to be two independently-sampled clock
/// reads, so `until - created_at` equalled `Duration::days(N)` only when both
/// samples landed in the same wall-clock second — off by exactly one second
/// whenever they straddled a boundary. The natural gap between the two reads
/// is tens to hundreds of microseconds, so this was a real, if rare, flake —
/// worse under load, where the gap spans an awaited SQLite round trip — and
/// the four `memorysafe-shadow` golden fixtures (pinned to `2592000`) only
/// caught it *probabilistically*, exactly as often as a straddle happened to
/// occur during that run.
///
/// This asserts the identity directly rather than relying on timing to
/// surface it: `write.rs` now reuses `admit_ctx.now` instead of sampling a
/// second `OffsetDateTime::now_utc()`, so `until - created_at` must equal
/// `Duration::days(30)` (`BaselineConfig::default().protection_window_days`)
/// exactly, on every run, regardless of what instant the test happens to run
/// at.
#[tokio::test]
async fn a_protected_items_window_is_exactly_the_configured_number_of_days() {
    let dir = tempfile::tempdir().expect("tempdir");
    let e = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ));

    e.remember(RememberRequest::new(
        scope(),
        "the first thing anyone ever told it",
    ))
    .await
    .unwrap();

    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    match stored[0].protection {
        Protection::Protected { until } => {
            assert_eq!(
                until - stored[0].created_at,
                Duration::days(30),
                "the item's created_at and its own protection window's until \
                 must be computed from the same clock read, not two \
                 independently-sampled ones"
            );
        }
        other => panic!("expected a Protected outcome, got {other:?}"),
    }
}
