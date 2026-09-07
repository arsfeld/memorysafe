use crate::Engine;
use crate::error::EngineError;
use crate::validate;
use memorysafe_backend::{MergeWrite, Page, WriteTransaction};
use memorysafe_core::{
    Action, Actor, AuditEvent, AuditRecord, ItemId, ItemRef, MaintainContext, MaintenanceCandidate,
    MemoryItem, MergeStrategy, Protection,
};
use memorysafe_embed::QuantizedVector;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Items examined per call. Maintenance is explicit and resumable rather than
/// a background thread, so the caller controls how much work happens at once.
pub const MAINTAIN_BATCH: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintainCursor {
    pub offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintainReport {
    pub scanned: usize,
    pub forgotten: usize,
    pub protection_released: usize,
    pub consolidated: usize,
    pub next_cursor: Option<MaintainCursor>,
}

impl Engine {
    pub async fn maintain(
        &self,
        scope: &memorysafe_core::Scope,
        cursor: Option<MaintainCursor>,
    ) -> Result<MaintainReport, EngineError> {
        let offset = cursor.map(|c| c.offset).unwrap_or(0);
        let batch = self
            .backend
            .list(
                scope,
                &Page {
                    offset,
                    limit: MAINTAIN_BATCH,
                },
            )
            .await?;

        if batch.is_empty() {
            return Ok(MaintainReport {
                scanned: 0,
                forgotten: 0,
                protection_released: 0,
                consolidated: 0,
                next_cursor: None,
            });
        }

        let scanned = batch.len();
        let capacity = self.backend.capacity_state(scope).await?;
        // A short page means there is nothing after it. This is the same fact
        // `next_cursor` is derived from below, so the two cannot disagree.
        let is_final_batch = scanned < MAINTAIN_BATCH;
        let scored: Vec<MaintenanceCandidate> = batch
            .into_iter()
            .map(|item| MaintenanceCandidate {
                value: memorysafe_core::Score::clamped(0.5),
                fragility: memorysafe_core::Score::clamped(0.5),
                item,
                // OPEN: same gap as the `admit` path above — `Backend::list`
                // returns bare `MemoryItem`s and the access statistics are not
                // on that type. `(None, 0)` reads as "never recalled", which
                // is the wrong answer for a hot item, and maintenance is
                // precisely where staleness is supposed to matter.
                last_accessed_at: None,
                access_count: 0,
            })
            .collect();
        let ctx = MaintainContext {
            scope: scope.clone(),
            batch: scored,
            is_final_batch,
            capacity,
            stats: self.backend.scope_stats(scope).await?,
            now: OffsetDateTime::now_utc(),
        };

        let policy = self.policy.clone();
        let x = ctx.clone();
        // `Arc<dyn GovernancePolicy>` is not `RefUnwindSafe` (see `run_assess`
        // and `run_admit` in `write.rs` for the same escape hatch): this call
        // never hands the policy a `&mut` to anything shared, so a panic here
        // leaves no half-mutated state for the caller to observe.
        let call = std::panic::AssertUnwindSafe(move || policy.maintain(&x));
        let decisions = match validate::call_policy(call) {
            Ok(d) => d,
            Err(failure) => match self.stance {
                validate::FailureStance::FailClosed => {
                    return Err(EngineError::PolicyRefused(failure.to_string()));
                }
                validate::FailureStance::FailSafe => self
                    .fallback_policy
                    .maintain(&ctx)
                    .map_err(|e| EngineError::PolicyRefused(e.to_string()))?,
            },
        };

        let mut to_forget: Vec<ItemId> = Vec::new();
        let mut forgotten_refs: Vec<ItemRef> = Vec::new();
        let mut to_release: Vec<(ItemId, Protection)> = Vec::new();
        let mut released = 0usize;
        let mut consolidated = 0usize;

        for d in &decisions {
            for e in &d.evictions {
                // Looked up in THIS batch, not trusted as a bare id — the same
                // discipline the merge arm below applies, and for the same
                // reason. One `Arc<dyn GovernancePolicy>` serves every scope
                // this engine handles; without this check a policy that
                // retains an id it saw while deciding for scope A can hand
                // that same id back as an "eviction" while deciding for scope
                // B. This is defence in depth against a `Backend` whose
                // eviction does not cascade a removed item's other rows —
                // its vector row, most concretely — under the same scope
                // predicate as the row itself: a backend failing that
                // property would delete the item row correctly (scoped) but
                // strip scope A's vector regardless (unscoped), silently.
                // The engine enforces pinning, and now membership, even if a
                // policy forgets.
                let Some(candidate) = ctx.batch.iter().find(|c| c.item.id == e.item) else {
                    continue;
                };
                if candidate.item.protection != Protection::Pinned {
                    // The ref is built here, from the candidate already in
                    // hand, and not after the loop: `to_forget` is moved into
                    // `txn.evictions` on the way to the backend, and a
                    // `MaintenanceRun` record that named nothing was the
                    // result of trying to derive the refs from it afterwards.
                    forgotten_refs.push(ItemRef::from_item(&candidate.item));
                    to_forget.push(e.item.clone());
                }
            }
            // Actually apply the release. Counting it and moving on is how a
            // protection window silently never expires.
            if let (Some(id), Action::Retain { protection }) = (&d.subject, &d.action)
                && d.evictions.is_empty()
            {
                to_release.push((id.clone(), *protection));
                released += 1;
            }
            // `Action::Merge` is not `admit`'s merge applier with different
            // arguments: the absorbed side here is a real, stored item with
            // its own id, audit history and embedding, not an admission
            // candidate with none of the three. `d.subject` names it —
            // `Decision::subject`'s own doc says `maintain` MUST set it — and
            // it is looked up in THIS batch, not trusted as a bare id: a
            // subject `maintain` never offered is skipped rather than
            // forgotten sight-unseen, the same discipline `forget`'s own
            // pre-existence check applies for the identical reason —
            // defence in depth against a `Backend` whose eviction cascade is
            // not itself scope-filtered.
            if let (Some(subject), Action::Merge { into, strategy }) = (&d.subject, &d.action) {
                let Some(absorbed) = ctx.batch.iter().find(|c| &c.item.id == subject) else {
                    continue;
                };
                // The same pinning defence the eviction loop above applies:
                // merging an item away deletes it exactly as an eviction
                // does, so a policy may not use `Merge` as a side door around
                // a pin or an unexpired protection window.
                if !absorbed.item.protection.is_evictable(ctx.now) {
                    continue;
                }
                if self
                    .apply_merge(scope, &absorbed.item, into, *strategy)
                    .await?
                {
                    consolidated += 1;
                }
            }
        }

        let forgotten = to_forget.len();

        for (id, protection) in to_release {
            // Reuse the audited protect path rather than hand-rolling a write.
            self.protect(scope, &id, protection).await?;
        }

        if !to_forget.is_empty() || released > 0 {
            // Names the items this run removed, like every other mutating
            // path in this engine. The refs cover `to_forget` only: the
            // protection releases counted in `released` each write their own
            // audit record through `self.protect` above, which already names
            // its own item, so repeating them here would double-count them in
            // the trail.
            let audit = AuditRecord::new(
                scope.clone(),
                AuditEvent::MaintenanceRun,
                std::mem::take(&mut forgotten_refs),
                Actor::system(),
                OffsetDateTime::now_utc(),
            );
            let mut txn = WriteTransaction::new(scope.clone(), audit);
            txn.evictions = to_forget;
            self.backend.apply(txn).await?;
            // Any write invalidates its scope (see `write.rs`'s own call for
            // the full rationale). `to_release`'s own writes already
            // invalidate individually through `self.protect` above; this
            // covers `to_forget`, which does not go through `protect` at all.
            self.cache.invalidate_scope(scope).await;
        }

        // Advance past what survived; forgotten rows AND consolidated
        // (merged-away) rows have both shifted the window. `Backend::list` is
        // a total order over the whole scope, so every row deleted from
        // inside the scanned window — whether by eviction or by a merge's own
        // `txn.evictions` on the absorbed item — moves everything after it
        // one position earlier. Accounting for `forgotten` alone left
        // `consolidated` rows uncounted, overshooting the next offset by
        // exactly that many and silently skipping that many items forever.
        let next_offset = offset + scanned.saturating_sub(forgotten + consolidated);
        let next_cursor = if is_final_batch {
            None
        } else {
            Some(MaintainCursor {
                offset: next_offset,
            })
        };

        Ok(MaintainReport {
            scanned,
            forgotten,
            protection_released: released,
            consolidated,
            next_cursor,
        })
    }

    /// Applies one `Action::Merge` decision from `maintain`: writes the
    /// merged content to `into`, deletes `absorbed`, and writes one `Merged`
    /// audit record naming both — all in a single backend transaction
    /// (`WriteTransaction` carries both `evictions` and `merge`, and `apply`
    /// commits them together with the audit row or not at all).
    ///
    /// **Not `remember`'s merge applier with different arguments.** There the
    /// incoming side is an admission `Candidate` with no id, no audit history
    /// and no embedding. Here `absorbed` is a real, stored `MemoryItem` with
    /// all three:
    /// - its id is evicted (`txn.evictions`), which also deletes its vector
    ///   row — it must not keep surfacing in neighbour search under an id
    ///   that no longer resolves to a live item;
    /// - its earlier audit rows are left exactly as they are. A scope is
    ///   never rewritten to make a row fit, and the one `Merged` record this
    ///   method writes is what makes the item's disappearance explicable to
    ///   anyone reading the trail later;
    /// - `into`'s own vector is refreshed from the merged content when the
    ///   embedder is available, and left as-is (stale, not wrong) when it is
    ///   not — the same "a missing model must never cost a memory" rule
    ///   `remember` and `protect` already follow.
    ///
    /// The audit's two `ItemRef`s are built from each item's state as fetched
    /// — before the merge SQL runs, not predicted from it — so this never
    /// duplicates (and cannot drift from) `items::merge`'s own tag/attr union
    /// logic.
    ///
    /// Returns `false`, having written nothing, when `into` does not exist or
    /// is outside `scope` (`Backend::get` is itself scope-filtered, so one
    /// lookup answers both) or when `into == absorbed.id`: a policy naming a
    /// bad merge target is a policy bug, not a crash or a silently dropped
    /// write.
    async fn apply_merge(
        &self,
        scope: &memorysafe_core::Scope,
        absorbed: &MemoryItem,
        into: &ItemId,
        strategy: MergeStrategy,
    ) -> Result<bool, EngineError> {
        if *into == absorbed.id {
            return Ok(false);
        }
        let Some(target) = self.backend.get(scope, into).await? else {
            return Ok(false);
        };

        let merged_body = match strategy {
            MergeStrategy::AppendAndUnion => format!("{}\n\n{}", target.body, absorbed.body),
            MergeStrategy::ReplaceBody => absorbed.body.clone(),
        };
        // A missing or failed embedder must never cost a user their memory:
        // the merge itself still proceeds and the content is still stored.
        // What it costs is reach, not memory — `into`'s existing vector row
        // is dropped (see the conditional deletion in
        // `memorysafe-backend-sqlite`'s `lib.rs` `apply`, keyed on exactly
        // this `vector` being `None`) rather than left stale under the
        // *pre-merge* body, and the item is marked `pending_embedding` below
        // so a future backfill can repair it. Losing vector-search
        // reachability until then is the deliberate trade; losing the
        // memory, or leaving it silently unfindable by either mechanism
        // forever, is not.
        let vector = self
            .embedder
            .embed(&merged_body)
            .ok()
            .map(|e| QuantizedVector::from_embedding(&e));
        // Mirrors `remember`'s own rule (`write.rs`: `pending_embedding =
        // embedding.is_none()`), derived from this call's `vector` rather
        // than re-deriving it: `MergeWrite::pending_embedding` and
        // `MergeWrite::vector` must agree, by construction, everywhere a
        // `MergeWrite` is built — see that field's own doc for why.
        let pending_embedding = vector.is_none();
        let byte_size = merged_body.len() as u64;

        let refs = vec![ItemRef::from_item(absorbed), ItemRef::from_item(&target)];
        let audit = AuditRecord::new(
            scope.clone(),
            AuditEvent::Merged,
            refs,
            Actor::system(),
            OffsetDateTime::now_utc(),
        );
        let mut txn = WriteTransaction::new(scope.clone(), audit);
        txn.evictions = vec![absorbed.id.clone()];
        txn.merge = Some(MergeWrite {
            target: into.clone(),
            body: merged_body,
            tags: absorbed.tags.clone(),
            attrs: absorbed.attrs.clone(),
            vector,
            byte_size,
            pending_embedding,
        });

        self.backend.apply(txn).await?;
        // Same rationale as the decision-application write above: a merge
        // changes `item_count`/`total_bytes` for `scope` (one item absorbed
        // away) exactly as an eviction does.
        self.cache.invalidate_scope(scope).await;
        Ok(true)
    }
}

// Task 37's cache-invalidation ruling: `maintain`'s own decision-application
// write (the `backend.apply` a few lines above `next_offset`) and
// `apply_merge`'s write both change item counts and bytes for `scope`, so
// each must invalidate it exactly as `remember`'s write does. Unit tests, not
// `tests/maintain.rs` integration tests, because they need `Engine`'s private
// `cache` field (`pub(crate)`, visible anywhere in this crate) to observe
// invalidation directly, and `apply_merge` is itself private to this module.
#[cfg(test)]
mod cache_invalidation_tests {
    use super::*;
    use crate::EngineConfig;
    use crate::write::RememberRequest;
    use memorysafe_backend_sqlite::SqliteBackend;
    use memorysafe_core::{Scope, ScopeStats};
    use memorysafe_embed::DeterministicEmbedder;
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
        Scope::new("acme", "user-42", "agent").unwrap()
    }

    fn sentinel() -> ScopeStats {
        ScopeStats {
            item_count: 999_999,
            ..Default::default()
        }
    }

    /// A minimal policy whose `maintain` forces exactly one eviction and
    /// nothing else — no `Retain` decision, ever, so `to_release` stays empty
    /// and `released` stays `0`. This matters: an earlier version of this
    /// test used `BaselinePolicy::default()` throughout and let it choose
    /// what to do with a second, "healthy" item, which turned out to itself
    /// release a protection window on that item (`released == 1`). Since
    /// `protect` invalidates on its own, that masked the very call this test
    /// exists to check — deleting `maintain`'s own forgotten-items
    /// invalidation call still passed every test, because `self.protect`'s
    /// call (triggered by the *other* item) invalidated the same scope first.
    /// Delegating only `assess`/`admit` to `BaselinePolicy` (needed so the
    /// seeding `remember` call is admitted normally) and fully owning
    /// `maintain` closes that gap by construction.
    struct ForcesOneForgetOnlyPolicy {
        baseline: BaselinePolicy,
        target_body: String,
    }

    impl memorysafe_core::GovernancePolicy for ForcesOneForgetOnlyPolicy {
        fn id(&self) -> memorysafe_core::PolicyId {
            memorysafe_core::PolicyId::new("test-forces-one-forget-only", "0.0.1")
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
            unimplemented!("this test never calls compose")
        }

        fn maintain(
            &self,
            ctx: &memorysafe_core::MaintainContext,
        ) -> Result<Vec<memorysafe_core::Decision>, memorysafe_core::PolicyError> {
            let target = ctx.batch.iter().find(|c| c.item.body == self.target_body);
            Ok(match target {
                Some(t) => vec![memorysafe_core::Decision {
                    subject: None,
                    action: memorysafe_core::Action::Reject,
                    evictions: vec![memorysafe_core::Eviction {
                        item: t.item.id.clone(),
                        reason: memorysafe_core::Reason::new(
                            memorysafe_core::ReasonCode::CapacityPressure,
                            "forced forget for the cache-invalidation test",
                            memorysafe_core::features! {},
                        ),
                    }],
                    reasons: vec![],
                    policy: self.id(),
                }],
                None => vec![],
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
    async fn maintain_invalidates_the_scopes_cached_stats_when_it_forgets_something() {
        let target_body = "a lone item a forced-forget policy will remove";
        let e = engine_with(ForcesOneForgetOnlyPolicy {
            baseline: BaselinePolicy::default(),
            target_body: target_body.into(),
        });
        e.remember(RememberRequest::new(scope(), target_body))
            .await
            .unwrap();

        e.cache.put_stats(&scope(), sentinel()).await;
        assert!(
            e.cache.stats(&scope()).await.is_some(),
            "premise: cache seeded"
        );

        let report = e.maintain(&scope(), None).await.unwrap();
        assert_eq!(report.forgotten, 1, "premise: an actual forget happened");
        assert_eq!(
            report.protection_released, 0,
            "premise: isolate the forgotten-items write from protect's own invalidation"
        );
        assert!(
            e.cache.stats(&scope()).await.is_none(),
            "maintain must invalidate its scope's cached stats when it forgets something"
        );
    }

    /// Negative control: `maintain`'s forgotten-items write is guarded by
    /// `!to_forget.is_empty() || released > 0`, so a pass that does neither
    /// never calls `backend.apply` at all and must not invalidate anything —
    /// pins that the invalidation is tied to an actual write, not
    /// unconditionally run on every `maintain` call. `BaselinePolicy` here
    /// (not the forcing policy above) genuinely proposes no eviction and no
    /// release for one healthy item, which the two explicit `assert_eq!`s
    /// confirm rather than assume.
    #[tokio::test]
    async fn maintain_over_a_healthy_scope_leaves_the_cache_alone() {
        let e = engine();
        e.remember(RememberRequest::new(scope(), "a perfectly healthy memory"))
            .await
            .unwrap();

        e.cache.put_stats(&scope(), sentinel()).await;
        let report = e.maintain(&scope(), None).await.unwrap();
        assert_eq!(report.forgotten, 0);
        assert_eq!(report.protection_released, 0);
        assert!(
            e.cache.stats(&scope()).await.is_some(),
            "a no-op maintenance pass must not invalidate a scope's cache"
        );
    }

    #[tokio::test]
    async fn apply_merge_invalidates_the_scopes_cached_stats() {
        let e = engine();
        let into_id = e
            .remember(RememberRequest::new(scope(), "target of a merge"))
            .await
            .unwrap()
            .item_id
            .unwrap();
        let absorbed_id = e
            .remember(RememberRequest::new(scope(), "an unrelated absorbed item"))
            .await
            .unwrap()
            .item_id
            .unwrap();
        let absorbed_item = e
            .backend
            .get(&scope(), &absorbed_id)
            .await
            .unwrap()
            .expect("the absorbed item must exist");

        e.cache.put_stats(&scope(), sentinel()).await;
        let applied = e
            .apply_merge(
                &scope(),
                &absorbed_item,
                &into_id,
                MergeStrategy::AppendAndUnion,
            )
            .await
            .unwrap();
        assert!(applied, "premise: an actual merge was applied");
        assert!(
            e.cache.stats(&scope()).await.is_none(),
            "apply_merge must invalidate its scope's cached stats"
        );
    }
}
