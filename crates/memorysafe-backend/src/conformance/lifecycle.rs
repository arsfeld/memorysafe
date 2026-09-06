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
