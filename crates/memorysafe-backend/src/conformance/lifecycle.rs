use super::{BackendFactory, fx};
use crate::portability::{ExportRecord, ImportStream, ScopeSelector};
use crate::{AuditAggregateFilter, Backend, BackendError, Page};
use memorysafe_core::{
    Actor, ActorKind, AuditEvent, AuditFilter, AuditId, AuditRecord, PurgeCascade, Scope,
    SubjectId, TenantId,
};
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
/// 5. **The asserted id sequence was non-deterministic.**
///    `AuditRecord::new` sets `id: AuditId::new()` — the plain ULID
///    generator — while taking `at` as a parameter, so two records minted in
///    the same millisecond are ordered *randomly* relative to each other.
///    These four are minted microseconds apart, so the exact two-element
///    sequence below failed roughly half the time against a fast in-memory
///    backend and passed against one with a real fsync between writes: a
///    verdict that depended on backend speed rather than on conformance.
///    Fixed by pinning all four ids to the ascending literals in
///    `fx::AUDIT_ORDER_ULIDS`, with the eviction's the largest while its `at`
///    stays earlier than the third admit's — so `at`-ordering and id-ordering
///    genuinely disagree and only the documented id-ordering can pass.
///
/// **The implementation this test rejects:** a backend that orders `audit`
/// by the business timestamp `at` (the obvious reading of "newest first" for
/// a log, and the column a human would index) rather than by `AuditId`. Such
/// a backend returns `[third admit, eviction]` under `limit: 2` where the
/// contract requires `[eviction, third admit]`.
///
/// **It passes vacuously if** the four records ever come to share a
/// timestamp, or if the eviction's `at` is moved back after the third
/// admit's: either collapses the disagreement between the two orderings and
/// both conventions then produce the same sequence.
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

    // Pinned, ascending, eviction last and largest — never
    // `AuditRecord::new`'s generated ids; see `fx::AUDIT_ORDER_ULIDS` and
    // point 5 above.
    let audit_ids: Vec<AuditId> = fx::AUDIT_ORDER_ULIDS
        .iter()
        .map(|u| AuditId::parse(u).expect("literal must be a canonical ULID"))
        .collect();

    for (i, at) in [t0, t1, t2].into_iter().enumerate() {
        let mut txn = fx::admit_txn(
            &scope,
            fx::item_at(&scope, &format!("memory {i}"), at),
            None,
        );
        txn.audit.id = audit_ids[i].clone();
        let applied = backend.apply(txn).await.unwrap();
        assert_eq!(
            applied.audit_id, audit_ids[i],
            "apply must persist and return the AuditId it was given — the echo \
             rule on `Backend`. Every id assertion below is written against the \
             ids this test supplied, so a minted id makes them unreadable"
        );
    }

    // `list` orders ascending by `created_at`, so `items[0]` is the item
    // admitted at `t0` — the oldest one.
    let items = backend.list(&scope, &Page::default()).await.unwrap();
    let mut evict = fx::evict_txn_at(&scope, vec![items[0].id.clone()], t_evict);
    evict.audit.id = audit_ids[3].clone();
    let evicted = backend.apply(evict).await.unwrap();
    assert_eq!(
        evicted.audit_id, audit_ids[3],
        "apply must persist and return the AuditId it was given (echo rule)"
    );

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

    let report = backend
        .purge_subject(
            &tenant,
            &subject,
            PurgeCascade::Cascade,
            fx::purge_record(&a, AuditId::new(), Actor::system()),
        )
        .await
        .unwrap();
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
         (the purge's own SubjectPurged record did not exist before the purge and is \
         counted in neither term — see PurgeReport)"
    );
    // Cascade is now an argument, not a profile the backend guesses, so which
    // side of the equation the six rows land on is this test's business.
    assert_eq!(
        report.audit_rows_removed, 6,
        "under PurgeCascade::Cascade every pre-existing audit row is removed"
    );
    assert_eq!(
        report.audit_rows_preserved, 0,
        "under PurgeCascade::Cascade nothing is preserved"
    );
    // What is left in each namespace's audit is the purge's own record and
    // nothing else. It was filed under `a`, so `b` is empty.
    let left_in_a = backend.audit(&a, &AuditFilter::default()).await.unwrap();
    assert_eq!(
        left_in_a.len(),
        1,
        "a cascading purge must leave exactly its own SubjectPurged record"
    );
    assert_eq!(left_in_a[0].event, AuditEvent::SubjectPurged);
    assert!(
        backend
            .audit(&b, &AuditFilter::default())
            .await
            .unwrap()
            .is_empty(),
        "a cascading purge left detail rows behind in the subject's other namespace"
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
            PurgeCascade::Cascade,
            fx::purge_record(&doomed, AuditId::new(), Actor::system()),
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
    let report = target
        .import(&selector.tenant, exported.clone())
        .await
        .unwrap();
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

    // A stream of a `Header` and nothing else is valid and imports nothing —
    // `Backend::import`'s doc says so explicitly, and `export` of a tenant
    // that was never written to produces exactly that. This is the round
    // trip's other edge, previously unchecked: nothing in this suite verified
    // that a header-only stream survives instead of being rejected as
    // malformed for lacking any content.
    let untouched_tenant = TenantId::new("nobody-ever-wrote-here").unwrap();
    let empty_selector = ScopeSelector {
        tenant: untouched_tenant.clone(),
        subject: None,
        namespace: None,
        include_audit: true,
    };
    let header_only = source.export(&empty_selector).await.unwrap();
    assert_eq!(
        header_only.len(),
        1,
        "an untouched tenant's export must be exactly the Header, nothing else"
    );
    assert!(matches!(header_only[0], ExportRecord::Header { .. }));
    let header_only_report = target
        .import(&untouched_tenant, header_only)
        .await
        .expect("a header-only stream is valid and must not be rejected");
    assert_eq!(header_only_report.items_imported, 0);
    assert_eq!(header_only_report.vectors_imported, 0);
    assert_eq!(header_only_report.audit_imported, 0);
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
    let first = target
        .import(&selector.tenant, exported.clone())
        .await
        .unwrap();
    assert_eq!(first.items_imported, 1);
    assert!(
        target
            .audit(&scope, &AuditFilter::default())
            .await
            .unwrap()
            .is_empty(),
        "include_audit: false must not import any audit rows"
    );

    let second = target.import(&selector.tenant, exported).await.unwrap();
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

/// The destination tenant is compared against **every** record, not just the
/// first one.
///
/// **The implementation this exists to reject: one that reads the tenant from
/// the first record and imports the rest on that authority.** That was the
/// shape of both backend sketches before `import` took a destination, and it
/// is the shape anyone re-derives from "every record in a stream belongs to
/// one tenant". Such an implementation passes the naive all-agree case —
/// `export_import_round_trips_exactly` and `import_is_idempotent` both feed it
/// a single-tenant stream — and fails only here. So this is the *only* test in
/// the suite where a first-record implementation is distinguishable from a
/// per-record one, which is why the disagreeing record is deliberately not
/// first: put it first and a first-record implementation passes by accident,
/// having checked the one record it was ever going to check.
///
/// A cross-tenant record must reject the whole import rather than be
/// retargeted into the destination. Retargeting would keep the write silent
/// and merely relocate the differential — the caller would learn neither where
/// the row landed nor that its scope had been rewritten. The assertions below
/// therefore check both: that nothing from the stream is present in the
/// destination, and that the agreeing record did not land either.
pub async fn import_rejects_a_later_record_whose_tenant_disagrees<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let destination = TenantId::new("t").unwrap();
    let mine = Scope::new("t", "s", "n").unwrap();
    let theirs = Scope::new("other", "s", "n").unwrap();

    let agreeing = fx::item(&mine, "belongs to the destination tenant");
    let foreign = fx::item(&theirs, "belongs to somebody else entirely");

    // Header, then an agreeing record, then a disagreeing one. The order is
    // the whole point of the test — see the doc comment.
    let stream: ImportStream = vec![
        ExportRecord::Header {
            format_version: crate::FORMAT_VERSION,
            exported_at: 0,
        },
        ExportRecord::Item {
            item: Box::new(agreeing.clone()),
            vector: None,
        },
        ExportRecord::Item {
            item: Box::new(foreign.clone()),
            vector: None,
        },
    ];

    let err = backend
        .import(&destination, stream)
        .await
        .expect_err("a record whose tenant disagrees with the destination must reject the import");
    assert!(
        matches!(err, BackendError::MalformedImport(_)),
        "expected MalformedImport for a cross-tenant record, got {err:?}"
    );

    // Rejected means rejected: not "the good records landed and the bad one
    // did not". A partial import would leave the caller with a corpus it never
    // asked for and no report describing it.
    assert!(
        backend
            .list(&mine, &Page::default())
            .await
            .unwrap()
            .is_empty(),
        "a rejected import still wrote rows into the destination"
    );
    assert!(
        backend.get(&mine, &agreeing.id).await.unwrap().is_none(),
        "the agreeing record landed even though the import was rejected"
    );
    // And specifically: the foreign record was not quietly rewritten into the
    // destination tenant. A backend that retargets rather than rejects fails
    // here even if it also (wrongly) returned an error.
    assert!(
        backend.get(&mine, &foreign.id).await.unwrap().is_none(),
        "the foreign record was retargeted into the destination tenant"
    );
    assert!(
        backend
            .list(&theirs, &Page::default())
            .await
            .unwrap()
            .is_empty(),
        "a rejected import wrote into the tenant the payload named"
    );
}

/// The disagreement rule covers audit rows too, and is checked independently
/// of the item check rather than folded into it.
///
/// `Backend::import`'s doc says audit rows "follow from the same comparison,
/// not from a rule of their own" — but in both plan sketches the item-tenant
/// check and the audit-tenant check are a distinct `if` in a distinct match
/// arm, and until this test no conformance test ever put a foreign
/// `ExportRecord::Audit` in a stream. A backend that checks every item's
/// tenant and simply forgets the `ExportRecord::Audit` arm passes every other
/// test in this suite — including
/// `import_rejects_a_later_record_whose_tenant_disagrees`, whose stream
/// always carries a foreign *item*, so that test's import is rejected
/// regardless of whether the audit-row check exists at all.
///
/// **The implementation this exists to reject: exactly that backend.** Every
/// item here agrees with `destination`; only the audit record's `scope`
/// names a different tenant. With no foreign item to catch the mismatch
/// first, only a real per-audit-row tenant check rejects this import — which
/// is what makes this test able to fail where the other one cannot.
pub async fn import_rejects_a_foreign_audit_record_even_when_every_item_agrees<
    F: BackendFactory,
