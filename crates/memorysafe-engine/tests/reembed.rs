use memorysafe_backend::{Backend, Page};
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    ActorKind, AuditEvent, AuditFilter, EmbedderId, Embedding, RecallBudget, RecallMode,
    RecallRequest, Scope, SensitivityLevel,
};
use memorysafe_embed::{DeterministicEmbedder, EmbedError, Embedder};
use memorysafe_engine::{Engine, EngineConfig, REEMBED_BATCH, ReembedCursor, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// An embedder that can be switched off, standing in for a missing model file.
struct FlakyEmbedder {
    inner: DeterministicEmbedder,
    up: AtomicBool,
}

impl FlakyEmbedder {
    fn new() -> Self {
        Self {
            inner: DeterministicEmbedder::new(256),
            up: AtomicBool::new(true),
        }
    }
    fn go_down(&self) {
        self.up.store(false, Ordering::SeqCst);
    }
    fn come_back(&self) {
        self.up.store(true, Ordering::SeqCst);
    }
}

impl Embedder for FlakyEmbedder {
    fn id(&self) -> EmbedderId {
        self.inner.id()
    }
    fn dim(&self) -> u16 {
        self.inner.dim()
    }
    fn embed(&self, text: &str) -> Result<Embedding, EmbedError> {
        if self.up.load(Ordering::SeqCst) {
            self.inner.embed(text)
        } else {
            Err(EmbedError::Unavailable("model file missing".into()))
        }
    }
}

/// Records every text handed to `embed`, so a test can assert that a
/// re-embedding pass really reached the model rather than being served from
/// `EngineCache`'s content-addressed embedding cache — which is keyed on the
/// text alone and would silently make a model migration a no-op.
struct RecordingEmbedder {
    inner: DeterministicEmbedder,
    seen: Mutex<Vec<String>>,
}

impl RecordingEmbedder {
    fn new() -> Self {
        Self {
            inner: DeterministicEmbedder::new(256),
            seen: Mutex::new(Vec::new()),
        }
    }
    fn seen(&self) -> Vec<String> {
        self.seen.lock().expect("embedder log poisoned").clone()
    }
}

impl Embedder for RecordingEmbedder {
    fn id(&self) -> EmbedderId {
        self.inner.id()
    }
    fn dim(&self) -> u16 {
        self.inner.dim()
    }
    fn embed(&self, text: &str) -> Result<Embedding, EmbedError> {
        self.seen
            .lock()
            .expect("embedder log poisoned")
            .push(text.to_string());
        self.inner.embed(text)
    }
}

fn engine_with(embedder: Arc<dyn Embedder>) -> Engine {
    engine_and_backend_with(embedder).0
}

/// The same engine, plus a handle on the backend underneath it.
///
/// `Engine::backend` is `pub(crate)`, and `Engine::recall` cannot answer "does
/// this item have a vector row?" — its hard filters set
/// `exclude_pending_embedding: false`, so a keyword hit alone satisfies it.
/// `Backend::neighbours` is a policy-free pure vector search, so an item
/// appears in its results **iff** a `vectors` row exists for it. That is the
/// only instrument in this file that can tell a written vector from a cleared
/// flag.
fn engine_over(backend: Arc<SqliteBackend>, embedder: Arc<dyn Embedder>) -> Engine {
    Engine::new(EngineConfig::new(
        backend,
        embedder,
        Arc::new(BaselinePolicy::default()),
    ))
}

fn engine_and_backend_with(embedder: Arc<dyn Embedder>) -> (Engine, Arc<SqliteBackend>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(SqliteBackend::open(dir.keep()));
    let engine = Engine::new(EngineConfig::new(
        backend.clone(),
        embedder,
        Arc::new(BaselinePolicy::default()),
    ));
    (engine, backend)
}

/// How many items in the scope have a vector row, probed with `text`'s own
/// embedding. Pure vector search: a `pending_embedding` item, or any item
/// whose vector row was never written, cannot appear here at all.
async fn vector_hits(backend: &Arc<SqliteBackend>, text: &str) -> usize {
    let probe = DeterministicEmbedder::new(256).embed(text).unwrap();
    backend
        .neighbours(&scope(), &probe, 10)
        .await
        .unwrap()
        .len()
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

/// Every `Reembedded` audit row in the scope. `AuditFilter`'s default limit is
/// 100, which is below both `REEMBED_BATCH` and the corpus the paging test
/// uses, so the limit is stated rather than inherited — a short page would
/// otherwise read as "fewer records were written".
async fn reembed_audit_count(e: &Engine) -> usize {
    e.audit(
        &scope(),
        &AuditFilter {
            events: vec![AuditEvent::Reembedded],
            limit: 10_000,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .len()
}

#[tokio::test]
async fn a_write_during_an_outage_is_kept_and_flagged_pending() {
    let flaky = Arc::new(FlakyEmbedder::new());
    let e = engine_with(flaky.clone());

    flaky.go_down();
    let out = e
        .remember(RememberRequest::new(
            scope(),
            "written while the model was missing",
        ))
        .await
        .unwrap();
    assert!(out.item_id.is_some(), "the memory must not be lost");

    let stored = e.review(&scope(), &Page::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert!(
        stored[0].pending_embedding,
        "the item should be flagged for backfill"
    );
}

/// **The vector row is asserted directly, not inferred from recall.**
/// `Engine::recall` sets `exclude_pending_embedding: false` in its hard
/// filters, so this item is reachable by keyword *before* any backfill runs
/// — the `ws.items.len() == 1` at the end of this test would pass just as
/// well against an implementation that cleared the flag and wrote no vector
/// at all. `vector_hits` is what closes that: it is a pure vector search, so
/// the 0-then-1 pair around the backfill is evidence a `vectors` row was
/// created.
#[tokio::test]
async fn backfill_makes_a_pending_item_vector_searchable() {
    let flaky = Arc::new(FlakyEmbedder::new());
    let (e, backend) = engine_and_backend_with(flaky.clone());
    const BODY: &str = "the cat sat on the mat during the outage";

    flaky.go_down();
    e.remember(RememberRequest::new(scope(), BODY))
        .await
        .unwrap();
    flaky.come_back();

    assert_eq!(
        vector_hits(&backend, BODY).await,
        0,
        "the premise: an item admitted during an outage has no vector row"
    );

    let report = e.backfill_embeddings(&scope(), None).await.unwrap();
    assert_eq!(report.embedded, 1);
    assert_eq!(report.still_pending, 0);

    let stored = e.review(&scope(), &Page::default()).await.unwrap();
    assert!(!stored[0].pending_embedding, "the flag should be cleared");
    assert_eq!(
        vector_hits(&backend, BODY).await,
        1,
        "the backfill must write a vector row, not merely clear the flag"
    );

    // And it is now reachable by semantic recall, not just keyword.
    let ws = e
        .recall(RecallRequest {
            scope: scope(),
            query: Some("the cat sat on the mat during the outage".into()),
            tags_any: vec![],
            kinds: vec![],
            occurred_after: None,
            occurred_before: None,
            mode: RecallMode::WorkingSet,
            budget: RecallBudget {
                max_tokens: Some(2000),
                max_items: Some(5),
            },
            sensitivity_ceiling: SensitivityLevel::Restricted,
        })
        .await
        .unwrap();
    assert_eq!(ws.items.len(), 1);
}

#[tokio::test]
async fn backfill_leaves_items_pending_when_the_embedder_is_still_down() {
    let flaky = Arc::new(FlakyEmbedder::new());
    let e = engine_with(flaky.clone());

    flaky.go_down();
    e.remember(RememberRequest::new(scope(), "still no model available"))
        .await
        .unwrap();

    let report = e.backfill_embeddings(&scope(), None).await.unwrap();
    assert_eq!(report.embedded, 0);
    assert_eq!(
        report.still_pending, 1,
        "a failed backfill must not clear the flag"
    );

    let stored = e.review(&scope(), &Page::default()).await.unwrap();
    assert!(stored[0].pending_embedding);
}

/// **A no-op assertion needs a presence control, or it is a test of nothing.**
/// Every number the first half asserts is zero, and an implementation that
/// returned an all-zero report unconditionally would pass it. So the second
/// half drives the *same* engine and the *same* instruments — one
/// `backfill_embeddings` call, one `Reembedded` audit count — into the state
/// where something does happen, and checks that both report it. The premise
/// assertions (`scanned`, and the item actually being non-pending) are there
/// so "nothing to do" cannot be confused with "nothing was looked at".
///
/// The name covers both halves deliberately. A name narrower than its body is
/// the same defect as a body narrower than its name, and this file already
/// contains one instance of the latter.
#[tokio::test]
async fn backfill_does_nothing_over_a_healthy_scope_and_only_the_pending_item_in_a_mixed_one() {
    let flaky = Arc::new(FlakyEmbedder::new());
    let e = engine_with(flaky.clone());
    e.remember(RememberRequest::new(
        scope(),
        "embedded normally at write time",
    ))
    .await
    .unwrap();

    let stored = e.review(&scope(), &Page::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert!(
        !stored[0].pending_embedding,
        "the premise: this scope has nothing to backfill"
    );

    let report = e.backfill_embeddings(&scope(), None).await.unwrap();
    assert_eq!(report.embedded, 0);
    assert_eq!(report.still_pending, 0);
    assert!(report.next_cursor.is_none());
    assert_eq!(
        report.scanned, 1,
        "the premise: the pass must have looked at the item to conclude \
         there was nothing to do"
    );
    assert_eq!(
        reembed_audit_count(&e).await,
        0,
        "a backfill with nothing to do must write no audit record"
    );

    // The presence control. Same engine, same call, same instrument: put one
    // pending item into the scope alongside the healthy one and both the
    // report and the audit log must now show exactly one re-embedding — which
    // also pins the pending-only filter, since embedding both items would
    // read 2 here.
    flaky.go_down();
    e.remember(RememberRequest::new(
        scope(),
        "admitted during an outage and awaiting a vector",
    ))
    .await
    .unwrap();
    flaky.come_back();

    let report = e.backfill_embeddings(&scope(), None).await.unwrap();
    assert_eq!(report.scanned, 2);
    assert_eq!(
        report.embedded, 1,
        "exactly the pending item, and not the healthy one beside it"
    );
    assert_eq!(report.still_pending, 0);
    assert_eq!(reembed_audit_count(&e).await, 1);
}

#[tokio::test]
async fn a_scope_reembed_rewrites_every_vector_and_audits_the_run() {
    let e = engine_with(Arc::new(DeterministicEmbedder::new(256)));
    for i in 0..3 {
        e.remember(RememberRequest::new(
            scope(),
            &format!("memory {i} about subject {i}"),
        ))
        .await
        .unwrap();
    }

    let report = e.reembed_scope(&scope(), None).await.unwrap();
    assert_eq!(report.scanned, 3);
    assert_eq!(
        report.embedded, 3,
        "reembed rewrites every vector, not just pending ones"
    );

    let audit = e
        .audit(
            &scope(),
            &AuditFilter {
                events: vec![AuditEvent::Reembedded],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(audit.len(), 3, "one audited mutation per re-embedded item");
    assert!(
        audit.iter().all(|r| r.actor.kind == ActorKind::System),
        "a migration nobody asked for by hand is the system's own act, and \
         the trail has to say so"
    );
    let mut audited: Vec<&str> = audit
        .iter()
        .flat_map(|r| r.items.iter().map(|i| i.id().as_str()))
        .collect();
    audited.sort_unstable();
    audited.dedup();
    assert_eq!(
        audited.len(),
        3,
        "three records naming three distinct items, not one item audited \
         three times"
    );
}

#[tokio::test]
async fn reembedding_preserves_the_items_themselves() {
    let e = engine_with(Arc::new(DeterministicEmbedder::new(256)));
    for i in 0..3 {
        e.remember(RememberRequest::new(
            scope(),
            &format!("memory {i} about subject {i}"),
        ))
        .await
        .unwrap();
    }
    let before = e.review(&scope(), &Page::default()).await.unwrap();

    let report = e.reembed_scope(&scope(), None).await.unwrap();
    assert_eq!(
        report.embedded, 3,
        "the premise: an unchanged corpus is only evidence if the rows were \
         actually deleted and rewritten"
    );

    let after = e.review(&scope(), &Page::default()).await.unwrap();
    assert_eq!(before, after, "re-embedding must not alter the items");
}

/// **A no-op assertion needs a presence control** — see
/// `backfill_does_nothing_over_a_healthy_scope_and_only_the_pending_item_in_a_mixed_one`
/// for the full argument. Here the same `scanned` counter is driven from an
/// empty scope to a populated one inside one test, so a `scanned` hard-wired
/// to zero cannot pass.
#[tokio::test]
async fn backfill_scans_nothing_on_an_empty_scope_and_one_item_once_it_is_populated() {
    let e = engine_with(Arc::new(DeterministicEmbedder::new(256)));
    assert!(
        e.review(&scope(), &Page::default())
            .await
            .unwrap()
            .is_empty(),
        "the premise: the scope really is empty"
    );

    let report = e.backfill_embeddings(&scope(), None).await.unwrap();
    assert_eq!(report.scanned, 0);
    assert!(report.next_cursor.is_none());

    // The presence control: one item, and the same counter must move.
    e.remember(RememberRequest::new(
        scope(),
        "the scope is no longer empty",
    ))
    .await
    .unwrap();
    let report = e.backfill_embeddings(&scope(), None).await.unwrap();
    assert_eq!(
        report.scanned, 1,
        "the scanned counter must report presence, or its zero above says nothing"
    );
}

/// The migration must reach the model, not the write-time cache.
///
/// `EngineCache`'s embedding cache is keyed on the text alone, with no model
/// in the key. On the write path that is free correctness; here it would be a
/// silent failure — `reembed_scope` exists precisely because the model
/// changed, and an implementation that called `embed_cached` would rewrite
/// every item with the vector the *previous* model produced and report a
/// successful migration that changed nothing. Every body is already in that
/// cache when this pass starts, put there by `remember`, so a second
/// appearance in the log can only come from a direct call.
#[tokio::test]
async fn reembed_scope_reaches_the_embedder_rather_than_the_write_time_cache() {
    let recorder = Arc::new(RecordingEmbedder::new());
    let e = engine_with(recorder.clone());

    let bodies: Vec<String> = (0..3)
        .map(|i| format!("memory {i} about subject {i}"))
        .collect();
    for body in &bodies {
        e.remember(RememberRequest::new(scope(), body))
            .await
            .unwrap();
    }
    assert_eq!(
        recorder.seen(),
        bodies,
        "the premise: each body was embedded exactly once at write time, so \
         every later appearance is this job's own call"
    );

    let report = e.reembed_scope(&scope(), None).await.unwrap();
    assert_eq!(report.embedded, 3);

    let mut expected = bodies.clone();
    expected.extend(bodies.iter().cloned());
    assert_eq!(
        recorder.seen(),
        expected,
        "every item's body must be handed to the embedder a second time"
    );
}

/// Step 3a item 3, decided and pinned: `still_pending` counts items that are
/// **still `pending_embedding` after the pass**, not every target whose
/// embed failed.
///
/// Under `reembed_scope` a failing item that was never pending keeps its
/// existing vector and its `pending_embedding = false` column. Counting it
/// would produce a report claiming one item awaits a backfill while
/// `review()` shows none — and this test is the difference between the two
/// readings.
#[tokio::test]
async fn a_failed_reembed_of_a_healthy_item_is_not_reported_as_still_pending() {
    let flaky = Arc::new(FlakyEmbedder::new());
    let e = engine_with(flaky.clone());
    e.remember(RememberRequest::new(
        scope(),
        "embedded normally at write time",
    ))
    .await
    .unwrap();

    flaky.go_down();
    let report = e.reembed_scope(&scope(), None).await.unwrap();
    assert_eq!(report.scanned, 1, "the premise: the item was a target");
    assert_eq!(report.embedded, 0, "the premise: its re-embed failed");
    assert_eq!(
        report.still_pending, 0,
        "the item was never pending and is not pending now; a report that \
         said otherwise would contradict review()"
    );

    let stored = e.review(&scope(), &Page::default()).await.unwrap();
    assert!(
        !stored[0].pending_embedding,
        "the data the report describes: still not pending"
    );
    assert_eq!(
        reembed_audit_count(&e).await,
        0,
        "nothing was written, so nothing may be audited"
    );
}

/// **The scenario `reembed_scope` exists for: an actual change of embedding
/// model, end to end.**
///
/// Every other test in this file runs one embedder, so none of them can see
/// whether `vectors.embedder` and `vectors.dim` are rewritten — the migration's
/// entire point. Two `DeterministicEmbedder`s of different dimension give two
/// different `EmbedderId`s *and* two different dims, and
/// `SqliteBackend::neighbours` refuses a probe whose model disagrees with what
/// the scope is indexed with (`vectors::scope_embedder`, returning
/// `BackendError::EmbedderMismatch`). That refusal is the instrument: the new
/// model's probe is rejected before the migration and accepted after, and the
/// old model's probe swaps places with it. Both engines share one backend,
/// which is what makes this a migration of an existing corpus rather than two
/// unrelated scopes.
#[tokio::test]
async fn reembed_scope_migrates_a_scope_to_a_new_embedding_model() {
    let old_model = Arc::new(DeterministicEmbedder::new(256));
    let (before, backend) = engine_and_backend_with(old_model.clone());

    let bodies: Vec<String> = (0..3)
        .map(|i| format!("memory {i} about subject {i}"))
        .collect();
    for body in &bodies {
        before
            .remember(RememberRequest::new(scope(), body))
            .await
            .unwrap();
    }

    // The new model. Different dimension, therefore a different `EmbedderId`
    // as well — `DeterministicEmbedder::new` derives its id from its dim.
    let new_model = Arc::new(DeterministicEmbedder::new(384));
    let after = engine_over(backend.clone(), new_model.clone());
    assert_ne!(
        old_model.id(),
        new_model.id(),
        "the premise: this is a change of model, not of instance"
    );

    let old_probe = old_model.embed(&bodies[0]).unwrap();
    let new_probe = new_model.embed(&bodies[0]).unwrap();

    assert!(
        !backend
            .neighbours(&scope(), &old_probe, 10)
            .await
            .unwrap()
            .is_empty(),
        "the premise: the corpus is indexed with the old model and answers it"
    );
    assert!(
        matches!(
            backend.neighbours(&scope(), &new_probe, 10).await,
            Err(memorysafe_backend::BackendError::EmbedderMismatch { .. })
        ),
        "before the migration the new model's probe must be refused, not \
         silently compared across two vector spaces"
    );

    let report = after.reembed_scope(&scope(), None).await.unwrap();
    assert_eq!((report.scanned, report.embedded), (3, 3));

    // The two probes have swapped places: this is the whole migration.
    assert_eq!(
        backend
            .neighbours(&scope(), &new_probe, 10)
            .await
            .unwrap()
            .len(),
        3,
        "after the migration every item answers the new model's probe"
    );
    assert!(
        matches!(
            backend.neighbours(&scope(), &old_probe, 10).await,
            Err(memorysafe_backend::BackendError::EmbedderMismatch { .. })
        ),
        "and the old model's probe is now the one refused"
    );

    assert_eq!(
        reembed_audit_count(&after).await,
        3,
        "an explicit migration is an audited one"
    );
    assert_eq!(
        after
            .review(&scope(), &Page::default())
            .await
            .unwrap()
            .len(),
        3,
        "changing model must not cost the corpus an item"
    );
}

/// Paging, and the arithmetic behind `next_cursor`.
///
/// Three things are pinned here that nothing else can reach:
///
/// 1. **`REEMBED_BATCH` is 200.** The second pass scanning exactly 5 is only
///    true for that value; a batch of 100 would leave 105.
/// 2. **Re-embedding does not disturb pagination.** Each item is deleted and
///    re-inserted, but `Backend::list` orders by `(created_at, id)` — both
///    carried through unchanged — so no row moves. Exactly 205 `Reembedded`
///    records across the two passes is the proof: a shifted row would be
///    scanned twice or never.
/// 3. **The next offset is the cursor it was given plus what it scanned**,
///    not what it scanned alone. The third pass starts from a deliberately
///    non-zero, non-batch-aligned offset, where the two differ; from offset 0
///    they coincide and the bug is invisible.
#[tokio::test]
async fn reembedding_pages_through_a_large_scope_and_resumes_from_its_cursor() {
    let e = engine_with(Arc::new(DeterministicEmbedder::new(256)));
    const TOTAL: usize = 205;
    for i in 0..TOTAL {
        let mut r = RememberRequest::new(scope(), &format!("memory number {i} on subject {i}"));
        r.idempotency_key = Some(format!("seed-{i}"));
        e.remember(r).await.unwrap();
    }
    let full_page = Page {
        offset: 0,
        limit: TOTAL + 10,
    };
    assert_eq!(
        e.review(&scope(), &full_page).await.unwrap().len(),
        TOTAL,
        "the premise: all items admitted as distinct rows, none merged away"
    );

    let first = e.reembed_scope(&scope(), None).await.unwrap();
    assert_eq!(first.scanned, REEMBED_BATCH);
    assert_eq!(first.embedded, REEMBED_BATCH);
    let cursor = first
        .next_cursor
        .expect("205 items over a 200-item batch must page");
    assert_eq!(cursor.offset, 200);

    let second = e.reembed_scope(&scope(), Some(cursor)).await.unwrap();
    assert_eq!(
        second.scanned, 5,
        "205 total minus the 200 already advanced past leaves exactly 5"
    );
    assert_eq!(second.embedded, 5);
    assert!(
        second.next_cursor.is_none(),
        "a short page means there is nothing after it"
    );

    assert_eq!(
        reembed_audit_count(&e).await,
        TOTAL,
        "every item re-embedded exactly once across the two passes: a row \
         that moved under the delete-and-reinsert would be counted twice or \
         missed entirely"
    );
    assert_eq!(
        e.review(&scope(), &full_page).await.unwrap().len(),
        TOTAL,
        "the corpus must survive its own migration"
    );

    // (3) The offset arithmetic, from a cursor where `offset + scanned` and
    // `scanned` differ. `backfill_embeddings` rather than `reembed_scope`
    // because the scope is now healthy, so this pass writes nothing and
    // isolates the cursor from everything else.
    let third = e
        .backfill_embeddings(&scope(), Some(ReembedCursor { offset: 3 }))
        .await
        .unwrap();
    assert_eq!(third.scanned, REEMBED_BATCH);
    assert_eq!(
        third.next_cursor.expect("a full page must page").offset,
        203,
        "the next offset is the cursor it was given plus what it scanned"
    );
}
