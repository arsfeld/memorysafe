//! `MergeWrite` gained a `pending_embedding` field so a merge whose re-embed
//! fails can be found by a future backfill job, and so the flag and the
//! `vectors` row it corresponds to are kept as one fact — see
//! `memorysafe_backend::write::MergeWrite::pending_embedding`'s own doc, and
//! `memorysafe-backend-sqlite`'s `lib.rs` `apply` merge arm, which deletes the
//! target's vector row exactly when the merge carries no fresh vector.
//! Nothing before this file exercised either half of that rule through
//! `remember`'s own merge branch (`write.rs`'s `Action::Merge` arm):
//! `merge_write_content.rs` only checks the merged body, and every other
//! merge test in this crate hands the target a real, successful embedding.
//!
//! `remember`'s merge branch is not reachable through `BaselinePolicy`'s own
//! near-duplicate detection when the *incoming* write's own embedding fails:
//! `gather::assess_context` only searches for neighbours when it has an
//! embedding to search with, so a failed embed sees zero neighbours and
//! `BaselinePolicy` never proposes a merge on its own in that case.
//! `MergesIntoLaterTargetPolicy` below forces the decision directly instead —
//! the same "always decides" shape `merge_target_check.rs`'s
//! `MergeIntoNothingPolicy` uses for its own rogue-merge tests — so both
//! directions of the flag/vector biconditional can be exercised regardless of
//! what a real policy would decide on its own.
//!
//! Reuses the `SpyEmbedder` technique from `protect_embedder_reach.rs` and
//! `read_embedder_reach.rs`: content-blind (the identical vector for any
//! text), so a vector search with a query sharing no keywords with a stored
//! body can only succeed through a real vector row — `DeterministicEmbedder`
//! is "monotone in token overlap" and cannot separate the two arms.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    Action, AdmitContext, AssessContext, Assessed, Assessment, Candidate, ComposeContext, Decision,
    EmbedderId, Embedding, ItemId, MaintainContext, MergeStrategy, PolicyError, PolicyId, Reason,
    ReasonCode, RecallBudget, RecallMode, RecallRequest, Scope, ScoredCandidate, SensitivityLevel,
    WorkingSet, features,
};
use memorysafe_embed::{EmbedError, Embedder};
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Ignores its input text: every successful call returns the identical unit
/// vector regardless of what text is embedded, and every call while
/// `set_failing(true)` returns the same error regardless of what text is
/// embedded — the same double `protect_embedder_reach.rs` and
/// `read_embedder_reach.rs` use, for the same reason.
struct SpyEmbedder {
    dim: u16,
    id: EmbedderId,
    fail: AtomicBool,
    calls: AtomicUsize,
}

impl SpyEmbedder {
    fn new(dim: u16) -> Self {
        Self {
            dim,
            id: EmbedderId::new("spy-embedder"),
            fail: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        }
    }

    fn set_failing(&self, fail: bool) {
        self.fail.store(fail, Ordering::SeqCst);
    }
}

impl Embedder for SpyEmbedder {
    fn id(&self) -> EmbedderId {
        self.id.clone()
    }

    fn dim(&self) -> u16 {
        self.dim
    }

    fn embed(&self, _text: &str) -> Result<Embedding, EmbedError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(EmbedError::Unavailable(
                "forced failure for a test double".into(),
            ));
        }
        let mut v = vec![0.0f32; self.dim as usize];
        v[0] = 1.0;
        Ok(Embedding::new(v, self.id.clone()))
    }
}

/// Delegates `assess` and (until `set_target` is called) `admit` to
/// `BaselinePolicy` unchanged, so the seeding `remember` call that creates
/// the merge target behaves normally. Once a target is set, every subsequent
/// `admit` unconditionally decides `Action::Merge` into it, regardless of the
/// candidate's own embedding or neighbours — a real, previously-stored
/// target rather than `MergeIntoNothingPolicy`'s bogus one.
struct MergesIntoLaterTargetPolicy {
    baseline: BaselinePolicy,
    target: Mutex<Option<ItemId>>,
}

impl MergesIntoLaterTargetPolicy {
    fn new() -> Self {
        Self {
            baseline: BaselinePolicy::default(),
            target: Mutex::new(None),
        }
    }

    fn set_target(&self, id: ItemId) {
        *self.target.lock().unwrap() = Some(id);
    }
}

impl memorysafe_core::GovernancePolicy for MergesIntoLaterTargetPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::new("test-merges-into-later-target", "0.0.1")
    }

    fn assess(&self, cand: &Candidate, ctx: &AssessContext) -> Result<Assessment, PolicyError> {
        self.baseline.assess(cand, ctx)
    }

    fn admit(&self, assessed: &Assessed, ctx: &AdmitContext) -> Result<Decision, PolicyError> {
        let target = self.target.lock().unwrap().clone();
        match target {
            None => self.baseline.admit(assessed, ctx),
            Some(into) => Ok(Decision {
                subject: None,
                action: Action::Merge {
                    into,
                    strategy: MergeStrategy::AppendAndUnion,
                },
                evictions: vec![],
                reasons: vec![Reason::new(
                    ReasonCode::HighRedundancy,
                    "test-forced merge into a later target",
                    features! {},
                )],
                policy: self.id(),
            }),
        }
    }

    fn compose(
        &self,
        _req: &RecallRequest,
        _candidates: &[ScoredCandidate],
        _ctx: &ComposeContext,
    ) -> Result<WorkingSet, PolicyError> {
        unimplemented!("remember() never calls compose")
    }

    fn maintain(&self, _ctx: &MaintainContext) -> Result<Vec<Decision>, PolicyError> {
        unimplemented!("remember() never calls maintain")
    }
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "coding-agent").unwrap()
}