>(
    factory: &F,
) {
    let backend = factory.create().await;
    let destination = TenantId::new("t").unwrap();
    let mine = Scope::new("t", "s", "n").unwrap();
    let theirs = Scope::new("other", "s", "n").unwrap();

    let agreeing = fx::item(&mine, "belongs to the destination tenant");
    let foreign_audit = AuditRecord::new(
        theirs.clone(),
        AuditEvent::Admitted,
        vec![],
        Actor::system(),
        OffsetDateTime::UNIX_EPOCH,
    );

    let stream: ImportStream = vec![
        ExportRecord::Header {
            format_version: crate::FORMAT_VERSION,
            exported_at: 0,
        },
        ExportRecord::Item {
            item: Box::new(agreeing.clone()),
            vector: None,
        },
        ExportRecord::Audit {
            audit: Box::new(foreign_audit),
        },
    ];

    let err = backend.import(&destination, stream).await.expect_err(
        "an audit row whose tenant disagrees with the destination must reject the import",
    );
    assert!(
        matches!(err, BackendError::MalformedImport(_)),
        "expected MalformedImport for a cross-tenant audit row, got {err:?}"
    );

    // Rejected means rejected for this record class exactly as for items:
    // the agreeing item must not have landed just because it, by itself,
    // agreed with the destination.
    assert!(
        backend
            .list(&mine, &Page::default())
            .await
            .unwrap()
            .is_empty(),
        "a rejected import still wrote rows into the destination"
    );
    assert!(
        backend.get(&mine, &agreeing.id).await.unwrap().is_none(),
        "the agreeing item landed even though the import was rejected for its \
         accompanying audit row"
    );
    assert!(
        backend
            .audit(&mine, &AuditFilter::default())
            .await
            .unwrap()
            .is_empty(),
        "a rejected import must not write the agreeing tenant's audit either"
    );
    assert!(
        backend
            .audit(&theirs, &AuditFilter::default())
            .await
            .unwrap()
            .is_empty(),
        "the foreign audit row was retargeted or written into the tenant it named"
    );
}

/// Aggregates survive a cascading `purge_subject`. Detail rows do not.
///
/// **The implementation this exists to reject: one that stores aggregates in,
/// or cascades them from, the audit detail table.** That is the natural shape
/// — aggregates are derived from audit rows, they are written in the same
/// transaction as the audit row that feeds them (Tasks 20 and 23), and
/// `purge::subject` already sweeps `audit` by subject. One more table in the
/// same `DELETE ... WHERE subject = ?` sweep, or an `ON DELETE CASCADE` from
/// the audit row, and the aggregate is gone. Nothing else in the suite would
/// notice: no other test reads an aggregate at all.
///
/// The case that makes it able to fail is the baseline read *before* the
/// purge. Without it, "the aggregate still has the same count afterwards" is
/// satisfied by a backend that never wrote one — both reads return nothing and
/// the equality holds vacuously. So the count is asserted non-zero first, and
/// the comparison afterwards is against that same value.
///
/// Only `Admitted` aggregates are compared. A backend may legitimately write
/// its own `SubjectPurged` audit row and count it, exactly as `PurgeReport`'s
/// doc allows for detail rows, so a whole-vector equality would fail a
/// conformant backend.
pub async fn audit_aggregates_survive_a_cascading_purge<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let tenant = TenantId::new("t").unwrap();
    let subject = SubjectId::new("doomed").unwrap();
    let scope = Scope::new("t", "doomed", "ns").unwrap();

    for i in 0..3 {
        backend
            .apply(fx::admit_txn_embedded(
                &scope,
                fx::item(&scope, &format!("memory {i}")),
            ))
            .await
            .unwrap();
    }

    let admits_before: Vec<_> = backend
        .audit_aggregates(&tenant, &AuditAggregateFilter::default())
        .await
        .unwrap()
        .into_iter()
        .filter(|a| a.key.event == AuditEvent::Admitted)
        .collect();
    let counted_before: u64 = admits_before.iter().map(|a| a.count).sum();
    assert_eq!(
        counted_before, 3,
        "the aggregate must exist and count the three admits before the purge, \
         or 'aggregates survive' is satisfied vacuously by a backend that never \
         wrote one"
    );
    assert_eq!(
        backend
            .audit(&scope, &AuditFilter::default())
            .await
            .unwrap()
            .len(),
        3,
        "the detail rows must exist before the purge too, so the contrast after \
         it means something"
    );

    let report = backend
        .purge_subject(
            &tenant,
            &subject,
            PurgeCascade::Cascade,
            fx::purge_record(&scope, AuditId::new(), Actor::system()),
        )
        .await
        .unwrap();
    assert_eq!(report.items_removed, 3, "the purge did not run");
    // Detail rows are accounted for as removed or preserved, exactly as
    // `purge_subject_removes_everything_for_that_subject` requires. This call
    // cascades, so all three are removed — what is asserted here is that the
    // aggregate's fate is not tied to either term.
    assert_eq!(
        report.audit_rows_removed + report.audit_rows_preserved,
        3,
        "every pre-existing audit row must be accounted for"
    );
    // `+ 1` for the purge's own SubjectPurged record, which is written under
    // both cascade modes and counted in neither term of the report.
    assert_eq!(
        backend
            .audit(&scope, &AuditFilter::default())
            .await
            .unwrap()
            .len() as u64,
        report.audit_rows_preserved + 1,
        "the surviving detail rows must be exactly the ones the report says it \
         preserved, plus the purge's own record"
    );

    let after = backend
        .audit_aggregates(&tenant, &AuditAggregateFilter::default())
        .await
        .unwrap();
    let admits_after: Vec<_> = after
        .iter()
        .filter(|a| a.key.event == AuditEvent::Admitted)
        .cloned()
        .collect();
    assert_eq!(
        admits_after, admits_before,
        "purge_subject destroyed or altered the audit aggregates; they name no \
         subject and no namespace, so there is nothing in them for a subject \
         purge to erase"
    );

    // And nothing that survived names the purged subject or its namespace.
    // `AggregateKey` cannot represent either, so this can only fail if a
    // backend smuggled one into a field that can hold text — the policy name,
    // say. It is the residual claim the whole key design rests on, so it is
    // asserted rather than assumed.
    let json = serde_json::to_string(&after).unwrap();
    assert!(
        !json.contains("doomed"),
        "an aggregate row that outlived the purge names the purged subject: {json}"
    );
    assert!(
        !json.contains("\"ns\""),
        "an aggregate row that outlived the purge names the namespace: {json}"
    );
}

/// `PurgeCascade::Preserve`: the subject's *data* is erased, its audit detail
/// survives.
///
/// The existing purge tests both cascade, and until `cascade` became an
/// argument `Preserve` was not backend behaviour at all — the engine faked it
/// by reading the rows out and re-inserting them afterwards. It is backend
/// behaviour now, and this is its only coverage.
///
/// **The implementation this exists to reject: one that ignores `cascade` and
/// always cascades.** That is what both backend sketches did before `cascade`
/// became an argument — a single unconditional `DELETE ... WHERE subject = ?`
/// across every table, which the Postgres plan's sketch still carries — and it
/// passes both other purge tests, which only ever ask for `Cascade`.
/// A close second: one that reads `Preserve` as "do not touch the audit table
/// at all" and therefore skips its own `SubjectPurged` insert; the record is
/// written under *both* modes, so that one fails here too.
///
/// **It passes vacuously if** the baselines below are removed. With no audit
/// rows before the purge, "every pre-existing row is still readable" and
/// `preserved == pre_existing` are both satisfied by zero; with no items,
/// "the items are gone" is satisfied by a backend that stored nothing. Hence
/// the corpus, the capacity, the vector hit and the audit count are all
/// asserted *before* the purge runs.
pub async fn purge_subject_preserves_audit_when_asked<F: BackendFactory>(factory: &F) {
    use memorysafe_embed::Embedder;

    let backend = factory.create().await;
    let tenant = TenantId::new("t").unwrap();
    let subject = SubjectId::new("doomed").unwrap();
    let scope = Scope::new("t", "doomed", "ns").unwrap();

    // Two plain admits plus one carrying an idempotency key, so the purge's
    // effect on the idempotency records is observable at all: nothing else in
    // this suite can see whether they survived a purge.
    for i in 0..2 {
        backend
            .apply(fx::admit_txn_embedded(
                &scope,
                fx::item(&scope, &format!("memory {i}")),
            ))
            .await
            .unwrap();
    }
    let keyed = fx::item(&scope, "written under an idempotency key");
    let mut keyed_txn = fx::admit_txn_embedded(&scope, keyed.clone());
    keyed_txn.idempotency_key = Some("survives-a-purge?".into());
    keyed_txn.payload_digest = Some(keyed.digest());
    backend.apply(keyed_txn).await.unwrap();

    // Baselines. Without these every post-purge assertion is satisfiable by a
    // backend that stored nothing at all.
    assert_eq!(
        backend.list(&scope, &Page::default()).await.unwrap().len(),
        3,
        "the corpus must exist before the purge for the report to mean anything"
    );
    let capacity_before = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(capacity_before.used_items, 3);
    assert!(
        capacity_before.used_bytes > 0,
        "capacity accounting must be non-zero before the purge, or 'capacity is \
         gone' is satisfied by a backend that never counted anything"
    );
    let probe = fx::embedder().embed("memory").unwrap();
    assert!(
        !backend
            .neighbours(&scope, &probe, 10)
            .await
            .unwrap()
            .is_empty(),
        "vectors must be searchable before the purge, or 'the vectors are gone' \
         is vacuous"
    );
    let before: BTreeSet<_> = backend
        .audit(&scope, &AuditFilter::default())
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(before.len(), 3, "three admits, three audit rows");

    let purge_id = AuditId::parse("01ARZ3NDEKTSV4RRFFQ69G5FD9").unwrap();
    let report = backend
        .purge_subject(
            &tenant,
            &subject,
            PurgeCascade::Preserve,
            fx::purge_record(&scope, purge_id.clone(), Actor::system()),
        )
        .await
        .unwrap();

    // Items, vectors and capacity go under Preserve exactly as under Cascade.
    assert_eq!(report.items_removed, 3);
    assert_eq!(report.vectors_removed, 3);
    assert!(
        backend
            .list(&scope, &Page::default())
            .await
            .unwrap()
            .is_empty(),
        "Preserve preserves the audit, not the items"
    );
    assert!(
        backend.get(&scope, &keyed.id).await.unwrap().is_none(),
        "Preserve left an item behind"
    );
    assert!(
        backend
            .neighbours(&scope, &probe, 10)
            .await
            .unwrap()
            .is_empty(),
        "Preserve left the subject's vectors behind"
    );
    let capacity_after = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(
        capacity_after.used_items, 0,
        "Preserve left the subject's capacity accounting behind"
    );
    assert_eq!(capacity_after.used_bytes, 0);

    // The audit detail is what survives, and the report says so.
    assert_eq!(
        report.audit_rows_removed, 0,
        "under PurgeCascade::Preserve no audit row is removed"
    );
    assert_eq!(
        report.audit_rows_preserved,
        before.len() as u64,
        "audit_rows_preserved must equal the number of rows the subject had \
         immediately before the call — see PurgeReport's equation"
    );

    let after = backend
        .audit(&scope, &AuditFilter::default())
        .await
        .unwrap();
    let after_ids: BTreeSet<_> = after.iter().map(|r| r.id.clone()).collect();
    assert!(
        before.is_subset(&after_ids),
        "a preserved audit row was destroyed by the purge: had {before:?}, kept {after_ids:?}"
    );

    // The discriminating clause. The purge's own record must be present — and
    // must NOT be inside `audit_rows_preserved`, which counts only rows that
    // existed before the call. Without this, a backend that counts its own row
    // passes the equation above and the equation is decorative.
    let purged_row = after
        .iter()
        .find(|r| r.event == AuditEvent::SubjectPurged)
        .expect("the purge must insert its own SubjectPurged record under Preserve too");
    assert_eq!(
        purged_row.id, purge_id,
        "the SubjectPurged row was written under a minted id, not the one given"
    );
    assert_eq!(
        after.len(),
        before.len() + 1,
        "the audit must hold exactly the preserved rows plus the purge's own \
         record; audit_rows_preserved ({}) must not count that record",
        report.audit_rows_preserved
    );

    // Idempotency records go under Preserve as well: replaying the purged
    // subject's key must write, not replay. Left until last because it
    // repopulates the scope.
    let replacement = fx::item(&scope, "written under an idempotency key");
    let mut retry = fx::admit_txn(&scope, replacement.clone(), None);
    retry.idempotency_key = Some("survives-a-purge?".into());
    retry.payload_digest = Some(replacement.digest());
    let applied = backend.apply(retry).await.unwrap();
    assert!(
        !applied.replayed,
        "the purged subject's idempotency record outlived the purge, so a later \
         write replayed an outcome for an item that no longer exists"
    );
    assert_eq!(
        backend.list(&scope, &Page::default()).await.unwrap().len(),
        1,
        "the post-purge write was swallowed by a surviving idempotency record"
    );
}

