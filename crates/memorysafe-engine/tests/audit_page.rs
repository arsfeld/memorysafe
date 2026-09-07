//! `Engine::audit_page` (Plan 3 Task 9, Ruling D): the "ask the backend for
//! one extra row, then report `truncated`" protocol, hoisted out of the HTTP
//! audit route (and the CLI's identical mirror of it) into the engine, so
//! there is exactly one place that gets the boundary right.
//!
//! Both directions of the boundary are pinned here, directly against
//! `audit_page`, rather than only indirectly through whichever adapter calls
//! it: exactly `requested` rows must report `truncated == false`, and exactly
//! `requested + 1` must report `truncated == true` — with the returned page
//! still truncated down to `requested` records either way.

use memorysafe_core::{AuditFilter, Scope};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

fn engine() -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    Engine::new(EngineConfig::new(
        Arc::new(memorysafe_backend_sqlite::SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ))
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

async fn seed(e: &Engine, s: &Scope, count: usize) {
    for i in 0..count {
        e.remember(RememberRequest::new(
            s.clone(),
            &format!("distinct memory {i} about topic {i}"),
        ))
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn exactly_the_requested_row_count_is_not_truncated() {
    let e = engine();
    let s = scope();
    seed(&e, &s, 3).await;

    let (records, truncated) = e.audit_page(&s, &AuditFilter::default(), 3).await.unwrap();

    assert_eq!(records.len(), 3);
    assert!(!truncated, "exactly `requested` rows must not be truncated");
}

#[tokio::test]
async fn one_more_than_requested_is_truncated_and_capped_to_requested() {
    let e = engine();
    let s = scope();
    seed(&e, &s, 4).await;

    let (records, truncated) = e.audit_page(&s, &AuditFilter::default(), 3).await.unwrap();

    assert_eq!(
        records.len(),
        3,
        "the page must be truncated down to `requested`, never leak the probe row"
    );
    assert!(
        truncated,
        "`requested + 1` rows available must report truncated"
    );
}
