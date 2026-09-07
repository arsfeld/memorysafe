use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{AuditEvent, AuditFilter, Budget, Scope};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use time::Duration;

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
async fn an_expired_item_is_removed_and_the_reason_is_recorded() {
    let e = engine();
    let mut r = RememberRequest::new(scope(), "this memory expires immediately");
    r.ttl = Some(Duration::seconds(-1)); // already past
    e.remember(r).await.unwrap();
    e.remember(RememberRequest::new(
        scope(),
        "this one has no expiry at all",
    ))
    .await
    .unwrap();

    let report = e.maintain(&scope(), None).await.unwrap();
    assert_eq!(report.forgotten, 1);

    let left = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].body, "this one has no expiry at all");

    let audit = e
        .audit(
            &scope(),
            &AuditFilter {
                events: vec![AuditEvent::MaintenanceRun],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!audit.is_empty(), "maintenance must write an audit record");
}

#[tokio::test]
async fn maintenance_over_a_healthy_scope_changes_nothing() {
    let e = engine();
    for i in 0..3 {
        e.remember(RememberRequest::new(
            scope(),
            &format!("healthy memory {i} about topic {i}"),
        ))
        .await
        .unwrap();
    }
    let report = e.maintain(&scope(), None).await.unwrap();
    assert_eq!(report.forgotten, 0);
    assert_eq!(
        e.review(&scope(), &Default::default()).await.unwrap().len(),
        3
    );
}

#[tokio::test]
async fn maintenance_resumes_from_its_cursor() {
    let e = engine();
    for i in 0..250 {
        let mut r = RememberRequest::new(scope(), &format!("memory number {i} on subject {i}"));
        r.idempotency_key = Some(format!("seed-{i}"));
        e.remember(r).await.unwrap();
    }

    let first = e.maintain(&scope(), None).await.unwrap();
    assert!(first.next_cursor.is_some(), "a large scope must page");
    assert!(first.scanned > 0);

    let second = e.maintain(&scope(), first.next_cursor).await.unwrap();
    assert!(second.scanned > 0);
    assert!(
        second.next_cursor.is_none() || second.next_cursor.unwrap().offset > first.scanned,
        "the cursor must advance"
    );
}

#[tokio::test]
async fn maintenance_reclaims_an_over_budget_namespace() {
    let e = engine();
    // Seed above budget, then tighten the budget so the scope is over it.
    for i in 0..6 {
        e.remember(RememberRequest::new(
            scope(),
            &format!("memory {i} concerning subject {i}"),
        ))
        .await
        .unwrap();
    }
    e.set_budget(
        &scope(),
        Budget {
            max_items: Some(3),
            max_bytes: None,
        },
    )
    .await
    .unwrap();

    let report = e.maintain(&scope(), None).await.unwrap();
    assert_eq!(
        report.forgotten, 3,
        "6 items against a budget of 3 means 3 reclaimed"
    );
    assert_eq!(
        e.review(&scope(), &Default::default()).await.unwrap().len(),
        3
    );
}

#[tokio::test]
async fn maintenance_on_an_empty_scope_is_a_no_op() {
    let e = engine();
    let report = e.maintain(&scope(), None).await.unwrap();
    assert_eq!(report.scanned, 0);
    assert_eq!(report.forgotten, 0);
    assert!(report.next_cursor.is_none());
}

// --- Task 36's own required `Action::Merge` coverage ------------------------
//
// The five tests above are the brief's mandated minimum; none of them
// exercises a merge. The brief's prose (and the plan's cross-lane contract
// with Task 29) requires this job to *apply* a merge decision — writing the
// merged content to `into`, deleting the absorbed item, and writing one
// `Merged` audit record naming both items, all in one transaction — and
// requires its own test for it, deliberately not satisfied by Task 29's test
// that a merge decision is merely *produced*.
//
// `MergesNamedPolicy` forces a merge by matching on body text, bypassing
// whatever `BaselinePolicy::maintain` would decide on its own: the two
// bodies used below are unrelated enough that neither admission-time
// near-duplicate merging nor `BaselinePolicy`'s own maintenance heuristics
// would fold them together on their own, so the merge below is entirely a
// product of this job applying the forced decision, not a coincidence of the
// baseline policy's thresholds.