/// The echo rule for `apply` — see the rule itself on the `Backend` trait.
///
/// **The implementation this exists to reject: one that lets storage mint the
/// audit row's key** — a `DEFAULT`/autoincrement column, or an
/// `AuditId::new()` inside the insert — while returning the id it was handed.
/// The return value looks correct to every caller and only reading the row
/// back exposes it. Nothing else in this suite reads an audit row's id and
/// compares it to one supplied by the caller.
///
/// **It passes vacuously if** the read-back is dropped: asserting only
/// `applied.audit_id == id` is satisfied by returning the argument, which a
/// backend that wrote no audit row at all also does.
pub async fn apply_persists_the_audit_id_it_was_given<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let id = AuditId::parse("01ARZ3NDEKTSV4RRFFQ69G5FE0").unwrap();
    let mut txn = fx::admit_txn(&scope, fx::item(&scope, "a memory"), None);
    txn.audit.id = id.clone();

    let applied = backend.apply(txn).await.unwrap();
    assert_eq!(
        applied.audit_id, id,
        "apply returned an AuditId other than the one its transaction carried"
    );

    let rows = backend
        .audit(&scope, &AuditFilter::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "apply wrote no audit row at all");
    assert_eq!(
        rows[0].id, id,
        "apply returned the given AuditId but stored the row under a different \
         one — `order by id` over this table now means two things"
    );
}

/// The echo rule for `record_recall` — see the rule itself on the `Backend`
/// trait.
///
/// **The implementation this exists to reject: one that updates the items'
/// access statistics and returns `record.id` without ever inserting the audit
/// row.** That is not a strawman — `record_recall`'s contract gained the
/// statistics obligation second, and the only other test of this method,
/// `retrieval::recall_updates_access_statistics`, asserts *only* the
/// item-side effect. Such a backend passes it today: recalls silently vanish
/// from the compliance log while every count and timestamp looks right.
///
/// **It passes vacuously if** the `audit()` read is dropped, or if it is not
/// narrowed to `Recalled`: this scope also holds the admit's own row, so a
/// non-empty assertion over an unfiltered query is satisfied by the admit
/// alone.
pub async fn record_recall_persists_the_audit_id_it_was_given<F: BackendFactory>(factory: &F) {
    use memorysafe_core::ItemRef;

    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let item = fx::item(&scope, "the memory that gets recalled");
    backend
        .apply(fx::admit_txn_embedded(&scope, item.clone()))
        .await
        .unwrap();

    let id = AuditId::parse("01ARZ3NDEKTSV4RRFFQ69G5FE1").unwrap();
    let mut record = AuditRecord::new(
        scope.clone(),
        AuditEvent::Recalled,
        vec![ItemRef::from_item(&item)],
        Actor::system(),
        OffsetDateTime::UNIX_EPOCH + Duration::seconds(3_600),
    );
    record.id = id.clone();

    let returned = backend.record_recall(record).await.unwrap();
    assert_eq!(
        returned, id,
        "record_recall returned an AuditId other than the one its record carried"
    );

    let recalls = backend
        .audit(
            &scope,
            &AuditFilter {
                events: vec![AuditEvent::Recalled],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        recalls.len(),
        1,
        "record_recall wrote no audit row — it updated the access statistics and \
         returned the id, which is all the access-statistics test can see"
    );
    assert_eq!(
        recalls[0].id, id,
        "record_recall stored the recall under a minted AuditId instead of the \
         one it was given"
    );
}

/// The echo rule for `import` — see the rule itself on the `Backend` trait.
///
/// **The implementation this exists to reject: one whose import writes audit
/// rows through the same path as `apply`, letting the row take a fresh id.**
/// It is the natural shape — there is already a "write an audit row" helper —
/// and the report still counts three rows imported, so
/// `export_import_round_trips_exactly`, which compares `audit_imported`
/// against a count, cannot see it. Comparing the id *sets* can. The
/// consequence is concrete: an auditor cannot match a row in the migrated
/// tenant against the same row in the export it came from, and `after`
/// cursors taken before the migration name nothing afterwards.
///
/// **It passes vacuously if** the source has no audit rows — two empty sets
/// are equal — so the source's set is asserted non-empty and of a known size
/// first.
pub async fn import_preserves_every_audit_id<F: BackendFactory>(factory: &F) {
    let source = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    for i in 0..3 {
        source
            .apply(fx::admit_txn_embedded(
                &scope,
                fx::item(&scope, &format!("memory {i}")),
            ))
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

    let source_ids: BTreeSet<_> = source
        .audit(&scope, &AuditFilter::default())
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(
        source_ids.len(),
        3,
        "the source must hold three audit rows, or comparing id sets across the \
         round trip compares nothing"
    );

    let target = factory.create().await;
    let report = target.import(&selector.tenant, exported).await.unwrap();
    assert_eq!(report.audit_imported, 3);

    let target_ids: BTreeSet<_> = target
        .audit(&scope, &AuditFilter::default())
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(
        source_ids, target_ids,
        "import re-minted the audit ids: the imported rows are no longer the rows \
         that were exported, and nothing that referenced them still resolves"
    );
}

/// The echo rule for `purge_subject`, and the ordering of its two writes —
/// see the rule itself on the `Backend` trait.
///
/// One test, three distinct failure modes, each named in the message that
/// catches it:
///
/// 1. **The record is dropped.** A backend that treats `audit` as advisory —
///    deleting the subject's rows and never inserting the one it was handed —
///    leaves an erasure with no record that it happened, which is the single
///    row a regulator asks for.
/// 2. **The id is minted.** The row is written, but under storage's own key,
///    so nothing the caller holds names it.
/// 3. **Insert before delete.** The record is written first and then swept
///    away by the very cascade it is recording — the purge eats its own
///    record. Modes 1 and 3 are indistinguishable by row count, which is
///    exactly why both are named where the count is asserted.
///
/// A fourth, weaker one is covered by the actor assertion: a backend that
/// discards the given record and writes its own `SubjectPurged` row would
/// most plausibly stamp it `Actor::system()`, losing the human who ordered
/// the erasure.
///
/// **It passes vacuously if** the purge is asked to `Preserve`: modes 1 and 3
/// then look identical to a correct run for a different reason (nothing is
/// deleted, so nothing can eat the record), and the count assertion no longer
/// isolates the insert. It cascades deliberately.
pub async fn purge_subject_persists_the_record_it_was_given<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let tenant = TenantId::new("t").unwrap();
    let subject = SubjectId::new("doomed").unwrap();
    let scope = Scope::new("t", "doomed", "ns").unwrap();

    for i in 0..2 {
        backend
            .apply(fx::admit_txn(
                &scope,
                fx::item(&scope, &format!("memory {i}")),
                None,
            ))
            .await
            .unwrap();
    }
    assert_eq!(
        backend
            .audit(&scope, &AuditFilter::default())
            .await
            .unwrap()
            .len(),
        2,
        "two rows must exist for the cascade to have something to sweep, or the \
         insert-before-delete mode cannot be provoked"
    );

    let id = AuditId::parse("01ARZ3NDEKTSV4RRFFQ69G5FE2").unwrap();
    // Not `Actor::system()`: that is what a backend inventing its own record
    // would write, so it could not be told apart from the given one.
    let actor = Actor {
        kind: ActorKind::Human,
        id: Some("dpo-7".into()),
    };
    backend
        .purge_subject(
            &tenant,
            &subject,
            PurgeCascade::Cascade,
            fx::purge_record(&scope, id.clone(), actor.clone()),
        )
        .await
        .unwrap();

    let rows = backend
        .audit(&scope, &AuditFilter::default())
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "after a cascading purge the audit must hold exactly the purge's own \
         record. Zero rows means either the record was dropped, or it was \
         inserted before the delete and swept away by the purge's own cascade; \
         more than one means the cascade did not run"
    );
    assert_eq!(
        rows[0].event,
        AuditEvent::SubjectPurged,
        "the surviving row is not the purge's record"
    );
    assert_eq!(
        rows[0].id, id,
        "the SubjectPurged row was stored under a minted AuditId instead of the \
         one the caller supplied"
    );
    assert_eq!(
        rows[0].actor, actor,
        "the SubjectPurged row does not name the actor from the record it was \
         given — the backend wrote a record of its own instead of the one \
         handed to it"
    );
}

