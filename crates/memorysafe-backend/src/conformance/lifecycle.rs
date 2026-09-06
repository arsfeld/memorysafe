use super::{BackendFactory, fx};
use crate::portability::ScopeSelector;
use crate::{Backend, Page};
use memorysafe_core::{AuditEvent, AuditFilter, Scope, SubjectId, TenantId};
use std::collections::BTreeSet;
use time::{Duration, OffsetDateTime};

/// Exercises every axis `AuditFilter` can narrow on: `events`, the
/// `since`/`until` time window, and `limit` — including the newest-first
/// ordering `limit` depends on.
///
/// An earlier draft of this test had defects, fixed here:
///
/// 1. **The time bound was never exercised**, despite the test's name —
///    `since`/`until` sat unused. Fixed by querying a `since`/`until` window
///    that must return a genuine strict subset of the four audit rows: not
///    all of them, and not none.
/// 2. **Every record shared one timestamp.** `fx::item`/`fx::evict_txn` both
///    pin `UNIX_EPOCH`, so any ordering assertion over `at` held for *any*
///    order, including oldest-first. Fixed by giving each of the four
///    records its own timestamp — `at` stores whole seconds (see
///    `AuditFilter::after`'s doc on why a time cursor can't separate
///    same-second rows), so anything finer would not survive a real round
///    trip through storage.
/// 3. **The ordering assertion compared `at`**, the one field
///    `memorysafe_core::AuditFilter` documents as unable to provide a total
///    order: rows are ordered by `AuditId` instead, because "`at` is whole
///    seconds and cannot separate rows written in the same second, so a
///    time-based cursor would repeat or skip them." Fixed by asserting
///    identity instead: the two rows returned under `limit: 2` must be the
///    two most recently *written* rows, newest first — checked against the
///    `AuditId`s the writes themselves returned, not against `at`.
/// 4. **The eviction's `at` was the newest of the four**, so ordering by
///    `at` and ordering by `AuditId` (write order) agreed by construction —
///    the test could not tell the two conventions apart, and would have
///    certified a backend that (wrongly) orders by `at`. Fixed by placing
///    the eviction's business timestamp *between* the second and third
///    admit's rather than after all of them, so the two orderings
///    genuinely disagree and only the documented one (`AuditId`) is
///    asserted as correct.
pub async fn audit_filter_narrows_by_event_and_time<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    // Three admits at t0/t1/t2, ten seconds apart. The eviction's business
    // timestamp (`t_evict`) sits between t1 and t2 rather than after
    // everything, so it is not simultaneously "newest by `at`" and "newest
    // by write order" — see point 4 above.
    let t0 = OffsetDateTime::UNIX_EPOCH;
    let t1 = t0 + Duration::seconds(10);
    let t2 = t0 + Duration::seconds(20);
    let t_evict = t0 + Duration::seconds(15);

    let mut audit_ids = Vec::new();
    for (i, at) in [t0, t1, t2].into_iter().enumerate() {
        let applied = backend
            .apply(fx::admit_txn(
                &scope,
                fx::item_at(&scope, &format!("memory {i}"), at),
                None,
            ))
            .await
            .unwrap();
        audit_ids.push(applied.audit_id);
    }

    // `list` orders ascending by `created_at`, so `items[0]` is the item
    // admitted at `t0` — the oldest one.
    let items = backend.list(&scope, &Page::default()).await.unwrap();
    let evicted = backend
        .apply(fx::evict_txn_at(&scope, vec![items[0].id.clone()], t_evict))
        .await
        .unwrap();
    audit_ids.push(evicted.audit_id);

    let admits = backend
        .audit(
            &scope,
            &AuditFilter {
                events: vec![AuditEvent::Admitted],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(admits.len(), 3);
    assert!(admits.iter().all(|r| r.event == AuditEvent::Admitted));

    let forgets = backend
        .audit(
            &scope,
            &AuditFilter {
                events: vec![AuditEvent::Forgotten],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(forgets.len(), 1);

    // Time filter: a window strictly between the first admit's timestamp
    // and the third's (5s..25s) admits the second admit (10s), the
    // eviction (15s), and the third admit (20s), and excludes only the
    // first admit (0s) — a strict subset (3 of 4), so a backend that
    // ignores the filter and returns everything, or one that returns
    // nothing, both fail. The bounds are chosen off any record's exact
    // timestamp so the test does not depend on whether `since`/`until` are
    // inclusive or exclusive at the edges.
    let since = t0 + Duration::seconds(5);
    let until = t0 + Duration::seconds(25);
    let windowed = backend
        .audit(
            &scope,
            &AuditFilter {
                since: Some(since),
                until: Some(until),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        windowed.len(),
        3,
        "since/until must select a strict subset of the four rows, not all or none"
    );
    assert!(
        windowed.iter().all(|r| r.at >= since && r.at <= until),
        "a row outside [since, until] leaked through the time filter"
    );
    let windowed_ids: BTreeSet<_> = windowed.iter().map(|r| r.id.clone()).collect();
    let expected_ids: BTreeSet<_> = [
        audit_ids[1].clone(),
        audit_ids[3].clone(),
        audit_ids[2].clone(),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        windowed_ids, expected_ids,
        "since/until returned the wrong rows"
    );

    // Ordering: `limit: 2` must return the two most recently *written*
    // rows, newest first — by `AuditId` (write/insertion order), not by
    // the business `at` field. The eviction's `at` (15s) is earlier than
    // the third admit's (20s), so a backend that (wrongly) orders by `at`
    // would return [third admit, eviction] here; the documented ordering
    // by `AuditId` returns [eviction, third admit], since the eviction was
    // written after the third admit regardless of the business timestamp
    // it carries.
    let limited = backend
        .audit(
            &scope,
            &AuditFilter {
                limit: 2,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(limited.len(), 2);
    let limited_ids: Vec<_> = limited.iter().map(|r| r.id.clone()).collect();
    assert_eq!(
        limited_ids,
        vec![audit_ids[3].clone(), audit_ids[2].clone()],
        "audit must come back newest first by write order (AuditId), not by the business `at` field: the eviction, then the third admit"
    );
}

/// Right-to-delete must be total for the subject.
pub async fn purge_subject_removes_everything_for_that_subject<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let tenant = TenantId::new("t").unwrap();
    let subject = SubjectId::new("doomed").unwrap();
    let a = Scope::new("t", "doomed", "ns-a").unwrap();
    let b = Scope::new("t", "doomed", "ns-b").unwrap();

    for scope in [&a, &b] {
        for i in 0..3 {
            backend
                .apply(fx::admit_txn_embedded(
                    scope,
                    fx::item(scope, &format!("memory {i}")),
                ))
                .await
                .unwrap();
        }
    }

    // Baseline: tie the purge report's own numbers to observed state. Every
    // check below the purge is satisfied by a backend that stored nothing
    // at all unless this baseline proves the corpus existed first.
    assert_eq!(
        backend.list(&a, &Page::default()).await.unwrap().len(),
        3,
        "the corpus must exist before the purge for the purge report to mean anything"
    );

    let report = backend.purge_subject(&tenant, &subject).await.unwrap();
    assert_eq!(report.items_removed, 6);
    assert_eq!(report.vectors_removed, 6);

    assert!(backend.list(&a, &Page::default()).await.unwrap().is_empty());
    assert!(backend.list(&b, &Page::default()).await.unwrap().is_empty());
    assert_eq!(backend.capacity_state(&a).await.unwrap().used_items, 0);
    assert_eq!(backend.capacity_state(&b).await.unwrap().used_items, 0);
    assert_eq!(
        report.audit_rows_removed + report.audit_rows_preserved,
        6,
        "every pre-existing audit row must be accounted for as removed or preserved \
         (a backend's own SubjectPurged audit row, if it writes one, did not exist \
         before the purge and is not counted in this total)"
    );
}

pub async fn purge_subject_leaves_other_subjects_intact<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let doomed = Scope::new("t", "doomed", "ns").unwrap();
    let keeper = Scope::new("t", "keeper", "ns").unwrap();

    backend
        .apply(fx::admit_txn(&doomed, fx::item(&doomed, "goes away"), None))
        .await
        .unwrap();
    let keeper_item = fx::item(&keeper, "stays");
    backend
        .apply(fx::admit_txn(&keeper, keeper_item.clone(), None))
        .await
        .unwrap();

    backend
        .purge_subject(
            &TenantId::new("t").unwrap(),
            &SubjectId::new("doomed").unwrap(),
        )
        .await
        .unwrap();

    assert!(
        backend
            .list(&doomed, &Page::default())
            .await
            .unwrap()
            .is_empty()
    );
    let keeper_items = backend.list(&keeper, &Page::default()).await.unwrap();
    assert_eq!(keeper_items.len(), 1);
    assert_eq!(
        keeper_items[0].id, keeper_item.id,
        "purge left the keeper with a different item than the one it admitted"
    );
    // A purge scoped as `DELETE FROM capacity WHERE tenant = ?` (plausible,
    // since purge's own arguments are tenant + subject) would correctly
    // scope the item delete but silently zero an unrelated subject's
    // capacity accounting. Nothing else in this suite purges, so only this
    // test can catch it.
    assert_eq!(
        backend.capacity_state(&keeper).await.unwrap().used_items,
        1,
        "purging one subject zeroed an unrelated subject's capacity accounting"
    );
    assert_eq!(
        backend
            .audit(&keeper, &AuditFilter::default())
            .await
            .unwrap()
            .len(),
        1,
        "purging one subject destroyed another's audit"
    );
}

/// Invariant 5 from the spec: export then import reproduces the corpus
/// exactly.
///
/// The three items are built to each carry a different set of
/// `MemoryItem`'s fields away from their shared defaults, not just
/// `kind`/`tags`/`sensitivity`: one carries `Protection::Pinned`, one an
/// `occurred_at`, a `ttl`, and an `attrs` entry, and one
/// `pending_embedding: true` with a non-`Internal` sensitivity. A round
/// trip that silently dropped or re-derived any of these on import, rather
/// than actually persisting and restoring them, is caught by the
/// full-`MemoryItem` equality below. This still cannot catch a field the
/// backend never persists at all — `before` and `after` are both read back
/// through the same backend, via `list()` — but it converts every
/// plausible import-side re-derivation from invisible to caught.
pub async fn export_import_round_trips_exactly<F: BackendFactory>(factory: &F) {
    let source = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let mut cats = fx::item_with(
        &scope,
        "first memory about cats",
        "fact",
        &["work"],
        memorysafe_core::SensitivityLevel::Internal,
    );
    cats.protection = memorysafe_core::Protection::Pinned;

    let mut dogs = fx::item_with(
        &scope,
        "second memory about dogs",
        "preference",
        &["home", "pets"],
        memorysafe_core::SensitivityLevel::Internal,
    );
    dogs.occurred_at = Some(OffsetDateTime::UNIX_EPOCH + Duration::seconds(1));
    dogs.ttl = Some(Duration::days(30));
    dogs.attrs
        .insert("confidence".to_string(), serde_json::json!(0.75));

    let mut zstd = fx::item_with(
        &scope,
        "third memory about zstandard",
        "procedure",
        &[],
        memorysafe_core::SensitivityLevel::Restricted,
    );
    zstd.pending_embedding = true;

    for item in [cats, dogs, zstd] {
        source
            .apply(fx::admit_txn_embedded(&scope, item))
            .await
            .unwrap();
    }

    let selector = ScopeSelector {
        tenant: TenantId::new("t").unwrap(),
        subject: None,
        namespace: None,
        include_audit: true,
    };
    let exported = source.export(&selector).await.unwrap();

    let target = factory.create().await;
    let report = target.import(exported.clone()).await.unwrap();
    assert_eq!(report.items_imported, 3);
    assert_eq!(report.vectors_imported, 3);
    assert_eq!(
        report.audit_imported, 3,
        "include_audit: true must actually export and import the audit rows, \
         not just set the flag"
    );

    let mut before = source
        .list(
            &scope,
            &Page {
                offset: 0,
                limit: 100,
            },
        )
        .await
        .unwrap();
    let mut after = target
        .list(
            &scope,
            &Page {
                offset: 0,
                limit: 100,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        before.len(),
        3,
        "the source corpus must exist before comparing it to the imported copy"
    );
    before.sort_by(|a, b| a.id.cmp(&b.id));
    after.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(
        before, after,
        "round trip did not reproduce the items exactly"
    );

    // Vectors survived: the same probe ranks the same way on both sides.
    use memorysafe_embed::Embedder;
    let probe = fx::embedder().embed("cats").unwrap();
    let src_hits = source.neighbours(&scope, &probe, 3).await.unwrap();
    let tgt_hits = target.neighbours(&scope, &probe, 3).await.unwrap();
    assert_eq!(
        src_hits.len(),
        3,
        "the source itself must have ranked vectors before comparing rankings \
         across the round trip — otherwise empty-vs-empty would pass"
    );
    let ids = |v: &[memorysafe_core::ScoredCandidate]| {
        v.iter().map(|c| c.item.id.clone()).collect::<Vec<_>>()
    };
    assert_eq!(
        ids(&src_hits),
        ids(&tgt_hits),
        "vector ranking changed across the round trip"
    );
}

/// Importing the same stream twice must not duplicate anything.
pub async fn import_is_idempotent<F: BackendFactory>(factory: &F) {
    let source = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    source
        .apply(fx::admit_txn_embedded(
            &scope,
            fx::item(&scope, "only memory"),
        ))
        .await
        .unwrap();

    let selector = ScopeSelector {
        tenant: TenantId::new("t").unwrap(),
        subject: None,
        namespace: None,
        include_audit: false,
    };
    let exported = source.export(&selector).await.unwrap();

    let target = factory.create().await;
    let first = target.import(exported.clone()).await.unwrap();
    assert_eq!(first.items_imported, 1);
    assert!(
        target
            .audit(&scope, &AuditFilter::default())
            .await
            .unwrap()
            .is_empty(),
        "include_audit: false must not import any audit rows"
    );

    let second = target.import(exported).await.unwrap();
    assert_eq!(second.items_imported, 0);
    assert_eq!(second.items_skipped_existing, 1);
    assert_eq!(
        second.vectors_imported, 0,
        "a skipped item's vector must not be re-inserted either"
    );
    assert_eq!(
        target.list(&scope, &Page::default()).await.unwrap().len(),
        1
    );
}
