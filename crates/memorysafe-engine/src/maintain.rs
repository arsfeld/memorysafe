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
                // B, and `items::delete` being scope-filtered only saves the
                // ROW — `vectors::delete` carries no scope predicate at all
                // and would silently strip scope A's vector regardless. The
                // engine enforces pinning, and now membership, even if a
                // policy forgets.
                let Some(candidate) = ctx.batch.iter().find(|c| c.item.id == e.item) else {
                    continue;
                };
                if candidate.item.protection != Protection::Pinned {
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
            // pre-existence check applies for the identical reason
            // (`vectors::delete` has no scope predicate of its own).
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
            let audit = AuditRecord::new(
                scope.clone(),
                AuditEvent::MaintenanceRun,
                vec![],
                Actor::system(),
                OffsetDateTime::now_utc(),
            );
            let mut txn = WriteTransaction::new(scope.clone(), audit);
            txn.evictions = to_forget;
            self.backend.apply(txn).await?;
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
        // leave `into`'s existing vector row alone rather than fail the merge.
        let vector = self
            .embedder
            .embed(&merged_body)
            .ok()
            .map(|e| QuantizedVector::from_embedding(&e));
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
        });

        self.backend.apply(txn).await?;
        Ok(true)
    }
}