/// `ScopeSelector`'s optional fields actually narrow the export.
///
/// **The implementation this rejects:** one whose `export` selects on
/// `sel.tenant` alone and ignores `sel.subject` and `sel.namespace`. That is
/// not a strawman — it is the natural SQLite shape, where a tenant *is* one
/// database file and "export this tenant" is the whole query, so the two
/// optional fields have nothing obvious to do. It passes every other test in
/// this suite: `export_import_round_trips_exactly`, `import_is_idempotent`
/// and `import_preserves_every_audit_id` all set `subject: None,
/// namespace: None`, which is precisely the selector a tenant-wide export
/// answers correctly.
///
/// Both directions are asserted for each field, because presence alone
/// certifies the ignoring backend: it exports everything, so everything
/// wanted is present. The absence assertions are what fail it.
///
/// **The two fields are narrowed one at a time, in two separate exports.** A
/// single export setting both would be passed by a backend that honours
/// `namespace` and ignores `subject` (or the reverse), since the corpus is
/// arranged so either filter alone still excludes some records.
///
/// **A fourth export repeats the narrowing with `include_audit: true`**, and
/// that one carries a disclosure consequence rather than a correctness one.
/// Narrowing enforced over `Item` records alone leaves a backend free to emit
/// the whole tenant's audit table beside one subject's items — one subject's
/// export carrying another subject's audit rows, in a product whose promise is
/// that this cannot happen. Bodies are never in an audit row; ids, digests and
/// feature numbers are.
///
/// **Vacuity:** the test proves nothing if the corpus lives in a single
/// namespace or belongs to a single subject — there would be nothing for a
/// narrowing selector to exclude, and "only the matching records came back"
/// would be true of the whole tenant. The fixture therefore spans two
/// namespaces *and* two subjects, and asserts before exporting that all four
/// items are readable, so a backend that simply stored nothing cannot pass by
/// exporting an empty stream. The audit half has the mirror trap: absence
/// alone is satisfied by a backend that emits no audit rows at all under
/// `include_audit: true`, so the wanted row's presence is asserted too.
///
/// `include_audit: false` is not re-tested here — that direction is already
/// covered by `import_is_idempotent`, which exports with the flag off and
/// asserts the target's audit is empty after importing the stream.
pub async fn export_narrows_to_the_selectors_subject_and_namespace<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let tenant = TenantId::new("t").unwrap();

    // Four items across two subjects and two namespaces, so that narrowing on
    // either axis alone still leaves something out.
    let mine_a = Scope::new("t", "s", "ns-a").unwrap();
    let mine_b = Scope::new("t", "s", "ns-b").unwrap();
    let theirs_a = Scope::new("t", "other", "ns-a").unwrap();
    let theirs_b = Scope::new("t", "other", "ns-b").unwrap();

    let mut ids = Vec::new();
    let mut audit_ids = Vec::new();
    for scope in [&mine_a, &mine_b, &theirs_a, &theirs_b] {
        let item = fx::item(scope, &format!("a memory in {}", scope.key()));
        ids.push(item.id.clone());
        let applied = backend
            .apply(fx::admit_txn(scope, item, None))
            .await
            .unwrap();
        // One admit, one audit row, filed under that admit's own scope — so
        // each of the four audit rows is narrowable on exactly the same two
        // axes as its item.
        audit_ids.push(applied.audit_id);
    }
    let (mine_a_id, mine_b_id, theirs_a_id, theirs_b_id) = (
        ids[0].clone(),
        ids[1].clone(),
        ids[2].clone(),
        ids[3].clone(),
    );

    for (scope, id) in [
        (&mine_a, &mine_a_id),
        (&mine_b, &mine_b_id),
        (&theirs_a, &theirs_a_id),
        (&theirs_b, &theirs_b_id),
    ] {
        assert!(
            backend.get(scope, id).await.unwrap().is_some(),
            "the corpus must exist before an export can be said to narrow it"
        );
    }

    // Every `ItemId` an export stream carries, in order of appearance.
    fn exported_ids(stream: &[ExportRecord]) -> Vec<memorysafe_core::ItemId> {
        stream
            .iter()
            .filter_map(|r| match r {
                ExportRecord::Item { item, .. } => Some(item.id.clone()),
                _ => None,
            })
            .collect()
    }

    // Narrow on namespace only: both subjects' `ns-a` items, neither `ns-b`.
    let by_namespace = backend
        .export(&ScopeSelector {
            tenant: tenant.clone(),
            subject: None,
            namespace: Some(memorysafe_core::Namespace::new("ns-a").unwrap()),
            include_audit: false,
        })
        .await
        .unwrap();
    let got = exported_ids(&by_namespace);
    assert!(
        got.contains(&mine_a_id) && got.contains(&theirs_a_id),
        "namespace: Some(ns-a) dropped an item that is in ns-a"
    );
    assert!(
        !got.contains(&mine_b_id) && !got.contains(&theirs_b_id),
        "namespace: Some(ns-a) exported an item from ns-b — the selector's \
         namespace was ignored and the whole tenant came back"
    );
    assert_eq!(
        got.len(),
        2,
        "namespace: Some(ns-a) must export exactly the two ns-a items"
    );

    // Narrow on subject only: both of `s`'s namespaces, neither of `other`'s.
    let by_subject = backend
        .export(&ScopeSelector {
            tenant: tenant.clone(),
            subject: Some(SubjectId::new("s").unwrap()),
            namespace: None,
            include_audit: false,
        })
        .await
        .unwrap();
    let got = exported_ids(&by_subject);
    assert!(
        got.contains(&mine_a_id) && got.contains(&mine_b_id),
        "subject: Some(s) dropped an item belonging to s"
    );
    assert!(
        !got.contains(&theirs_a_id) && !got.contains(&theirs_b_id),
        "subject: Some(s) exported another subject's item — the selector's \
         subject was ignored"
    );
    assert_eq!(
        got.len(),
        2,
        "subject: Some(s) must export exactly the two items s owns"
    );

    // And both together select the single item at their intersection, so a
    // backend that honours one field by accident of the other is caught.
    let both = backend
        .export(&ScopeSelector {
            tenant,
            subject: Some(SubjectId::new("s").unwrap()),
            namespace: Some(memorysafe_core::Namespace::new("ns-a").unwrap()),
            include_audit: false,
        })
        .await
        .unwrap();
    assert_eq!(
        exported_ids(&both),
        vec![mine_a_id.clone()],
        "subject and namespace together must select their intersection, one item"
    );

    // **The audit half, and it is the half with a disclosure consequence.**
    // Every export above sets `include_audit: false`, and the one other test
    // that sets it `true` (`export_orders_the_stream_by_kind_then_by_id`)
    // narrows nothing — so without this fourth export nothing in the suite
    // combines a narrowing selector with audit records, and a backend that
    // narrows items while emitting the whole tenant's audit table passes.
    // That is one subject's export carrying another subject's audit rows.
    // Bodies are never in an audit row, but ids, content digests and feature
    // numbers are, and "subject X's export contains only subject X's data" is
    // the promise this product is sold on.
    let with_audit = backend
        .export(&ScopeSelector {
            tenant: TenantId::new("t").unwrap(),
            subject: Some(SubjectId::new("s").unwrap()),
            namespace: Some(memorysafe_core::Namespace::new("ns-a").unwrap()),
            include_audit: true,
        })
        .await
        .unwrap();
    let exported_audit: Vec<AuditId> = with_audit
        .iter()
        .filter_map(|r| match r {
            ExportRecord::Audit { audit } => Some(audit.id.clone()),
            _ => None,
        })
        .collect();
    assert!(
        exported_audit.contains(&audit_ids[0]),
        "include_audit: true dropped the audit row of the very scope the \
         selector names — narrowing must exclude other scopes, not everything"
    );
    for (i, label) in [
        (1usize, "the same subject's other namespace"),
        (2, "another subject, same namespace"),
        (3, "another subject, another namespace"),
    ] {
        assert!(
            !exported_audit.contains(&audit_ids[i]),
            "the export of one scope carried an audit row from {label}: \
             `ScopeSelector` narrowed the items and not the audit rows, which \
             is a cross-subject disclosure in the export path"
        );
    }
    assert_eq!(
        exported_audit.len(),
        1,
        "exactly one of the four audit rows belongs to (s, ns-a)"
    );
    // And the items are still narrowed when audit is switched on, so a
    // backend cannot trade one for the other.
    assert_eq!(exported_ids(&with_audit), vec![mine_a_id]);
}