struct MergesNamedPolicy {
    baseline: BaselinePolicy,
    absorbed_body: String,
    into_body: String,
    strategy: memorysafe_core::MergeStrategy,
}

impl memorysafe_core::GovernancePolicy for MergesNamedPolicy {
    fn id(&self) -> memorysafe_core::PolicyId {
        memorysafe_core::PolicyId::new("test-forced-merge", "0.0.1")
    }

    fn assess(
        &self,
        cand: &memorysafe_core::Candidate,
        ctx: &memorysafe_core::AssessContext,
    ) -> Result<memorysafe_core::Assessment, memorysafe_core::PolicyError> {
        self.baseline.assess(cand, ctx)
    }

    fn admit(
        &self,
        assessed: &memorysafe_core::Assessed,
        ctx: &memorysafe_core::AdmitContext,
    ) -> Result<memorysafe_core::Decision, memorysafe_core::PolicyError> {
        self.baseline.admit(assessed, ctx)
    }

    fn compose(
        &self,
        _req: &memorysafe_core::RecallRequest,
        _candidates: &[memorysafe_core::ScoredCandidate],
        _ctx: &memorysafe_core::ComposeContext,
    ) -> Result<memorysafe_core::WorkingSet, memorysafe_core::PolicyError> {
        unimplemented!("maintain tests never call compose")
    }

    fn maintain(
        &self,
        ctx: &memorysafe_core::MaintainContext,
    ) -> Result<Vec<memorysafe_core::Decision>, memorysafe_core::PolicyError> {
        let absorbed = ctx.batch.iter().find(|c| c.item.body == self.absorbed_body);
        let into = ctx.batch.iter().find(|c| c.item.body == self.into_body);
        Ok(match (absorbed, into) {
            (Some(a), Some(t)) => vec![memorysafe_core::Decision {
                subject: Some(a.item.id.clone()),
                action: memorysafe_core::Action::Merge {
                    into: t.item.id.clone(),
                    strategy: self.strategy,
                },
                evictions: vec![],
                reasons: vec![memorysafe_core::Reason::new(
                    memorysafe_core::ReasonCode::HighRedundancy,
                    "test-forced merge",
                    memorysafe_core::features! {},
                )],
                policy: self.id(),
            }],
            _ => vec![],
        })
    }
}

fn engine_with<P: memorysafe_core::GovernancePolicy + 'static>(policy: P) -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(policy),
    ))
}

