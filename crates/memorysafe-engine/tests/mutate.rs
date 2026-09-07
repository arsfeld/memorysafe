use memorysafe_backend::{Backend, CandidateQuery, HardFilters};
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    Action, AuditEvent, AuditFilter, Namespace, PURGED_COMPONENT, Protection, ReasonCode,
    RecallBudget, RecallMode, RecallRequest, Scope, SensitivityLevel, SubjectId, TenantId,
};
use memorysafe_embed::{DeterministicEmbedder, Embedder};
use memorysafe_engine::{Engine, EngineConfig, EngineError, ForgetSelector, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;
use time::{Duration, OffsetDateTime};

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

#[tokio::test]
async fn forgetting_by_id_removes_the_item_and_audits_it() {
    let e = engine();
    let out = e
        .remember(RememberRequest::new(scope(), "a memory to delete"))
        .await
        .unwrap();
    let id = out.item_id.unwrap();

    let f = e
        .forget(&scope(), ForgetSelector::Ids(vec![id.clone()]))
        .await
        .unwrap();
    assert_eq!(f.forgotten, vec![id]);
    assert!(
        e.review(&scope(), &Default::default())
            .await
            .unwrap()
            .is_empty()
    );

    let audit = e
        .audit(
            &scope(),
            &AuditFilter {
                events: vec![AuditEvent::Forgotten],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(audit.len(), 1);
}

#[tokio::test]
async fn forgetting_by_tag_removes_only_matching_items() {
    let e = engine();
    for (body, tag) in [
        ("alpha note", "work"),
        ("beta note", "home"),
        ("gamma note", "work"),
    ] {
        let mut r = RememberRequest::new(scope(), body);
        r.tags = vec![tag.into()];
        e.remember(r).await.unwrap();
    }

    let f = e
        .forget(&scope(), ForgetSelector::Tag("work".into()))
        .await
        .unwrap();
    assert_eq!(f.forgotten.len(), 2);
    let left = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].body, "beta note");
}

/// `ForgetSelector::Kind` had zero coverage from the brief's own literal
/// tests: only `Ids` and `Tag` were exercised, so a mutation deleting the
/// `.filter(|i| i.kind == kind)` predicate entirely (forgetting everything
/// regardless of kind) passed every other test in the crate. Mirrors
/// `forgetting_by_tag_removes_only_matching_items` exactly, substituting
/// `kind` for `tag`.
#[tokio::test]
async fn forgetting_by_kind_removes_only_matching_items() {
    let e = engine();
    for (body, kind) in [
        ("alpha note", "task"),
        ("beta note", "fact"),
        ("gamma note", "task"),
    ] {
        let mut r = RememberRequest::new(scope(), body);
        r.kind = kind.into();
        e.remember(r).await.unwrap();
    }

    let f = e
        .forget(&scope(), ForgetSelector::Kind("task".into()))
        .await
        .unwrap();
    assert_eq!(f.forgotten.len(), 2);
    let left = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].body, "beta note");
}

/// `ForgetOutcome.forgotten` must report what the backend actually removed
/// (`applied.evicted`), not the selector's raw target list (`targets`).
/// Substituting `targets` for `applied.evicted` survives every other test in
/// this file, because none of them ever asks to forget the same id twice —
/// `applied.evicted` de-duplicates (a second `items::delete` on an
/// already-gone row reports `size == 0` and is not pushed), while a raw
/// `targets` list would still hold both copies.
#[tokio::test]
async fn forgetting_the_same_id_twice_reports_it_once() {
    let e = engine();
    let id = e
        .remember(RememberRequest::new(scope(), "duplicate target"))
        .await
        .unwrap()
        .item_id
        .unwrap();

    let f = e
        .forget(&scope(), ForgetSelector::Ids(vec![id.clone(), id.clone()]))
        .await
        .unwrap();
    assert_eq!(
        f.forgotten,
        vec![id],
        "a duplicated selector must not be reported as two removals"
    );
}