/// `Backend::audit` returns exactly `min(filter.limit, rows still matching)`
/// — pinned from both sides in one test, because either side alone leaves the
/// other free.
///
/// **The implementation the short-page half rejects:** one that applies
/// `limit` *before* `events`, taking the newest `limit` rows and filtering
/// them afterwards. That is what you get from paging a materialised "recent
/// audit" view, or from `SELECT * FROM (SELECT ... ORDER BY id DESC LIMIT ?)
/// WHERE event = ?`, and it is invisible whenever the matching rows happen to
/// be the newest ones. Here exactly one row of six matches and it is
/// deliberately not among the newest three, so such a backend returns an
/// empty page for a query with a matching row in the log — the caller
/// concludes the subject was never touched. The same assertion also rejects a
/// backend that treats a short page as an error, or that loops fetching until
/// it has `limit` rows (which never terminates on an exhausted log): both
/// come from reading `limit` as a promise of page size rather than a bound.
///
/// **The over-limit half** — a limit below the number of matching rows must
/// truncate to exactly the limit — is not new coverage on its own;
/// `audit_filter_narrows_by_event_and_time` already asserts `limit: 2`
/// returns two of four rows. It is here so that one test pins the whole
/// formula rather than half of it: `min` is a claim about both arguments, and
/// a suite that checks each side in a different test can lose one without
/// noticing the other has become unconstrained.
///
/// **Vacuity:** the short-page half proves nothing if the matching rows ever
/// number at or above its limit, because `min` would then be the limit on
/// both sides and the two halves would test the same thing. It also proves
/// nothing if the eviction's audit id is among the newest three, since the
/// limit-then-filter backend would find it and pass — which is exactly why
/// the ids come from `fx::AUDIT_TRUNCATION_ULIDS` (eviction at index 1,
/// ascending, four admits above it) rather than from `AuditRecord::new`'s
/// generator, whose ids for six records written microseconds apart are
/// ordered randomly relative to each other.
pub async fn audit_returns_min_of_the_limit_and_the_rows_that_remain<F: BackendFactory>(
    factory: &F,
) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let ids: Vec<AuditId> = fx::AUDIT_TRUNCATION_ULIDS
        .iter()
        .map(|u| AuditId::parse(u).expect("literal must be a canonical ULID"))
        .collect();

    // Row 0: an admit, so the eviction below has something to evict.
    let doomed = fx::item(&scope, "the first memory, soon evicted");
    let mut first = fx::admit_txn(&scope, doomed.clone(), None);
    first.audit.id = ids[0].clone();
    backend.apply(first).await.unwrap();

    // Row 1: the eviction — the only `Forgotten` row, and by id the second
    // oldest of the six.
    let mut evict = fx::evict_txn(&scope, vec![doomed.id.clone()]);
    evict.audit.id = ids[1].clone();
    let evicted = backend.apply(evict).await.unwrap();
    assert_eq!(
        evicted.audit_id, ids[1],
        "apply must persist and return the AuditId it was given (the echo rule \
         on `Backend`); every assertion below names the ids this test supplied"
    );

    // Rows 2..5: four more admits, all newer than the eviction by id.
    for (i, id) in ids.iter().enumerate().skip(2) {
        let mut txn = fx::admit_txn(&scope, fx::item(&scope, &format!("memory {i}")), None);
        txn.audit.id = id.clone();
        backend.apply(txn).await.unwrap();
    }

    // Fewer rows remain than the limit allows: all of them come back, and the
    // short page is the caller's "exhausted" signal rather than an error.
    let all = backend
        .audit(
            &scope,
            &AuditFilter {
                limit: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        all.len(),
        6,
        "a limit above the number of rows must return every row — not pad, not \
         truncate to some internal batch size, not error"
    );
    assert!(
        all.len() < 100,
        "the point of this half is that the page is short; if the corpus ever \
         reaches the limit it stops being the case under test"
    );

    // The sharp version: a filter matching one row, under a limit of three.
    let forgotten = backend
        .audit(
            &scope,
            &AuditFilter {
                events: vec![AuditEvent::Forgotten],
                limit: 3,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        forgotten.len(),
        1,
        "one row matches and the limit is three, so exactly that row must come \
         back. An empty page here means `limit` was applied before `events`: \
         the newest three rows are all admits, so filtering them afterwards \
         finds nothing and the caller is told the eviction never happened"
    );
    assert_eq!(
        forgotten[0].id, ids[1],
        "the row returned is not the eviction this test wrote"
    );
    assert_eq!(forgotten[0].event, AuditEvent::Forgotten);

    // The complementary side: more rows match than the limit allows.
    let limited = backend
        .audit(
            &scope,
            &AuditFilter {
                events: vec![AuditEvent::Admitted],
                limit: 2,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        limited.len(),
        2,
        "five rows match and the limit is two, so the page must be exactly two"
    );
    assert_eq!(
        limited.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        vec![ids[5].clone(), ids[4].clone()],
        "a truncated page must be the newest rows by AuditId, newest first — \
         not an arbitrary two of the five that match"
    );
}

/// `AuditFilter::after` pages **backwards down the id order**: the next page
/// holds only ids strictly less than the cursor.
///
/// `Backend::audit`'s doc comment carried, until this test, the note that
/// "`filter.after` appears in no conformance test: the cursor is entirely
/// untested, so this doc comment is the only thing pinning down both its
/// direction and this rule." This is that test, and that doc now names it. It
/// amends no contract — the rule was already written down; nothing enforced
/// it.
///
/// **The implementations this rejects**, both of which keep the suite green
/// today:
///
/// - *`id > after`.* "After" reads as "later" in English and as `>` in SQL, so
///   a descending log paged with `id > after` is the single likeliest mistake
///   here. It re-serves the rows the caller has already seen and never reaches
///   the older ones: paging never terminates and the log looks infinite.
/// - *A temporal cursor* — `at > cursor_row.at`, or `at < it` — which is what
///   you get from reading `after` as a point in time rather than a position in
///   the returned order. This corpus is arranged so the two disagree: the
///   eviction carries the largest `AuditId` while its `at` sits between the
///   second and third admit's, so an `at`-based page 2 returns a different set
///   from an id-based one rather than the same set in another order.
/// - *`id <= after`*, an off-by-one that returns the cursor row a second time.
///   Caught by the strictness assertion rather than by the disjointness one,
///   since a duplicated cursor row is one shared id, not a shared page.
///
/// **Vacuity:** every assertion below holds trivially if the log has no more
/// rows than the limit — page 1 is then the whole log, page 2 is empty, and
/// "disjoint" and "strictly less" are true of nothing. The fixture writes four
/// rows and pages at `limit: 2`, so there are two full pages and a third,
/// empty one that proves termination. It would also prove nothing if the ids
/// ascended in the same order as `at`: an `at`-cursor backend would then
/// produce identical pages. `fx::AUDIT_ORDER_ULIDS` pins the ids, and the
/// eviction's `at` is placed *between* two admits so the two orders genuinely
/// disagree — the same construction, and the same reason, as
/// `audit_filter_narrows_by_event_and_time`.
pub async fn audit_pages_by_the_after_cursor_without_repeating_a_row<F: BackendFactory>(
    factory: &F,
) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let ids: Vec<AuditId> = fx::AUDIT_ORDER_ULIDS
        .iter()
        .map(|u| AuditId::parse(u).expect("literal must be a canonical ULID"))
        .collect();

    // Three admits at 0s/10s/20s, then an eviction whose business timestamp is
    // 15s — earlier than the third admit's — while its id is the largest of
    // the four. Id order and `at` order therefore disagree.
    let t0 = OffsetDateTime::UNIX_EPOCH;
    for (i, at) in [t0, t0 + Duration::seconds(10), t0 + Duration::seconds(20)]
        .into_iter()
        .enumerate()
    {
        let mut txn = fx::admit_txn(
            &scope,
            fx::item_at(&scope, &format!("memory {i}"), at),
            None,
        );
        txn.audit.id = ids[i].clone();
        backend.apply(txn).await.unwrap();
    }
    let items = backend.list(&scope, &Page::default()).await.unwrap();
    let mut evict = fx::evict_txn_at(
        &scope,
        vec![items[0].id.clone()],
        t0 + Duration::seconds(15),
    );
    evict.audit.id = ids[3].clone();
    backend.apply(evict).await.unwrap();

    let page = |after: Option<AuditId>| {
        let scope = scope.clone();
        let backend = &backend;
        async move {
            backend
                .audit(
                    &scope,
                    &AuditFilter {
                        after,
                        limit: 2,
                        ..Default::default()
                    },
                )
                .await
                .unwrap()
        }
    };

    let first = page(None).await;
    let first_ids: Vec<AuditId> = first.iter().map(|r| r.id.clone()).collect();
    assert_eq!(
        first_ids,
        vec![ids[3].clone(), ids[2].clone()],
        "the first page must be the two largest AuditIds, newest first"
    );

    let cursor = first_ids.last().expect("page 1 is not empty").clone();
    let second = page(Some(cursor.clone())).await;
    let second_ids: Vec<AuditId> = second.iter().map(|r| r.id.clone()).collect();

    assert!(
        second_ids.iter().all(|id| *id < cursor),
        "every row after the cursor must have a strictly smaller AuditId: \
         `id < after`, not `id > after` and not `id <= after`. Got {second_ids:?} \
         against cursor {cursor}"
    );
    let first_set: BTreeSet<_> = first_ids.iter().cloned().collect();
    let second_set: BTreeSet<_> = second_ids.iter().cloned().collect();
    assert!(
        first_set.is_disjoint(&second_set),
        "the second page re-served a row from the first: paging by the cursor \
         must not repeat"
    );
    assert_eq!(
        second_ids,
        vec![ids[1].clone(), ids[0].clone()],
        "the second page must be the remaining two rows, still newest first"
    );

    // The two pages together are the whole log, in descending id order — so
    // the cursor skipped nothing on the way down.
    let mut walked = first_ids.clone();
    walked.extend(second_ids.iter().cloned());
    assert_eq!(
        walked,
        vec![
            ids[3].clone(),
            ids[2].clone(),
            ids[1].clone(),
            ids[0].clone()
        ],
        "paging by the cursor must visit every row exactly once, descending"
    );

    // And paging terminates: a page shorter than `limit` means exhausted.
    let third = page(Some(ids[0].clone())).await;
    assert!(
        third.is_empty(),
        "there is nothing below the smallest id, so the page after it must be \
         empty — a non-empty page here means the cursor is not being applied"
    );
    assert!(
        third.len() < 2,
        "a page shorter than `limit` is the only signal a caller has that the \
         log is exhausted"
    );
}

/// `AuditFilter::since` and `until` are **inclusive**: a record timestamped
/// exactly on either bound matches.
///
/// `Backend::audit`'s doc comment carried, until this test, the note that
/// "the conformance suite's window test deliberately places both bounds off
/// every record's timestamp, so it cannot tell an inclusive backend from an
/// exclusive one."
/// That placement in `audit_filter_narrows_by_event_and_time` is deliberate
/// and stays as it is — it tests the window *without* depending on the
/// inclusivity choice, which is a property worth keeping. This is the separate
/// test that does depend on it.
///
/// **The implementation this rejects:** one whose SQL reads
/// `at > since AND at < until`. Exclusive bounds are what you get from writing
/// the comparison out by hand without deciding the edges, and the choice is
/// invisible in every other test in this suite. Against this fixture such a
/// backend returns one row where the contract requires three — the two
/// boundary records are exactly the ones it drops.
///
/// **Vacuity:** the test proves nothing unless the boundary records are the
/// *extremes* of the expected result. If `since` sat on an interior record's
/// timestamp, an exclusive backend would still return the earlier ones and the
/// set would differ by nothing observable at the edge. So `since` is placed
/// exactly on the earliest expected record and `until` exactly on the latest.
/// It would also prove nothing if no record lay outside the window, since
/// "everything came back" is then true of a backend that ignores the bounds
/// entirely: one record sits strictly below `since` and one strictly above
/// `until`, and both are asserted absent.
pub async fn audit_since_and_until_include_a_record_on_the_boundary<F: BackendFactory>(
    factory: &F,
) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    // Five records ten seconds apart. The window is [10s, 30s]: the records at
    // 10s and 30s are the boundaries and must be returned; 0s and 40s are
    // outside and must not be.
    let t0 = OffsetDateTime::UNIX_EPOCH;
    let mut written: Vec<(OffsetDateTime, AuditId)> = Vec::new();
    for step in 0..5u8 {
        let at = t0 + Duration::seconds(i64::from(step) * 10);
        let applied = backend
            .apply(fx::admit_txn(
                &scope,
                fx::item_at(&scope, &format!("memory at {step}0s"), at),
                None,
            ))
            .await
            .unwrap();
        written.push((at, applied.audit_id));
    }

    let since = written[1].0;
    let until = written[3].0;
    let returned = backend
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

    let got: BTreeSet<AuditId> = returned.iter().map(|r| r.id.clone()).collect();
    let expected: BTreeSet<AuditId> = written[1..=3].iter().map(|(_, id)| id.clone()).collect();
    assert_eq!(
        got, expected,
        "since and until are inclusive bounds: the records timestamped exactly \
         at {since} and at {until} must both be returned, along with the one \
         between them. An exclusive backend returns only the middle record"
    );
    assert_eq!(
        returned.len(),
        3,
        "three of the five records fall inside the closed window"
    );
    assert!(
        !got.contains(&written[0].1) && !got.contains(&written[4].1),
        "a record outside the window leaked through, so the bounds are not \
         being applied at all"
    );
}

