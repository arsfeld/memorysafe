//! `remember`'s `event` match (`write.rs`, mapping `decision.action` to an
//! `AuditEvent`) had no covering test: nothing in the suite ever inspected
//! `AuditRecord::event`. Hardcoding `AuditEvent::Admitted` unconditionally
//! survived every other test in the crate, including the merge and rejection
//! ones — they check the `WriteOutcome`'s `action`, never the audit record's
//! `event`.
//!
//! `events` below returns every event in the scope rather than picking one
//! by position. `AuditId` is minted by `ulid_id!` using `Ulid::generate()` —
//! the plain, non-monotonic generator (`conformance/fixtures.rs` documents
//! the consequence: ids minted within the same millisecond are ordered
//! randomly relative to each other) — and `audit::query` is `ORDER BY id
//! DESC`. `a_rejected_write_is_audited_as_rejected` and
//! `a_merged_write_is_audited_as_merged` each put two records into the same
//! scope in quick succession, so treating index `0` as "the newest" was a
//! coin flip: a confirmed flake, seen failing `left: Admitted, right:
//! Rejected`. Asserting on the multiset of events avoids the ordering
//! question entirely rather than winning it.

use memorysafe_backend::Backend;
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Action, Actor, ActorKind, AuditEvent, AuditId, AuditRecord, Scope};
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

/// Every event recorded in `scope()`, in whatever order `audit::query`
/// happens to return them. Deliberately not "the newest" or "the first" —
/// see the module doc for why picking one by position is unsound here.
async fn events(e: &Engine) -> Vec<AuditEvent> {
    e.audit(&scope(), &Default::default())
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.event)
        .collect()
}

fn count(events: &[AuditEvent], target: AuditEvent) -> usize {
    events.iter().filter(|&&e| e == target).count()
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

    // Exactly one write happened, so exactly one record exists — position
    // is unambiguous whatever the ordering, because there is only one
    // position to be in. Asserted explicitly rather than left implicit, the
    // same discipline `tests/retention.rs` uses for its own single-record
    // access.
    let events = events(&e).await;
    assert_eq!(events.len(), 1, "premise: exactly one write happened");
    assert_eq!(events[0], AuditEvent::Admitted);
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

    // Two records now share this scope, minted close enough together that
    // their relative order is not guaranteed (see the module doc). The
    // multiset is ordering-independent and strictly stronger than "the
    // first one is Rejected": exactly one Admitted (the seed) and exactly
    // one Rejected (the duplicate).
    let events = events(&e).await;
    assert_eq!(events.len(), 2, "premise: exactly two writes happened");
    assert_eq!(count(&events, AuditEvent::Admitted), 1);
    assert_eq!(count(&events, AuditEvent::Rejected), 1);
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

    // Same reasoning as the rejected-write test above: two records, order
    // not guaranteed, so assert the multiset instead of a position.
    let events = events(&e).await;
    assert_eq!(events.len(), 2, "premise: exactly two writes happened");
    assert_eq!(count(&events, AuditEvent::Admitted), 1);
    assert_eq!(count(&events, AuditEvent::Merged), 1);
}

/// Forces, deterministically, the one outcome `ulid::Ulid::generate()`'s
/// non-monotonic randomness can otherwise only produce by chance: two audit
/// records in one scope where the logically-second write (`Rejected`) is
/// minted with a numerically SMALLER id than the logically-first
/// (`Admitted`) — legal for two ids in the same millisecond, per
/// `conformance/fixtures.rs`. Bypasses `Engine` (which always mints its own
/// id) and talks to `Backend` directly so the ids can be hand-assigned; both
/// `AuditRecord::id` and `memorysafe_backend_sqlite::audit::insert` are
/// public/unconditional about persisting whatever id they are given.
///
/// This is the confirmed flake from the module doc, made to happen every
/// time instead of waiting for it: the old `audit.remove(0)`/`audit[0]` form
/// reads this scope's newest-by-id row as `Admitted`, when the record
/// actually written second — and second is what "the rejected write" means
/// in `a_rejected_write_is_audited_as_rejected` — is `Rejected`. The
/// multiset form the real tests now use does not have this problem: it
/// never asks which record is "first".
#[tokio::test]
async fn the_multiset_form_survives_the_ordering_that_breaks_the_old_positional_form() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backend = SqliteBackend::open(dir.keep());
    let scope = scope();
    let now = time::OffsetDateTime::now_utc();
    let actor = Actor {
        kind: ActorKind::Agent,
        id: None,
    };

    // The maximum and minimum valid ULID strings — as far apart in sort
    // order as two ids can be, so nothing about this depends on luck.
    let large_id = AuditId::parse("7ZZZZZZZZZZZZZZZZZZZZZZZZZ").unwrap();
    let small_id = AuditId::parse(&"0".repeat(26)).unwrap();

    let mut written_first = AuditRecord::new(
        scope.clone(),
        AuditEvent::Admitted,
        vec![],
        actor.clone(),
        now,
    );
    written_first.id = large_id;
    backend.record_recall(written_first).await.unwrap();

    let mut written_second =
        AuditRecord::new(scope.clone(), AuditEvent::Rejected, vec![], actor, now);
    written_second.id = small_id;
    backend.record_recall(written_second).await.unwrap();

    let rows = backend.audit(&scope, &Default::default()).await.unwrap();
    assert_eq!(rows.len(), 2, "premise: both records landed");

    // The OLD form, shown failing: `audit::query`'s `ORDER BY id DESC` puts
    // the large-id row first, which is `Admitted` — not the `Rejected` row
    // that was actually written second. A test asserting
    // `rows[0].event == AuditEvent::Rejected` (the shape every site in this
    // sweep used to have) would fail against this ordering.
    assert_eq!(
        rows[0].event,
        AuditEvent::Admitted,
        "demonstrates the flake: the highest-sorting id is Admitted here even \
         though Rejected was written second — `rows[0]` is not reliably \"the \
         newest\""
    );

    // The NEW form, shown immune: the multiset does not care which row
    // sorts first.
    let events: Vec<AuditEvent> = rows.iter().map(|r| r.event).collect();
    assert_eq!(count(&events, AuditEvent::Admitted), 1);
    assert_eq!(count(&events, AuditEvent::Rejected), 1);
}
