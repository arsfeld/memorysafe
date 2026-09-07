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

use memorysafe_backend::{Backend, MAX_AUDIT_LIMIT, WriteTransaction};
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Actor, AuditEvent, AuditFilter, AuditRecord, Scope};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;
use time::OffsetDateTime;

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

/// A bare, minimal `Backend::apply` per row — no policy, no embedder, no
/// `remember` pipeline — the same shortcut
/// `memorysafe-backend`'s own conformance fixtures use
/// (`conformance/fixtures.rs`) to seed a scope with many audit rows cheaply.
/// The ceiling tests below need `MAX_AUDIT_LIMIT + 1` rows purely to prove a
/// clamp fires; going through `remember`'s full assess/admit/embed pipeline
/// for a thousand-plus items would pay for correctness this pair of tests
/// does not need and does not exercise.
async fn seed_bare_audit_rows(backend: &dyn Backend, scope: &Scope, count: usize) {
    for _ in 0..count {
        let record = AuditRecord::new(
            scope.clone(),
            AuditEvent::MaintenanceRun,
            vec![],
            Actor::system(),
            OffsetDateTime::now_utc(),
        );
        backend
            .apply(WriteTransaction::new(scope.clone(), record))
            .await
            .expect("a bare audit-only transaction is valid: same scope, no upsert, no merge");
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

/// Fix round 1, Important 3: `AuditFilter` carries no clamp of its own (see
/// `MAX_AUDIT_LIMIT`'s own doc in `memorysafe-backend` for why it cannot),
/// so `audit_page` must apply one itself — a caller-supplied `requested`
/// above the ceiling must be silently capped to it, exactly the way
/// `Page::effective_limit()` caps `Page::limit` to `MAX_PAGE_LIMIT`.
///
/// Imports `MAX_AUDIT_LIMIT` rather than hardcoding `1000`, per this plan's
/// own standing rule for exactly this shape of test (`Page::effective_limit`'s
/// own test in `memorysafe-backend` does the same).
#[tokio::test]
async fn a_request_above_the_ceiling_is_clamped_to_max_audit_limit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(SqliteBackend::open(dir.keep()));
    let e = Engine::new(EngineConfig::new(
        backend.clone(),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ));
    let s = scope();
    seed_bare_audit_rows(backend.as_ref(), &s, MAX_AUDIT_LIMIT + 1).await;

    let (records, truncated) = e
        .audit_page(&s, &AuditFilter::default(), MAX_AUDIT_LIMIT + 500)
        .await
        .unwrap();

    assert_eq!(
        records.len(),
        MAX_AUDIT_LIMIT,
        "a `requested` above the ceiling must be capped to MAX_AUDIT_LIMIT, not honoured as-is"
    );
    assert!(
        truncated,
        "more rows exist than the clamped ceiling returned, so this must report truncated"
    );
}

/// Fix round 1, Important 3: the specific exposure the reviewer named, not
/// just "a big number is clamped" in the abstract. Before the clamp,
/// `requested.saturating_add(1)` on a `requested` near `i64::MAX` produced a
/// `usize` at or beyond `2^63`; cast to `i64` by
/// `memorysafe-backend-sqlite`'s `audit::query` (`filter.limit as i64`), that
/// wraps to a *negative* number, which SQLite treats as an unbounded
/// `LIMIT` — the single largest input a caller can send would have been the
/// one that removed the ceiling entirely, rather than merely exceeding it.
/// A test that only tries a "reasonably large" `requested` (the test above)
/// cannot distinguish a correct clamp from a naive one that clamps
/// `records.len()` after the fact but still sends the unclamped, wrapping
/// value to the backend — this test specifically exercises the value that
/// would expose that difference.
#[tokio::test]
async fn a_requested_value_large_enough_to_wrap_the_backends_i64_cast_is_still_bounded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(SqliteBackend::open(dir.keep()));
    let e = Engine::new(EngineConfig::new(
        backend.clone(),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ));
    let s = scope();
    seed_bare_audit_rows(backend.as_ref(), &s, MAX_AUDIT_LIMIT + 1).await;

    // `i64::MAX as usize`: exactly the value the reviewer traced through —
    // `saturating_add(1)` takes it to `2^63`, which wraps to `i64::MIN` under
    // an `as i64` cast. `usize::MAX` itself would also wrap, but this value
    // is the one named in the finding and is reachable from an ordinary
    // `?limit=9223372036854775807` query parameter parsed as `usize`.
    let requested = i64::MAX as usize;

    let (records, truncated) = e
        .audit_page(&s, &AuditFilter::default(), requested)
        .await
        .unwrap();

    assert_eq!(
        records.len(),
        MAX_AUDIT_LIMIT,
        "an unclamped wrap would ask SQLite for an unbounded LIMIT, returning every seeded \
         row (MAX_AUDIT_LIMIT + 1) instead of a bounded page"
    );
    assert!(
        truncated,
        "a bounded page short of the full seeded corpus must still report truncated"
    );
}