/// `export` emits `Header` first, then `Item`s ascending by `ItemId`, then
/// `Audit` rows ascending by `AuditId` — and the sections do not interleave.
///
/// `Backend::export`'s doc comment carried, until this test, the note that
/// "`export_import_round_trips_exactly` never inspects the export stream
/// itself — it compares `list` output after re-sorting both sides by
/// `ItemId`, so no conformance test observes the stream's order at all." This
/// is the test that observes it, and that doc now names it. The cost of leaving it unobserved is stated
/// on the trait: two backends exporting the same data in different orders
/// produce byte-different artifacts, so a customer checksumming a migration
/// cannot verify it.
///
/// **The implementation this rejects:** one that emits rows in whatever order
/// its storage returns them — `SELECT ... FROM items` with no `ORDER BY`,
/// which SQLite answers in rowid order, i.e. insertion order. Also one that
/// walks items and audit rows together in a single pass (a union view, or a
/// per-item "row then its audit trail" loop), which produces a correctly
/// ordered stream by every check except the section boundary.
///
/// **Vacuity, and here it is the whole difficulty:** `ItemId` is a ULID minted
/// at creation, so a corpus built with `fx::item` and inserted in the obvious
/// order is *already* ascending by id. Against such a fixture a backend that
/// sorts nothing at all passes, and the test certifies the defect it exists to
/// catch. Both corpora below are therefore built from pinned literal ids with
/// `fx::item_with_id` and inserted in a deliberately non-ascending order —
/// `2, 0, 1` — for the items *and* for their audit rows, and the premise that
/// insertion order differs from id order is asserted rather than assumed. The
/// "non-decreasing" assertions would also be vacuous over zero or one record
/// of a kind, so the fixture writes three of each and asserts both counts.
pub async fn export_orders_the_stream_by_kind_then_by_id<F: BackendFactory>(factory: &F) {
    use memorysafe_core::ItemId;

    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    // Both sequences come from fixture constants rather than inlined
    // literals, so the premise test and its consumer are joined by a symbol
    // and not by string equality: `fx::ITEM_ORDER_ULIDS` and
    // `fx::AUDIT_ORDER_ULIDS`, each checked ascending by its own test in
    // `fixtures::tests` today. Neither sequence is generated, because
    // generated ULIDs ascend with insertion and would hide the defect under
    // test.
    let item_ids: Vec<ItemId> = fx::ITEM_ORDER_ULIDS[..3]
        .iter()
        .map(|s| ItemId::parse(s).expect("literal must be a canonical ULID"))
        .collect();
    let audit_ids: Vec<AuditId> = fx::AUDIT_ORDER_ULIDS[..3]
        .iter()
        .map(|s| AuditId::parse(s).expect("literal must be a canonical ULID"))
        .collect();

    // Insertion order 2, 0, 1 — non-ascending on both id sequences at once.
    let insertion = [2usize, 0, 1];
    for i in insertion {
        let item = fx::item_with_id(&scope, item_ids[i].clone(), &format!("memory {i}"));
        let mut txn = fx::admit_txn(&scope, item, None);
        txn.audit.id = audit_ids[i].clone();
        backend.apply(txn).await.unwrap();
    }

    // The premise, asserted rather than assumed: a backend that emits rows in
    // insertion order must produce something other than ascending id order.
    let inserted: Vec<ItemId> = insertion.iter().map(|i| item_ids[*i].clone()).collect();
    let mut ascending = inserted.clone();
    ascending.sort();
    assert_ne!(
        inserted, ascending,
        "the corpus must be inserted out of id order, or a backend that sorts \
         nothing passes this test"
    );

    let stream = backend
        .export(&ScopeSelector {
            tenant: TenantId::new("t").unwrap(),
            subject: None,
            namespace: None,
            include_audit: true,
        })
        .await
        .unwrap();

    assert!(
        matches!(stream.first(), Some(ExportRecord::Header { .. })),
        "the stream must open with the Header: {stream:?}"
    );

    let mut items: Vec<ItemId> = Vec::new();
    let mut audits: Vec<AuditId> = Vec::new();
    let mut last_item_at: Option<usize> = None;
    let mut first_audit_at: Option<usize> = None;
    for (position, record) in stream.iter().enumerate() {
        match record {
            ExportRecord::Item { item, .. } => {
                items.push(item.id.clone());
                last_item_at = Some(position);
            }
            ExportRecord::Audit { audit } => {
                audits.push(audit.id.clone());
                first_audit_at = first_audit_at.or(Some(position));
            }
            ExportRecord::Header { .. } => {}
        }
    }

    assert_eq!(
        items.len(),
        3,
        "three items were written; with fewer than two an ordering assertion \
         proves nothing"
    );
    assert_eq!(
        audits.len(),
        3,
        "include_audit: true, and three audit rows were written"
    );
    assert_eq!(
        items, ascending,
        "Item records must be emitted ascending by ItemId. This corpus was \
         inserted in the order 2, 0, 1, so a backend emitting its natural row \
         order returns that instead"
    );
    // Against the pinned literals, ascending — not against a sorted copy of
    // the returned list, which any three ids satisfy by construction and which
    // would have made this half of the test unfalsifiable.
    let mut audits_ascending = audit_ids.clone();
    audits_ascending.sort();
    assert_eq!(
        audits, audits_ascending,
        "Audit records must be emitted ascending by AuditId, and their \
         insertion order was 2, 0, 1 as well"
    );
    // Unwrapped rather than compared as `Option`s: `None < Some(_)` holds, so
    // an empty section would satisfy the comparison without satisfying the
    // rule. The counts above already rule that out; this makes it structural.
    let last_item_at = last_item_at.expect("three Item records were emitted");
    let first_audit_at = first_audit_at.expect("three Audit records were emitted");
    assert!(
        last_item_at < first_audit_at,
        "every Item must precede every Audit row: the two sections must not \
         interleave, or two backends emitting the same records in the same \
         per-section order still produce different bytes"
    );
}

/// Ten aggregate rows spanning two days, both policy states, six `Some`
/// policies and two event classes — the corpus both aggregate-cursor tests
/// page over. Returns the tenant it wrote into.
///
/// Two of the six policies exist for the version hazard (`baseline@10` before
/// `baseline@9` as text, the reverse as numbers), two for the collation hazard
/// (`B@1` before `a@1` as bytes, the reverse under any locale-aware
/// collation), and two for the render collision: `("a@b", "c")` and
/// `("a", "b@c")` are different policies that `Display` renders identically as
/// `"a@b@c"`. A backend keying its aggregate table on the rendered string
/// cannot tell them apart at all and merges their counts into one row, which
/// the corpus size assertion catches deterministically rather than as a
/// coin-flip ordering failure.
///
/// Aggregates are produced by the write path, not by an API of their own: each
/// `apply` writes one audit row and increments the aggregate keyed by that
/// row's tenant, its decision's policy (`None` when the record carries no
/// decision), its event class, and `day_bucket(record.at)`. So the corpus is
/// built by choosing those four things per transaction.
async fn seed_aggregate_corpus<B: Backend>(backend: &B) -> TenantId {
    use memorysafe_core::{Decision, PolicyId, Reason, ReasonCode};

    let tenant = TenantId::new("t").unwrap();
    let scope = Scope::new("t", "s", "n").unwrap();
    let day0 = OffsetDateTime::UNIX_EPOCH;
    let day1 = day0 + Duration::days(1);

    let decision = |policy: PolicyId| {
        Decision::retain(
            policy,
            Reason::new(ReasonCode::HighValue, "seeded", Default::default()),
        )
    };
    let v10 = PolicyId::new("baseline", "10");
    let v9 = PolicyId::new("baseline", "9");
    // `PolicyId::new` validates neither field, so an uppercase name is a legal
    // policy — and this is the pair that separates byte order from any
    // locale-aware collation with certainty rather than by locale: `B` is 0x42
    // and `a` is 0x61, while every locale orders them the other way.
    //
    // (*Which engine defaults to which is recollection, unverified here — no
    // database was available. The assertion below does not depend on it: it
    // asserts byte order, which the trait mandates explicitly, so it is right
    // whatever the defaults turn out to be.*) Validation would not have helped:
    // `validate_component` permits `-`, `_` and `.`, and glibc collations
    // reweight punctuation, so a validated component is not collation-stable
    // either.
    let upper_b = PolicyId::new("B", "1");
    let lower_a = PolicyId::new("a", "1");
    // Same render, different policies. `Display` is `{name}@{version}` and
    // neither field is constrained, so both of these are `"a@b@c"`.
    let split_early = PolicyId::new("a@b", "c");
    let split_late = PolicyId::new("a", "b@c");

    // (day 0, None, Admitted)
    let a = fx::item_at(&scope, "day zero, no policy", day0);
    backend
        .apply(fx::admit_txn(&scope, a.clone(), None))
        .await
        .unwrap();

    // (day 0, Some(baseline@10), Admitted)
    let b = fx::item_at(&scope, "day zero, policy ten", day0);
    let mut txn = fx::admit_txn(&scope, b.clone(), None);
    txn.audit = txn.audit.clone().with_decision(decision(v10.clone()));
    backend.apply(txn).await.unwrap();

    // (day 0, Some(baseline@9), Admitted)
    let c = fx::item_at(&scope, "day zero, policy nine", day0);
    let mut txn = fx::admit_txn(&scope, c, None);
    txn.audit = txn.audit.clone().with_decision(decision(v9));
    backend.apply(txn).await.unwrap();

    // (day 0, Some(B@1), Admitted)
    let e = fx::item_at(&scope, "day zero, uppercase policy", day0);
    let mut txn = fx::admit_txn(&scope, e, None);
    txn.audit = txn.audit.clone().with_decision(decision(upper_b));
    backend.apply(txn).await.unwrap();

    // (day 0, Some(a@1), Admitted)
    let f = fx::item_at(&scope, "day zero, lowercase policy", day0);
    let mut txn = fx::admit_txn(&scope, f, None);
    txn.audit = txn.audit.clone().with_decision(decision(lower_a));
    backend.apply(txn).await.unwrap();

    // The render-collision pair: two keys whose policies render identically.
    // **Their events differ deliberately**, and the two orders disagree because
    // of it. Correct order compares the policy *parts*, so `("a", "b@c")`
    // precedes `("a@b", "c")` — `"a"` is a prefix of `"a@b"` — regardless of
    // event. A backend that stores the parts but orders by the rendered
    // `policy_name || '@' || policy_version` sees the two tie on day and on
    // policy, falls through to the event, and puts `admitted` before
    // `forgotten` — the opposite. With both rows carrying the same event that
    // backend ties on all three components and emits them in an arbitrary
    // order, so it would be caught only about half the time; this makes it
    // every time. (The other wrong implementation — a single rendered key
    // column — merges them into one row and is caught by the corpus size.)
    let g = fx::item_at(&scope, "day zero, name carries the separator", day0);
    let g_id = g.id.clone();
    let mut txn = fx::admit_txn(&scope, g, None);
    txn.audit = txn.audit.clone().with_decision(decision(split_early));
    backend.apply(txn).await.unwrap();

    let mut txn = fx::evict_txn_at(&scope, vec![g_id], day0);
    txn.audit = txn.audit.clone().with_decision(decision(split_late));
    backend.apply(txn).await.unwrap();

    // (day 1, None, Admitted)
    let d = fx::item_at(&scope, "day one, no policy", day1);
    backend.apply(fx::admit_txn(&scope, d, None)).await.unwrap();

    // (day 1, None, Forgotten)
    backend
        .apply(fx::evict_txn_at(&scope, vec![a.id.clone()], day1))
        .await
        .unwrap();

    // (day 1, Some(baseline@10), Forgotten)
    let mut txn = fx::evict_txn_at(&scope, vec![b.id.clone()], day1);
    txn.audit = txn.audit.clone().with_decision(decision(v10));
    backend.apply(txn).await.unwrap();

    tenant
}