fn recall_req(query: &str) -> RecallRequest {
    RecallRequest {
        scope: scope(),
        query: Some(query.into()),
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
    }
}

fn engine_with(policy: Arc<MergesIntoLaterTargetPolicy>, spy: Arc<SpyEmbedder>) -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        spy,
        policy,
    ))
}

/// One direction of the biconditional: a merge whose re-embed fails must
/// leave the target marked `pending_embedding` AND with no row in the
/// `vectors` table — not just the flag on its own, which the invariant
/// `read_embedder_reach.rs` documents ("a pending-embedding item has no row
/// in the `vectors` table at all") depends on holding for every pending
/// item, merged ones included.
#[tokio::test]
async fn a_failed_reembed_during_a_merge_marks_the_target_pending_with_no_vector_row() {
    let spy = Arc::new(SpyEmbedder::new(8));
    let policy = Arc::new(MergesIntoLaterTargetPolicy::new());
    let engine = engine_with(policy.clone(), spy.clone());

    let target_id = engine
        .remember(RememberRequest::new(scope(), "the original target content"))
        .await
        .unwrap()
        .item_id
        .unwrap();

    let stored = engine.review(&scope(), &Default::default()).await.unwrap();
    assert!(
        !stored
            .iter()
            .find(|i| i.id == target_id)
            .unwrap()
            .pending_embedding,
        "premise: the seeding remember succeeded and is not pending"
    );

    policy.set_target(target_id.clone());
    spy.set_failing(true);

    let outcome = engine
        .remember(RememberRequest::new(
            scope(),
            "mergedmarkerXYZ replaces the target body entirely",
        ))
        .await
        .unwrap();
    match &outcome.action {
        Action::Merge { into, .. } => assert_eq!(*into, target_id),
        other => panic!("expected a merge into the seeded target, got {other:?}"),
    }

    let stored = engine.review(&scope(), &Default::default()).await.unwrap();
    let merged = stored.iter().find(|i| i.id == target_id).unwrap();
    assert!(
        merged.pending_embedding,
        "a merge whose re-embed failed must leave the target marked pending_embedding"
    );

    let by_keyword = engine.recall(recall_req("mergedmarkerXYZ")).await.unwrap();
    assert!(
        !by_keyword.items.is_empty(),
        "a pending-embedding merge target must still be reachable through keyword search"
    );

    // Prove no vector row survives: with a WORKING embedder, a query sharing
    // no keywords with the stored body can only be found through a vector
    // row, and `SpyEmbedder` returns the same vector for every query it is
    // ever handed — so a surviving pre-merge vector row would match this
    // query exactly as well as a fresh one would.
    spy.set_failing(false);
    let by_vector = engine
        .recall(recall_req("totallyunrelatedqueryABC"))
        .await
        .unwrap();
    assert!(
        by_vector.items.is_empty(),
        "a merge whose re-embed failed must leave no row in the vectors \
         table — found via vector search on a query sharing no keywords with \
         the stored body, so a surviving stale vector row is the only \
         explanation"
    );
}

/// The converse: a merge whose re-embed succeeds must clear a stale
/// `pending_embedding: true` left over from the target's own troubled
/// history, and leave it with a fresh vector row. The task this file was
/// written for calls this case "falls out of the mirrored rule for free" —
/// which is exactly why it needs its own proof rather than an assumption.
#[tokio::test]
async fn a_successful_reembed_during_a_merge_clears_a_stale_pending_flag() {
    let spy = Arc::new(SpyEmbedder::new(8));
    let policy = Arc::new(MergesIntoLaterTargetPolicy::new());
    let engine = engine_with(policy.clone(), spy.clone());

    spy.set_failing(true);
    let target_id = engine
        .remember(RememberRequest::new(
            scope(),
            "the original target content, doomed to fail its own embed",
        ))
        .await
        .unwrap()
        .item_id
        .unwrap();

    let stored = engine.review(&scope(), &Default::default()).await.unwrap();
    assert!(
        stored
            .iter()
            .find(|i| i.id == target_id)
            .unwrap()
            .pending_embedding,
        "premise: the seeding remember failed to embed and is pending"
    );

    policy.set_target(target_id.clone());
    spy.set_failing(false);

    let outcome = engine
        .remember(RememberRequest::new(
            scope(),
            "mergedmarkerDEF replaces the target body entirely",
        ))
        .await
        .unwrap();
    match &outcome.action {
        Action::Merge { into, .. } => assert_eq!(*into, target_id),
        other => panic!("expected a merge into the seeded target, got {other:?}"),
    }

    let stored = engine.review(&scope(), &Default::default()).await.unwrap();
    let merged = stored.iter().find(|i| i.id == target_id).unwrap();
    assert!(
        !merged.pending_embedding,
        "a merge whose re-embed succeeded must clear a stale pending_embedding flag"
    );

    let by_vector = engine
        .recall(recall_req("totallydisjointqueryGHI"))
        .await
        .unwrap();
    assert!(
        !by_vector.items.is_empty(),
        "a merge whose re-embed succeeded must leave a fresh vector row behind"
    );
}