/// The existence pre-check on `ForgetSelector::Ids`
/// (`self.backend.get(scope, &id).await?.is_some()`, in `mutate.rs`) looks
/// removable: `ForgetOutcome` is unchanged with or without it, since it is
/// built from what the backend actually evicted, never from the raw
/// selector. It is not removable: it is defence in depth against a
/// `Backend` whose eviction does not cascade a removed item's other rows —
/// its vector row, most concretely — under the same scope predicate as the
/// row itself. A backend failing that property would let naming another
/// scope's item id (same tenant) leave that item's row untouched but
/// silently strip its vector. This seeds an item in a second scope, forgets
/// from a different scope naming that item's id, and confirms the item
/// survives both as a row (`review`) and as a vector (`Backend::neighbours`
/// against its own recomputed embedding) — the property `ForgetOutcome`
/// alone cannot show.
#[tokio::test]
async fn forgetting_an_id_from_another_scope_does_not_delete_its_vector() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(SqliteBackend::open(dir.keep()));
    let embedder = Arc::new(DeterministicEmbedder::new(256));
    let e = Engine::new(EngineConfig::new(
        backend.clone(),
        embedder.clone(),
        Arc::new(BaselinePolicy::default()),
    ));

    let scope_b = Scope::new("acme", "user-99", "agent").unwrap();
    let body = "a memory that belongs to an entirely different scope";
    let id_b = e
        .remember(RememberRequest::new(scope_b.clone(), body))
        .await
        .unwrap()
        .item_id
        .unwrap();

    // Forget from scope A, naming B's id — same tenant, different scope.
    let f = e
        .forget(&scope(), ForgetSelector::Ids(vec![id_b.clone()]))
        .await
        .unwrap();
    assert!(
        f.forgotten.is_empty(),
        "an id belonging to another scope must never be reported as forgotten"
    );

    // B's item row must survive.
    let still_there = e.review(&scope_b, &Default::default()).await.unwrap();
    assert_eq!(
        still_there.len(),
        1,
        "cross-scope forget must not delete another scope's item"
    );

    // And B's vector must survive too.
    let embedding = embedder.embed(body).unwrap();
    let neighbours = backend.neighbours(&scope_b, &embedding, 5).await.unwrap();
    assert!(
        neighbours.iter().any(|c| c.item.id == id_b),
        "the vector for an item outside the forget scope must survive"
    );
}

#[tokio::test]
async fn forgetting_something_that_does_not_exist_is_not_an_error() {
    let e = engine();
    let f = e
        .forget(
            &scope(),
            ForgetSelector::Ids(vec![memorysafe_core::ItemId::new()]),
        )
        .await
        .unwrap();
    assert!(f.forgotten.is_empty());
}

#[tokio::test]
async fn pinning_an_item_survives_a_later_capacity_squeeze() {
    let e = engine();
    let out = e
        .remember(RememberRequest::new(scope(), "never forget this one"))
        .await
        .unwrap();
    let id = out.item_id.unwrap();

    e.protect(&scope(), &id, Protection::Pinned).await.unwrap();

    e.set_budget(
        &scope(),
        memorysafe_core::Budget {
            max_items: Some(1),
            max_bytes: None,
        },
    )
    .await
    .unwrap();
    for i in 0..4 {
        e.remember(RememberRequest::new(
            scope(),
            &format!("filler memory {i} about topic {i}"),
        ))
        .await
        .unwrap();
    }

    let left = e.review(&scope(), &Default::default()).await.unwrap();
    assert!(
        left.iter().any(|i| i.id == id),
        "a pinned item was evicted under capacity pressure"
    );
}

/// The brief's literal test only checked the total audit *count* (2), which a
/// mutation substituting the wrong event, the wrong stored protection, the
/// wrong `WriteOutcome::action`, or the wrong `ReasonCode` all survive
/// unnoticed — the count stays 2 regardless of what `protect` actually did.
/// This strengthens the original assertion (kept intact) with four that each
/// pin one value-deciding line.
#[tokio::test]
async fn protecting_writes_its_own_audit_record() {
    let e = engine();
    let id = e
        .remember(RememberRequest::new(scope(), "worth protecting"))
        .await
        .unwrap()
        .item_id
        .unwrap();

    // Floored to whole seconds: storage round-trips `OffsetDateTime` as Unix
    // seconds (see the plan's Global Constraints), so a sub-second `until`
    // would fail the stored-value equality check below for a reason that has
    // nothing to do with `protect`.
    let until = (OffsetDateTime::now_utc() + Duration::days(7))
        .replace_nanosecond(0)
        .unwrap();
    let outcome = e
        .protect(&scope(), &id, Protection::Protected { until })
        .await
        .unwrap();

    let audit = e.audit(&scope(), &AuditFilter::default()).await.unwrap();
    assert_eq!(audit.len(), 2, "remember plus protect");

    // `remember`'s own write is `Admitted`, and `protect`'s audit record is
    // built with `AuditEvent::Admitted` too (see `mutate.rs`'s doc comment on
    // why): a mutation substituting a different event for protect's own
    // record leaves the total count at 2 but drops this count to 1.
    let admitted = e
        .audit(
            &scope(),
            &AuditFilter {
                events: vec![AuditEvent::Admitted],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        admitted.len(),
        2,
        "remember's own admit plus protect's admit-shaped record"
    );

    // The outcome must reflect the protection actually requested, not a
    // fixed default — `Action::Retain { protection }` uses the caller's
    // `protection` parameter, and a mutation hardcoding a different value
    // here is otherwise invisible.
    assert_eq!(
        outcome.action,
        Action::Retain {
            protection: Protection::Protected { until }
        },
        "the outcome must report the protection that was actually requested"
    );
    assert_eq!(outcome.reasons[0].code, ReasonCode::Pinned);

    // And the stored item itself must carry that protection — proving the
    // value reached the persisted row, not just the returned outcome.
    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].protection,
        Protection::Protected { until },
        "the stored item must carry the protection that was requested"
    );
}