/// Paging `audit_aggregates` by `AuditAggregateFilter::after` visits every row
/// exactly once, in the documented order, across a corpus that includes
/// policy-less rows.
///
/// **The implementations this rejects**, all three of which pass every other
/// test in this suite:
///
/// - *A row-value comparison written from the struct's field order.*
///   `Backend::audit_aggregates` documents the order as `day`, then `policy`,
///   then the event's serialised name. `AggregateKey` **declares** `tenant,
///   policy, event, day` — `day` is documented first and declared last — so
///   the natural `(a, b, c, d) < (w, x, y, z)` written off the struct compares
///   `day` last. Over a corpus spanning two days that misorders the sweep, and
///   because the cursor comparison and the `ORDER BY` then disagree, rows
///   repeat or vanish.
/// - *Postgres's default NULL ordering.* "`None` sorts before every `Some`" is
///   SQLite's default and the **opposite** of Postgres's, which sorts nulls
///   larger than any non-null for `ASC`. A plain `ORDER BY policy` therefore
///   conforms in SQLite and inverts in Postgres, and Postgres needs an explicit
///   `NULLS FIRST`. Worse, a naive row-value comparison against a NULL
///   component evaluates to NULL rather than false, so the row is dropped and
///   the page comes back short — and a short page is defined to *mean* the log
///   is exhausted, so the sweep terminates early and silently, on the majority
///   of the key space.
/// - *A numerically compared version column.* Two `Some`s compare by name and
///   then by version, each as text, so `baseline@10` precedes `baseline@9`. A
///   backend storing version as a number reverses exactly that pair.
/// - *A policy key built from the rendered `name@version`.* `Display` is not
///   injective — `PolicyId` constrains neither field — so `("a@b", "c")` and
///   `("a", "b@c")` both render `"a@b@c"`. A backend keying its aggregate
///   table on that string merges two distinct policies' counts into one row,
///   and one ordering by it returns `Equal` for keys that `==` calls
///   different. The order is over the two parts, name then version.
/// - *A locale collation on the policy column.* That comparison is over
///   **bytes**, and byte order is forced rather than preferred: this test
///   measures a backend against `AggregateKey`'s `Ord`, which compares through
///   Rust's `String: Ord`. SQLite's default TEXT collation is `BINARY` and
///   Postgres's is the database's, which is locale-aware by default, so `B@1`
///   precedes `a@1` on one and follows it on the other. Postgres conforms only
///   with `COLLATE "C"` (`ucs_basic`) — and on both text columns of the key
///   comparison, not only `policy`, even though the `tenant` one is
///   unreachable here because `audit_aggregates` takes the tenant as a
///   parameter.
///
/// **This test compares the backend against `AggregateKey`'s `Ord`**, not
/// against a sequence written out here — a third copy of the order would be a
/// third thing to drift. That makes `Ord` itself load-bearing, so it is pinned
/// separately by `aggregates::tests::aggregate_keys_order_by_day_then_policy_
/// then_event_not_by_field_order`, whose assertions are written from the
/// method's doc comment rather than from the impl. The two endpoint assertions
/// below are the exception: they are read straight from the prose and do not
/// route through `Ord` at all.
///
/// **Vacuity.** The sweep proves nothing if the page size is not smaller than
/// the corpus (no cursor is ever used), if the corpus spans one day (the
/// field-order confusion is invisible), if it contains no policy-less row (the
/// NULL hazard is invisible, and it is the majority of the key space), or if
/// the two `Some` policies sort the same numerically and lexically (the
/// version hazard is invisible), or if no policy name reaches outside the
/// range where byte order and locale collation agree (the collation hazard is
/// invisible), or if no two policies render to the same string (the
/// collision is invisible, and after the ordering changed to compare name and
/// version separately nothing else would pin that it did). The fixture is ten
/// rows over two days, swept two at a time, with three policy-less rows, the
/// `baseline@9` / `baseline@10` pair that inverts under numeric comparison,
/// the `B@1` / `a@1` pair that inverts under any locale-aware collation, and
/// the `("a@b", "c")` / `("a", "b@c")` pair that renders identically — the last
/// of which carries two different events, so that a backend ordering by the
/// render breaks the resulting tie on the event and lands the wrong way round
/// deterministically rather than by coin flip.
pub async fn audit_aggregates_page_in_the_documented_order<F: BackendFactory>(factory: &F) {
    use crate::{AggregateKey, AuditAggregate};

    let backend = factory.create().await;
    let tenant = seed_aggregate_corpus(&backend).await;

    let page_size = 2;
    let mut swept: Vec<AuditAggregate> = Vec::new();
    let mut cursor: Option<AggregateKey> = None;
    let mut terminated = false;
    for _ in 0..10 {
        let page = backend
            .audit_aggregates(
                &tenant,
                &AuditAggregateFilter {
                    after: cursor.clone(),
                    limit: page_size,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            page.len() <= page_size,
            "a page may never exceed the limit it was given"
        );
        let short = page.len() < page_size;
        cursor = page.last().map(|a| a.key.clone());
        swept.extend(page);
        if short {
            terminated = true;
            break;
        }
    }
    assert!(
        terminated,
        "paging never reached a short page: either the cursor is not being \
         applied, or `min(limit, remaining)` is being padded"
    );

    assert_eq!(
        swept.len(),
        10,
        "the corpus is ten distinct keys and every one must be visited exactly \
         once. A smaller number means the sweep terminated early — or that the \
         two policies rendering as `a@b@c` were keyed on the render and merged \
         into one row, which is a lost count rather than a paging bug. A larger \
         one means the cursor re-served rows it had already returned"
    );
    assert!(
        page_size < swept.len(),
        "the page size must be smaller than the corpus, or no cursor is ever \
         used and this test degenerates into a single unpaginated read"
    );

    let keys: Vec<AggregateKey> = swept.iter().map(|a| a.key.clone()).collect();
    let unique: BTreeSet<AggregateKey> = keys.iter().cloned().collect();
    assert_eq!(
        unique.len(),
        keys.len(),
        "the sweep returned a duplicate key"
    );

    let mut canonical = keys.clone();
    canonical.sort();
    assert_eq!(
        keys, canonical,
        "the pages must arrive in the documented order — ascending by day, \
         then policy (None before every Some, two Somes by name and then \
         version), then the event's serialised name"
    );

    // Read straight from the prose rather than through `Ord`: the earliest day
    // comes first, and within it the policy-less row precedes every policied
    // one. If `Ord` were wrong these two would still be right.
    assert_eq!(
        keys.first().map(|k| (k.day, k.policy.clone())),
        Some((0, None)),
        "the first row must be the earliest day's policy-less one: day is the \
         leading component and None sorts before every Some"
    );
    assert!(
        keys.last()
            .is_some_and(|k| k.day == 1 && k.policy.is_some()),
        "the last row must be the later day's policied one"
    );

    // The premise the NULL hazard rests on: policy-less rows are actually in
    // the corpus. Without them a backend that mishandles NULL passes.
    assert_eq!(
        keys.iter().filter(|k| k.policy.is_none()).count(),
        3,
        "three of the corpus's rows carry no policy; if that ever reaches zero \
         this test stops exercising the ordering hazard it exists for"
    );
    assert_eq!(
        keys.iter()
            .filter(|k| k.day == 0)
            .filter(|k| k.policy.is_some())
            .count(),
        6,
        "day zero must hold all six policied rows, or one of the three hazards \
         — version comparison, collation, or the render collision — is not \
         exercised at all"
    );

    // The render collision, asserted by name from the documented rule and not
    // through `Ord`: policy compares by **name** and then by **version**, as
    // two components, so `("a", "b@c")` precedes `("a@b", "c")` because `"a"`
    // is a prefix of `"a@b"`. An ordering built on `Display` cannot separate
    // these — both render `"a@b@c"` — so it returns `Equal` for keys that are
    // not equal, and a backend storing the render as its key never has two
    // rows here to order.
    let by_parts = |name: &str, version: &str| {
        keys.iter().position(|k| {
            k.policy
                .as_ref()
                .is_some_and(|p| p.name == name && p.version == version)
        })
    };
    let (late, early) = (by_parts("a", "b@c"), by_parts("a@b", "c"));
    assert!(
        late.is_some() && early.is_some(),
        "both halves of the render-collision pair must be present as separate \
         rows: a backend that keyed on the rendered policy string merged them"
    );
    assert!(
        late < early,
        "policy compares by name then version, so ('a', 'b@c') precedes \
         ('a@b', 'c') — 'a' is a prefix of 'a@b'. A backend ordering by the \
         rendered `name@version` ties these two, falls through to the event, \
         and returns them the other way round: their events differ precisely so \
         that fallback is wrong every time rather than arbitrary"
    );

    // The collation pair, asserted by position rather than through `Ord`:
    // policies compare as bytes, so `B@1` (0x42) precedes `a@1` (0x61). Every
    // locale-aware collation reverses this, so a Postgres backend that leaves
    // the column at the database's default collation fails here and a SQLite
    // backend passes on its BINARY default — which is the cross-backend split
    // this pair exists to make visible.
    let position = |name: &str| {
        keys.iter()
            .position(|k| k.policy.as_ref().is_some_and(|p| p.to_string() == name))
    };
    let (b_at, a_at) = (position("B@1"), position("a@1"));
    assert!(
        b_at.is_some() && a_at.is_some(),
        "both halves of the collation pair must be in the swept corpus"
    );
    assert!(
        b_at < a_at,
        "policies order by byte order, not by the database's collation: B@1 \
         precedes a@1. Postgres needs COLLATE \"C\" (ucs_basic) on that \
         column; its default locale collation puts a@1 first"
    );

    // An unpaginated read must produce the same rows in the same order — a
    // backend whose paged order differs from its unpaged order is paging over
    // a different query than it answers.
    let whole = backend
        .audit_aggregates(
            &tenant,
            &AuditAggregateFilter {
                limit: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        whole.iter().map(|a| a.key.clone()).collect::<Vec<_>>(),
        keys,
        "paging and a single unpaginated read must agree, on contents and on \
         order"
    );
    assert_eq!(
        whole.iter().map(|a| a.count).sum::<u64>(),
        10,
        "ten audit rows, ten distinct keys, one event counted in each"
    );
}

/// `AuditAggregateFilter::after` names a **coordinate in the key space**, not
/// a stored row: a cursor whose key was never written still resumes correctly.
///
/// **The implementation this rejects:** one that resolves `after` to a stored
/// row — looking it up by equality, or storing a row id and seeking to it —
/// and pages from that row's position. It is the row-id cursor
/// `AuditAggregateFilter::after`'s own doc comment rejects, arriving by
/// another route: the doc rejected it because "a row-id cursor would name
/// nothing once its row was gone, and a caller paging through would see the
/// log end early for a reason unrelated to their query." Handed a key it
/// cannot find, such a backend returns an empty page (the log looks exhausted
/// three rows early) or ignores the cursor and returns everything.
///
/// **This cannot be folded into the sweep test.** Every cursor a sweep
/// produces came from a row the backend just returned, so a lookup-based
/// implementation resolves all of them and passes. Only a key that was never
/// stored distinguishes the two, and no paging fixture can generate one.
///
/// **Vacuity.** The test proves nothing if the cursor coincides with a stored
/// key (it is then just another page of the sweep), if it sorts below every
/// row (the whole corpus comes back, which a backend ignoring the cursor also
/// returns), or if it sorts above every row (an empty page, which the broken
/// backend also returns). So the cursor is placed strictly *between* stored
/// rows with three on each side, and both sides are asserted: the three below
/// absent, the three above present and in order. That the cursor names no
/// stored row is asserted rather than assumed.
pub async fn audit_aggregates_resume_from_a_cursor_that_names_no_stored_row<F: BackendFactory>(
    factory: &F,
) {
    use crate::AggregateKey;
    use memorysafe_core::PolicyId;

    let backend = factory.create().await;
    let tenant = seed_aggregate_corpus(&backend).await;

    let all = backend
        .audit_aggregates(
            &tenant,
            &AuditAggregateFilter {
                limit: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        all.len(),
        10,
        "the corpus must exist before it can be resumed into"
    );

    // Day 0, policy `nonexistent@1.0.0`: no such row was ever written, and it
    // sorts above every policy day zero holds — the names are "B", "a",
    // "a@b" and "baseline", all below "nonexistent" in byte order — so this
    // coordinate sits after all seven of day zero's rows and before all three
    // of day one's.
    let phantom = AggregateKey {
        tenant: tenant.clone(),
        policy: Some(PolicyId::new("nonexistent", "1.0.0")),
        event: AuditEvent::Rejected,
        day: 0,
    };
    assert!(
        all.iter().all(|a| a.key != phantom),
        "the cursor must name a key that was never stored, or this test is \
         indistinguishable from an ordinary page of the sweep"
    );

    let resumed = backend
        .audit_aggregates(
            &tenant,
            &AuditAggregateFilter {
                after: Some(phantom.clone()),
                limit: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert!(
        !resumed.is_empty(),
        "resuming from a key that names no stored row returned nothing: the \
         cursor is being looked up as a row instead of compared as a value, so \
         the log ends early for a reason unrelated to the caller's query"
    );
    assert!(
        resumed.iter().all(|a| a.key.day == 1),
        "every row strictly greater than a day-zero coordinate carrying a \
         policy above all of day zero's is a day-one row; a day-zero row came \
         back, so the cursor was ignored"
    );
    let expected: Vec<AggregateKey> = {
        let mut k: Vec<AggregateKey> = all
            .iter()
            .map(|a| a.key.clone())
            .filter(|k| *k > phantom)
            .collect();
        k.sort();
        k
    };
    assert_eq!(expected.len(), 3, "three rows sort above the cursor");
    assert_eq!(
        resumed.iter().map(|a| a.key.clone()).collect::<Vec<_>>(),
        expected,
        "resuming must return exactly the rows strictly greater than the \
         cursor, in the documented order"
    );
}

/// `AuditAggregateFilter`'s day window and its `policy` narrowing both narrow.
///
/// **Why this was missing.** `AuditAggregateFilter`'s doc says it "mirrors
/// `AuditFilter`'s shape ... so there is one idiom for a bounded, resumable
/// read across this trait rather than two", and `AuditFilter`'s `since`/`until`
/// *are* covered — by `audit_filter_narrows_by_event_and_time`. The mirror
/// carried the shape across and not the coverage: every
/// `AuditAggregateFilter` literal in this suite set only `after` and `limit`,
/// so a backend that ignored the day bounds and the policy narrowing and
/// returned the whole set passed. A claim of structural similarity transfers
/// structure, not tests, and a coverage check that reads the sentence sees a
/// tested sibling and moves on.
///
/// **The implementations this rejects:**
///
/// - *Day bounds ignored.* Returns all seven rows where the window admits two.
/// - *Day bounds treated as exclusive.* `since` and `until` are documented as
///   **inclusive**, in whole-UTC-day units. The window here is `[1, 1]` — a
///   single day, both bounds landing exactly on it — so an exclusive backend
///   returns nothing at all. This is a deliberate departure from
///   `audit_filter_narrows_by_event_and_time`, which places its bounds *off*
///   every record's timestamp precisely so it does not depend on the
///   inclusivity choice. That is right for a test of the window; it leaves the
///   choice itself unpinned, and days are integers, so there is no "between two
///   values" position available here that would still select a subset. Pinning
///   it is therefore both possible and necessary, and this is where it happens.
/// - *`policy` narrowing ignored.* Returns the policy-less rows too.
/// - *`policy` matched on name alone.* The corpus holds `alpha@1` and
///   `alpha@2`; a backend comparing only `policy_name` returns both where the
///   filter names one.
///
/// **Vacuity:** a filter test whose expected set is everything cannot fail, so
/// each of the three queries below selects a strict subset — two of seven, three
/// of seven, one of seven — and each asserts presence *and* absence. Rows sit
/// outside the window on both sides, so a backend clamping only one bound is
/// caught; and the excluded policies include both a different name and the same
/// name at a different version.
pub async fn audit_aggregates_narrow_by_day_window_and_policy<F: BackendFactory>(factory: &F) {
    use crate::AggregateKey;
    use memorysafe_core::{Decision, PolicyId, Reason, ReasonCode};

    let backend = factory.create().await;
    let tenant = TenantId::new("t").unwrap();
    let scope = Scope::new("t", "s", "n").unwrap();

    let alpha1 = PolicyId::new("alpha", "1");
    let alpha2 = PolicyId::new("alpha", "2");
    let beta = PolicyId::new("beta", "1");
    let decision = |policy: PolicyId| {
        Decision::retain(
            policy,
            Reason::new(ReasonCode::HighValue, "seeded", Default::default()),
        )
    };

    // Seven rows over three days: day 0 and day 2 sit outside the window on
    // either side, and `alpha@1` appears on all three days so the policy
    // narrowing is not a disguised day filter.
    let rows: [(i64, Option<PolicyId>); 7] = [
        (0, None),
        (0, Some(alpha1.clone())),
        (1, None),
        (1, Some(alpha1.clone())),
        (2, Some(alpha1.clone())),
        (2, Some(alpha2.clone())),
        (2, Some(beta.clone())),
    ];
    for (day, policy) in &rows {
        let at = OffsetDateTime::UNIX_EPOCH + Duration::days(*day);
        let item = fx::item_at(&scope, &format!("day {day} under {policy:?}"), at);
        let mut txn = fx::admit_txn(&scope, item, None);
        if let Some(p) = policy {
            txn.audit = txn.audit.clone().with_decision(decision(p.clone()));
        }
        backend.apply(txn).await.unwrap();
    }

    let read = |filter: AuditAggregateFilter| {
        let tenant = tenant.clone();
        let backend = &backend;
        async move {
            backend
                .audit_aggregates(&tenant, &filter)
                .await
                .unwrap()
                .into_iter()
                .map(|a| a.key)
                .collect::<Vec<AggregateKey>>()
        }
    };

    let everything = read(AuditAggregateFilter {
        limit: 100,
        ..Default::default()
    })
    .await;
    assert_eq!(
        everything.len(),
        7,
        "the corpus must exist before any narrowing means anything, and each of \
         the seven rows must be its own key"
    );

    // The window is a single day, with both bounds exactly on it. Days are
    // integers: there is no position between two of them, so this pins
    // inclusivity rather than avoiding it.
    let windowed = read(AuditAggregateFilter {
        since: Some(1),
        until: Some(1),
        limit: 100,
        ..Default::default()
    })
    .await;
    assert_eq!(
        windowed.len(),
        2,
        "day one holds two rows. Seven means the day bounds were ignored; zero \
         means they were applied exclusively, and `since`/`until` are documented \
         as inclusive"
    );
    assert!(
        windowed.iter().all(|k| k.day == 1),
        "a row outside [1, 1] came back: {windowed:?}"
    );

    // Policy narrowing across every day, so it cannot be a day filter wearing
    // a different name.
    let by_policy = read(AuditAggregateFilter {
        policy: Some(alpha1.clone()),
        limit: 100,
        ..Default::default()
    })
    .await;
    assert_eq!(
        by_policy.len(),
        3,
        "alpha@1 appears once on each of the three days"
    );
    assert!(
        by_policy.iter().all(|k| k.policy.as_ref() == Some(&alpha1)),
        "the policy narrowing let through a row with another policy — or a \
         policy-less row, which `policy: Some(..)` must exclude: {by_policy:?}"
    );
    assert!(
        !by_policy.iter().any(|k| k.policy.as_ref() == Some(&alpha2)),
        "alpha@2 came back for a filter naming alpha@1: the narrowing compares \
         the policy name and ignores the version"
    );

    // Both together, selecting one row of seven.
    let both = read(AuditAggregateFilter {
        since: Some(1),
        until: Some(1),
        policy: Some(alpha1.clone()),
        limit: 100,
        ..Default::default()
    })
    .await;
    assert_eq!(
        both.len(),
        1,
        "the window and the policy narrowing must intersect, not replace one \
         another"
    );
    assert_eq!(both[0].day, 1);
    assert_eq!(both[0].policy.as_ref(), Some(&alpha1));
}