#[tokio::test]
async fn maintenance_applies_a_merge_decision_writing_one_merged_audit_record() {
    let into_body = "a completely unrelated fact about river deltas";
    let absorbed_body = "the cat sat on the mat";
    let e = engine_with(MergesNamedPolicy {
        baseline: BaselinePolicy::default(),
        absorbed_body: absorbed_body.into(),
        into_body: into_body.into(),
        strategy: memorysafe_core::MergeStrategy::AppendAndUnion,
    });

    let into_outcome = e
        .remember(RememberRequest::new(scope(), into_body))
        .await
        .unwrap();
    // Tagged and given an attr, so the merge's tag/attr union is observable:
    // `items::merge` unions the absorbed side's tags and attrs into the
    // target's, regardless of strategy, and nothing else here would catch a
    // merge that read the wrong item's tags or attrs into that union.
    let mut absorbed_req = RememberRequest::new(scope(), absorbed_body);
    absorbed_req.tags = vec!["from-absorbed".into()];
    absorbed_req
        .attrs
        .insert("from_absorbed".into(), serde_json::json!(true));
    let absorbed_outcome = e.remember(absorbed_req).await.unwrap();
    let into_id = into_outcome
        .item_id
        .expect("the target must be admitted, not merged or rejected");
    let absorbed_id = absorbed_outcome
        .item_id
        .expect("the absorbed item must be admitted, not merged or rejected");
    assert_ne!(
        into_id, absorbed_id,
        "the premise needs two distinct stored items to merge"
    );
    assert_eq!(
        e.review(&scope(), &Default::default()).await.unwrap().len(),
        2,
        "both items must land as separate rows before maintenance runs"
    );

    let report = e.maintain(&scope(), None).await.unwrap();
    assert_eq!(report.consolidated, 1, "the merge decision must be applied");

    let left = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(
        left.len(),
        1,
        "the absorbed item must be gone; only the target remains"
    );
    assert_eq!(left[0].id, into_id);
    assert!(
        left[0].body.contains(absorbed_body) && left[0].body.contains(into_body),
        "AppendAndUnion must fold both bodies into the target, got: {}",
        left[0].body
    );
    assert!(
        left[0].tags.contains(&"from-absorbed".to_string()),
        "the absorbed item's tags must be unioned into the target, got: {:?}",
        left[0].tags
    );
    assert_eq!(
        left[0].attrs.get("from_absorbed"),
        Some(&serde_json::json!(true)),
        "the absorbed item's attrs must be unioned into the target, got: {:?}",
        left[0].attrs
    );

    let audit = e
        .audit(
            &scope(),
            &AuditFilter {
                events: vec![AuditEvent::Merged],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(audit.len(), 1, "exactly one Merged audit record");
    let ids: std::collections::BTreeSet<_> =
        audit[0].items.iter().map(|r| r.id().clone()).collect();
    assert_eq!(
        ids.len(),
        2,
        "the merged audit record must name both items, not just the target"
    );
    assert!(ids.contains(&into_id), "must name the merge target");
    assert!(ids.contains(&absorbed_id), "must name the absorbed item");
}

#[tokio::test]
async fn maintenance_merge_replace_body_strategy_overwrites_the_targets_content() {
    let into_body = "an old fact nobody references any more";
    let absorbed_body = "the fresher replacement content";
    let e = engine_with(MergesNamedPolicy {
        baseline: BaselinePolicy::default(),
        absorbed_body: absorbed_body.into(),
        into_body: into_body.into(),
        strategy: memorysafe_core::MergeStrategy::ReplaceBody,
    });

    e.remember(RememberRequest::new(scope(), into_body))
        .await
        .unwrap();
    e.remember(RememberRequest::new(scope(), absorbed_body))
        .await
        .unwrap();

    let report = e.maintain(&scope(), None).await.unwrap();
    assert_eq!(report.consolidated, 1);

    let left = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(
        left[0].body, absorbed_body,
        "ReplaceBody must overwrite, not append to, the target's content"
    );
}

// --- The merge arm's own defensive guards ------------------------------
//
// `Engine::maintain`'s merge arm adds three guards beyond the brief's literal
// text: pinning is enforced (a merge deletes the absorbed item exactly as an
// eviction does, so the same pin defence applies), a self-referential merge
// is refused (the item names itself as its own target), and a merge into a
// nonexistent target is refused (mirroring `remember`'s own
// `validate::merge_target` check). Each needs its own test — mutation testing
// this task's report runs found all three unguarded by the two tests above.

#[tokio::test]
async fn maintenance_never_merges_away_a_pinned_item() {
    let into_body = "a fact nothing else references";
    let absorbed_body = "a pinned fact a rogue policy tries to merge away";
    let e = engine_with(MergesNamedPolicy {
        baseline: BaselinePolicy::default(),
        absorbed_body: absorbed_body.into(),
        into_body: into_body.into(),
        strategy: memorysafe_core::MergeStrategy::AppendAndUnion,
    });

    e.remember(RememberRequest::new(scope(), into_body))
        .await
        .unwrap();
    let absorbed_outcome = e
        .remember(RememberRequest::new(scope(), absorbed_body))
        .await
        .unwrap();
    let absorbed_id = absorbed_outcome.item_id.unwrap();
    e.protect(&scope(), &absorbed_id, memorysafe_core::Protection::Pinned)
        .await
        .unwrap();

    let report = e.maintain(&scope(), None).await.unwrap();
    assert_eq!(
        report.consolidated, 0,
        "a pinned item must never be merged away, exactly as it may never be evicted"
    );

    let left = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(
        left.len(),
        2,
        "both items must still exist; the pinned one was not absorbed"
    );
}

#[tokio::test]
async fn maintenance_ignores_a_self_referential_merge_decision() {
    let body = "a single item a buggy policy tries to merge into itself";
    let e = engine_with(MergesNamedPolicy {
        baseline: BaselinePolicy::default(),
        absorbed_body: body.into(),
        into_body: body.into(),
        strategy: memorysafe_core::MergeStrategy::AppendAndUnion,
    });
    // An anchor write first: a scope's very first item is scored maximally
    // fragile by `BaselinePolicy` and gets its own protection window (see
    // `protection_field.rs`), which would make `is_evictable` false for a
    // reason unrelated to the self-merge guard this test targets and would
    // confound it — the merge would be skipped either way, for the wrong
    // reason.
    e.remember(RememberRequest::new(
        scope(),
        "an unrelated anchor memory establishing scope history",
    ))
    .await
    .unwrap();
    let outcome = e
        .remember(RememberRequest::new(scope(), body))
        .await
        .unwrap();
    let id = outcome.item_id.unwrap();

    let report = e.maintain(&scope(), None).await.unwrap();
    assert_eq!(
        report.consolidated, 0,
        "a self-referential merge must not be applied"
    );

    let left = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(
        left.len(),
        2,
        "both the anchor and the target item must survive a self-merge attempt"
    );
    let target = left
        .iter()
        .find(|i| i.id == id)
        .expect("the item must survive a self-merge attempt, not vanish");
    assert_eq!(
        target.body, body,
        "a rejected self-merge must not alter the item's content"
    );
}

/// Forces `Action::Merge` into a freshly-minted id that was never written
/// anywhere — the shape a rogue or buggy `maintain` policy produces, mirroring
/// `merge_target_check.rs`'s `MergeIntoNothingPolicy` for the `remember` path.
struct MergesIntoNothingMaintainPolicy {
    baseline: BaselinePolicy,
    absorbed_body: String,
}

impl memorysafe_core::GovernancePolicy for MergesIntoNothingMaintainPolicy {
    fn id(&self) -> memorysafe_core::PolicyId {
        memorysafe_core::PolicyId::new("test-forced-merge-into-nothing", "0.0.1")
    }

    fn assess(
        &self,
        cand: &memorysafe_core::Candidate,
        ctx: &memorysafe_core::AssessContext,
    ) -> Result<memorysafe_core::Assessment, memorysafe_core::PolicyError> {
        self.baseline.assess(cand, ctx)
    }

    fn admit(
        &self,
        assessed: &memorysafe_core::Assessed,
        ctx: &memorysafe_core::AdmitContext,
    ) -> Result<memorysafe_core::Decision, memorysafe_core::PolicyError> {
        self.baseline.admit(assessed, ctx)
    }

    fn compose(
        &self,
        _req: &memorysafe_core::RecallRequest,
        _candidates: &[memorysafe_core::ScoredCandidate],
        _ctx: &memorysafe_core::ComposeContext,
    ) -> Result<memorysafe_core::WorkingSet, memorysafe_core::PolicyError> {
        unimplemented!("maintain tests never call compose")
    }

    fn maintain(
        &self,
        ctx: &memorysafe_core::MaintainContext,
    ) -> Result<Vec<memorysafe_core::Decision>, memorysafe_core::PolicyError> {
        let absorbed = ctx.batch.iter().find(|c| c.item.body == self.absorbed_body);
        Ok(match absorbed {
            Some(a) => vec![memorysafe_core::Decision {
                subject: Some(a.item.id.clone()),
                action: memorysafe_core::Action::Merge {
                    into: memorysafe_core::ItemId::new(),
                    strategy: memorysafe_core::MergeStrategy::AppendAndUnion,
                },
                evictions: vec![],
                reasons: vec![memorysafe_core::Reason::new(
                    memorysafe_core::ReasonCode::HighRedundancy,
                    "bogus merge target",
                    memorysafe_core::features! {},
                )],
                policy: self.id(),
            }],
            None => vec![],
        })
    }
}

#[tokio::test]
async fn maintenance_ignores_a_merge_into_a_nonexistent_target() {
    let body = "an item a rogue policy tries to merge into nothing";
    let e = engine_with(MergesIntoNothingMaintainPolicy {
        baseline: BaselinePolicy::default(),
        absorbed_body: body.into(),
    });
    // An anchor write first, for the same reason
    // `maintenance_ignores_a_self_referential_merge_decision` needs one: a
    // scope's very first item is scored maximally fragile by
    // `BaselinePolicy` and gets its own protection window, which would make
    // `is_evictable` false for a reason unrelated to the missing-target
    // guard this test targets.
    e.remember(RememberRequest::new(
        scope(),
        "an unrelated anchor memory establishing scope history",
    ))
    .await
    .unwrap();
    let outcome = e
        .remember(RememberRequest::new(scope(), body))
        .await
        .unwrap();
    let id = outcome.item_id.unwrap();

    let report = e.maintain(&scope(), None).await.unwrap();
    assert_eq!(
        report.consolidated, 0,
        "a merge into a nonexistent target must not be applied"
    );

    let left = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(
        left.len(),
        2,
        "both the anchor and the absorbed item must survive when the merge target does not exist"
    );
    assert!(
        left.iter().any(|i| i.id == id),
        "the absorbed item must survive when its merge target does not exist"
    );
}

/// Ignores its input text: every call returns the identical unit vector
/// regardless of what text is embedded. Counts calls, so a mutation that
/// skips re-embedding entirely (rather than merely discarding the result) is
/// caught directly — the same technique `protect_embedder_reach.rs` uses for
/// `Engine::protect`'s own re-embed step, applied here to the merge arm's.
struct SpyEmbedder {
    dim: u16,
    id: memorysafe_core::EmbedderId,
    calls: AtomicUsize,
}

impl SpyEmbedder {
    fn new(dim: u16) -> Self {
        Self {
            dim,
            id: memorysafe_core::EmbedderId::new("spy-embedder"),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl memorysafe_embed::Embedder for SpyEmbedder {
    fn id(&self) -> memorysafe_core::EmbedderId {
        self.id.clone()
    }

    fn dim(&self) -> u16 {
        self.dim
    }

    fn embed(
        &self,
        _text: &str,
    ) -> Result<memorysafe_core::Embedding, memorysafe_embed::EmbedError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut v = vec![0.0f32; self.dim as usize];
        v[0] = 1.0;
        Ok(memorysafe_core::Embedding::new(v, self.id.clone()))
    }
}

/// `apply_merge` recomputes `into`'s vector from the merged content rather
/// than leaving its pre-merge vector row stale (see its own doc comment).
/// Asserting the stored body already pins that the *content* is folded
/// correctly; this pins that the *embedder is actually invoked again* for
/// it — a mutation that hardcodes the merge's vector to `None` (leaving
/// whatever vector `into` had before the merge untouched) passes every other
/// test here, since none of them inspects vector search reachability.
#[tokio::test]
async fn maintenance_merge_reembeds_the_targets_content() {
    let into_body = "a completely unrelated fact about river deltas";
    let absorbed_body = "the cat sat on the mat";
    // `SpyEmbedder` returns the identical vector for any text, which would
    // make every candidate look like a perfect near-duplicate of every other
    // at ADMISSION time — collapsing `into_body` and `absorbed_body` into one
    // item before maintenance ever runs, defeating the premise. So the two
    // items are seeded through a separate engine using a real,
    // content-sensitive embedder, and only maintenance itself runs under the
    // spy — both engines share the same on-disk tenant files.
    let dir = tempfile::tempdir().expect("tempdir").keep();
    let seed = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.clone())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ));
    seed.remember(RememberRequest::new(scope(), into_body))
        .await
        .unwrap();
    seed.remember(RememberRequest::new(scope(), absorbed_body))
        .await
        .unwrap();
    assert_eq!(
        seed.review(&scope(), &Default::default())
            .await
            .unwrap()
            .len(),
        2,
        "the premise needs two distinct stored items to merge"
    );

    let spy = Arc::new(SpyEmbedder::new(8));
    let e = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir)),
        spy.clone(),
        Arc::new(MergesNamedPolicy {
            baseline: BaselinePolicy::default(),
            absorbed_body: absorbed_body.into(),
            into_body: into_body.into(),
            strategy: memorysafe_core::MergeStrategy::AppendAndUnion,
        }),
    ));

    let calls_before_maintain = spy.calls();
    let report = e.maintain(&scope(), None).await.unwrap();
    assert_eq!(report.consolidated, 1);
    assert!(
        spy.calls() > calls_before_maintain,
        "the merge must re-embed the merged content rather than leaving the \
         target's vector stale"
    );
}