/// `protect`'s `let Some(mut item) = ... else { return Err(...) }` guard had
/// no covering test: nothing in the brief's literal suite ever calls
/// `protect` on an id that names no item, so deleting the guard (falling
/// through with a fabricated item instead of failing) compiles and passes
/// every other test in the crate.
#[tokio::test]
async fn protecting_a_nonexistent_item_returns_not_found() {
    let e = engine();
    let err = e
        .protect(
            &scope(),
            &memorysafe_core::ItemId::new(),
            Protection::Pinned,
        )
        .await
        .expect_err("protecting an id that names no item must fail");
    assert!(
        matches!(err, EngineError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

/// `protect` deletes the item's row and reinserts it (see the comment at
/// `txn.evictions` in `mutate.rs`). `items::insert` writes `MemoryItem`'s
/// own fields only; the `items` table's `access_count` and `last_access`
/// columns have no counterpart on `MemoryItem`, so they cannot be carried
/// forward and silently revert to schema defaults on every `protect` call.
/// This pins that current behaviour — deliberately not fixed here; see this
/// task's report for whether preserving it is the right long-term answer.
#[tokio::test]
async fn protecting_an_item_resets_its_accumulated_access_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(SqliteBackend::open(dir.keep()));
    let e = Engine::new(EngineConfig::new(
        backend.clone(),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ));

    let id = e
        .remember(RememberRequest::new(
            scope(),
            "a memory with a distinctive marker9000 token",
        ))
        .await
        .unwrap()
        .item_id
        .unwrap();

    // Accumulate real access history through the actual recall path —
    // `Engine::recall` calls `Backend::record_recall`, which increments
    // `access_count` and advances `last_access` for every returned item.
    for _ in 0..3 {
        e.recall(RecallRequest {
            scope: scope(),
            query: Some("marker9000".into()),
            tags_any: vec![],
            kinds: vec![],
            occurred_after: None,
            occurred_before: None,
            mode: RecallMode::WorkingSet,
            budget: RecallBudget {
                max_tokens: Some(4000),
                max_items: Some(5),
            },
            sensitivity_ceiling: SensitivityLevel::Restricted,
        })
        .await
        .unwrap();
    }

    let query = CandidateQuery {
        embedding: None,
        text: Some("marker9000".into()),
        filters: HardFilters {
            sensitivity_ceiling: SensitivityLevel::Restricted,
            ..HardFilters::default()
        },
        limit: 10,
    };
    let before = backend.retrieve_candidates(&scope(), &query).await.unwrap();
    let before_stats = before
        .iter()
        .find(|c| c.item.id == id)
        .expect("the item must be found before protect");
    assert!(
        before_stats.access_count > 0,
        "the recall loop above must have accumulated access history"
    );
    assert!(before_stats.last_accessed_at.is_some());

    e.protect(&scope(), &id, Protection::Pinned).await.unwrap();

    let after = backend.retrieve_candidates(&scope(), &query).await.unwrap();
    let after_stats = after
        .iter()
        .find(|c| c.item.id == id)
        .expect("the item must still be found after protect");
    assert_eq!(
        after_stats.access_count, 0,
        "protect's delete-then-reinsert silently resets access_count"
    );
    assert!(
        after_stats.last_accessed_at.is_none(),
        "protect's delete-then-reinsert silently resets last_accessed_at"
    );
}

#[tokio::test]
async fn purging_a_subject_removes_everything_it_owns() {
    let e = engine();
    let doomed = Scope::new("acme", "doomed", "agent").unwrap();
    let keeper = Scope::new("acme", "keeper", "agent").unwrap();

    for i in 0..3 {
        e.remember(RememberRequest::new(
            doomed.clone(),
            &format!("subject memory {i}"),
        ))
        .await
        .unwrap();
    }
    e.remember(RememberRequest::new(
        keeper.clone(),
        "another subject's memory",
    ))
    .await
    .unwrap();

    let report = e
        .purge_subject(
            &TenantId::new("acme").unwrap(),
            &SubjectId::new("doomed").unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(report.items_removed, 3);
    assert!(
        e.review(&doomed, &Default::default())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        e.review(&keeper, &Default::default()).await.unwrap().len(),
        1
    );
}

/// `Engine::purge_subject` hard-codes `PurgeCascade::Cascade` (deliberately —
/// see `mutate.rs`'s doc comment; Task 38 replaces it with a retention-profile
/// lookup, and this test does not touch that). But no test anywhere asserted
/// what `Cascade` actually does to the subject's *audit* rows, so a mutation
/// substituting `PurgeCascade::Preserve` survived every test in this file:
/// `items_removed` is identical under both, since cascade only governs audit
/// detail. This pins the audit-side half of the contract instead.
#[tokio::test]
async fn purging_a_subject_with_cascade_removes_its_pre_existing_audit_rows() {
    let e = engine();
    let doomed = Scope::new("acme", "doomed-cascade", "agent").unwrap();

    for i in 0..3 {
        e.remember(RememberRequest::new(
            doomed.clone(),
            &format!("cascade memory {i}"),
        ))
        .await
        .unwrap();
    }

    let report = e
        .purge_subject(
            &TenantId::new("acme").unwrap(),
            &SubjectId::new("doomed-cascade").unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        report.audit_rows_removed, 3,
        "PurgeCascade::Cascade must remove the subject's pre-existing audit rows"
    );
    assert_eq!(
        report.audit_rows_preserved, 0,
        "PurgeCascade::Cascade must not preserve any pre-existing audit rows"
    );
}

/// `purge_scope` picks the subject's lexicographically first namespace, but
/// every other test here only ever gives a subject items in a single
/// namespace, so `.next()` and `.last()` are indistinguishable to them.
/// Substituting `.last()` for `.next()` survived every other test in this
/// file. This gives a subject two namespaces, seeded out of lexical order,
/// and checks which one the `SubjectPurged` record actually landed under.
#[tokio::test]
async fn purge_subject_files_its_record_under_the_lexicographically_first_namespace() {
    let e = engine();
    let tenant = TenantId::new("acme").unwrap();
    let subject = SubjectId::new("multi-ns-subject").unwrap();

    let scope_z = Scope {
        tenant: tenant.clone(),
        subject: subject.clone(),
        namespace: Namespace::new("zzz-namespace").unwrap(),
    };
    let scope_a = Scope {
        tenant: tenant.clone(),
        subject: subject.clone(),
        namespace: Namespace::new("aaa-namespace").unwrap(),
    };

    // Seeded out of lexical order: the z-namespace item is written first.
    e.remember(RememberRequest::new(scope_z.clone(), "in the z namespace"))
        .await
        .unwrap();
    e.remember(RememberRequest::new(scope_a.clone(), "in the a namespace"))
        .await
        .unwrap();

    let report = e.purge_subject(&tenant, &subject).await.unwrap();
    assert_eq!(report.items_removed, 2, "both namespaces must be purged");

    let purged_filter = AuditFilter {
        events: vec![AuditEvent::SubjectPurged],
        ..Default::default()
    };
    let audit_a = e.audit(&scope_a, &purged_filter).await.unwrap();
    assert_eq!(
        audit_a.len(),
        1,
        "the purge record must be filed under the lexicographically first namespace"
    );
    let audit_z = e.audit(&scope_z, &purged_filter).await.unwrap();
    assert!(
        audit_z.is_empty(),
        "the purge record must not be filed under a later namespace"
    );
}

/// `purge_scope` falls back to `PURGED_COMPONENT` (`_purged`) when the
/// subject owns no items at all — but no test in this file ever purges an
/// empty subject, so a mutation substituting a different fallback literal
/// survived every other test here.
#[tokio::test]
async fn purging_a_subject_that_owns_nothing_falls_back_to_the_purged_namespace() {
    let e = engine();
    let tenant = TenantId::new("acme").unwrap();
    let subject = SubjectId::new("ghost-subject").unwrap();

    let report = e.purge_subject(&tenant, &subject).await.unwrap();
    assert_eq!(report.items_removed, 0);

    let fallback_scope = Scope {
        tenant: tenant.clone(),
        subject: subject.clone(),
        namespace: Namespace::new(PURGED_COMPONENT).unwrap(),
    };
    let audit = e
        .audit(
            &fallback_scope,
            &AuditFilter {
                events: vec![AuditEvent::SubjectPurged],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        audit.len(),
        1,
        "a subject owning nothing must still get a SubjectPurged record, \
         filed under the reserved fallback namespace"
    );
}
