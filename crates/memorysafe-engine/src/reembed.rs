//! Re-embedding. Two jobs over one implementation: the backfill that repairs
//! items admitted while the embedder was unavailable, and the explicit
//! migration that rewrites every vector in a scope after a change of
//! embedding model.
//!
//! Both are cursor-driven and caller-paced for the same reason `maintain` is:
//! nothing about a corpus changes unobserved, and the caller decides how much
//! work happens at once.

use crate::Engine;
use crate::error::EngineError;
use memorysafe_backend::{ItemWrite, Page, WriteTransaction};
use memorysafe_core::{Actor, ActorKind, AuditEvent, AuditRecord, ItemRef, MemoryItem, Scope};
use memorysafe_embed::QuantizedVector;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Items per call. Like maintenance, this is an explicit resumable job rather
/// than a background thread, so the caller controls the work done at once.
pub const REEMBED_BATCH: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReembedCursor {
    pub offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReembedReport {
    /// Items read from the scope in this pass, before the pending-only
    /// filter. `scanned` describes the window the cursor advanced over, not
    /// the work done inside it — a pass that scans 200 and embeds none has
    /// still consumed a page.
    pub scanned: usize,
    pub embedded: usize,
    /// Items that are **still `pending_embedding` after this pass**.
    ///
    /// Counted only for targets whose `pending_embedding` was already `true`
    /// and whose re-embed then failed. A `reembed_scope` target that was not
    /// pending and fails to embed keeps its existing vector and its `false`
    /// column, so counting it here would make this report contradict
    /// `Engine::review` on the same scope — see `Engine::reembed`'s own doc
    /// for why that choice was made rather than redefining the name.
    pub still_pending: usize,
    pub next_cursor: Option<ReembedCursor>,
}

impl Engine {
    /// Embeds items admitted while the embedder was unavailable. Without this,
    /// a transient outage becomes permanent silent recall degradation.
    ///
    /// Every target here was already `pending_embedding`, so a failure leaves
    /// the flag exactly where it was and this pass is safely retryable —
    /// `ReembedReport::still_pending` counts what is left to retry.
    pub async fn backfill_embeddings(
        &self,
        scope: &Scope,
        cursor: Option<ReembedCursor>,
    ) -> Result<ReembedReport, EngineError> {
        self.reembed(scope, cursor, true).await
    }

    /// Re-embeds every item in the scope. This is the migration to run after
    /// changing embedding model — vectors from different models are not
    /// comparable, so it is explicit rather than something that happens by
    /// accident.
    ///
    /// Run it to completion. A half-migrated scope holds vectors from two
    /// models under one `vectors.embedder` column, and `vectors::search`
    /// compares them as if they shared a space.
    pub async fn reembed_scope(
        &self,
        scope: &Scope,
        cursor: Option<ReembedCursor>,
    ) -> Result<ReembedReport, EngineError> {
        self.reembed(scope, cursor, false).await
    }

    /// One page of re-embedding. `pending_only` is the whole difference
    /// between the two public jobs above.
    ///
    /// **The embedder is called directly, not through `embed_cached`.**
    /// `EngineCache`'s embedding cache is content-addressed — keyed on the
    /// text alone, with no model in the key — which is exactly what makes it
    /// free correctness on the write path and exactly what makes it wrong
    /// here. `reembed_scope` exists to rewrite vectors *because the model
    /// changed*; served from that cache it would rewrite each item with the
    /// vector the previous model produced and report a successful migration
    /// that changed nothing.
    ///
    /// **Why each item is evicted and re-upserted rather than updated.**
    /// `items::insert` in the SQLite backend is a plain `INSERT` with no
    /// `ON CONFLICT` clause, so re-inserting a live id would violate the
    /// primary key. `SqliteBackend::apply` processes `txn.evictions` before
    /// its upsert branch, and `vectors.item_id REFERENCES items(id) ON DELETE
    /// CASCADE` takes the old vector row with the item row — so the upsert's
    /// `vectors::insert` hits no conflict and writes `subject`/`namespace`
    /// fresh from the item's own scope. The eviction is therefore **required**
    /// rather than gratuitous, and it is also what makes re-embedding the one
    /// path that *heals* a divergent vector row (see
    /// `every_vector_rows_scope_columns_agree_with_the_item_it_references` in
    /// `memorysafe-backend-sqlite`). Both halves commit in one
    /// `WriteTransaction` with the audit row, so an item is never briefly
    /// absent from the scope.
    ///
    /// **Consequence worth stating before someone files it as a bug:**
    /// `AppliedWrite::evicted` will name every re-embedded item's id, because
    /// that field is defined as the ids actually removed and the row genuinely
    /// was removed. Nothing was forgotten; the same id is re-inserted in the
    /// same transaction.
    ///
    /// **Pagination survives the round trip** because `Backend::list` orders
    /// by `(created_at, id)` — both carried unchanged through the delete and
    /// re-insert — not by insertion order. So, unlike `maintain`, this job
    /// removes no rows from the scanned window on net and `next_cursor` is
    /// simply `offset + scanned`.
    ///
    /// **`still_pending` counts genuinely-pending items only.** The
    /// alternative was to keep incrementing it on every failed target and
    /// redefine the name in prose as "targets left without a fresh vector".
    /// That was rejected: a `reembed_scope` target that was not pending keeps
    /// its old vector and its `pending_embedding = false` column, so the
    /// looser counter would report N still-pending while `Engine::review`
    /// showed none — a number no caller could cross-check against the data it
    /// claims to describe. As written, `still_pending` is exactly the count of
    /// scanned targets that `review` will still show as `pending_embedding`,
    /// and is therefore falsifiable.
    async fn reembed(
        &self,
        scope: &Scope,
        cursor: Option<ReembedCursor>,
        pending_only: bool,
    ) -> Result<ReembedReport, EngineError> {
        let offset = cursor.map(|c| c.offset).unwrap_or(0);
        let batch: Vec<MemoryItem> = self
            .backend
            .list(
                scope,
                &Page {
                    offset,
                    limit: REEMBED_BATCH,
                },
            )
            .await?;

        if batch.is_empty() {
            return Ok(ReembedReport {
                scanned: 0,
                embedded: 0,
                still_pending: 0,
                next_cursor: None,
            });
        }

        let scanned = batch.len();
        let targets: Vec<MemoryItem> = batch
            .into_iter()
            .filter(|i| !pending_only || i.pending_embedding)
            .collect();

        let mut embedded = 0usize;
        let mut still_pending = 0usize;

        for item in targets {
            let Ok(embedding) = self.embedder.embed(&item.body) else {
                // Leave the flag set; a failed backfill must be retryable.
                if item.pending_embedding {
                    still_pending += 1;
                }
                continue;
            };
            let vector = QuantizedVector::from_embedding(&embedding);

            let mut updated = item.clone();
            updated.pending_embedding = false;

            // Replacing the row is what clears the flag and rewrites the
            // vector in one atomic step. One audit record per item, not one
            // per run: Invariant 4 requires one record per mutation, and
            // re-embedding one item is one mutation. `AuditEvent::Reembedded`
            // is what makes the run queryable as a unit; the record count is
            // what keeps it honest.
            let audit = AuditRecord::new(
                scope.clone(),
                AuditEvent::Reembedded,
                vec![ItemRef::from_item(&updated)],
                Actor {
                    kind: ActorKind::System,
                    id: None,
                },
                OffsetDateTime::now_utc(),
            );
            let mut txn = WriteTransaction::new(scope.clone(), audit);
            txn.evictions = vec![item.id.clone()];
            txn.upsert = Some(ItemWrite {
                item: updated.clone(),
                vector: Some(vector),
            });

            self.backend.apply(txn).await?;
            embedded += 1;
        }

        // Any write invalidates its scope, on the same terms as `remember`,
        // `forget`, `protect`, `purge_subject` and `maintain` — see
        // `EngineCache`'s own doc comment. Unconditional on a non-empty page,
        // exactly as `remember`'s is: at worst one needless refetch, never a
        // stale statistic. The empty-scope early return above is what keeps a
        // pass that read no rows from reaching it, and
        // `a_pass_over_an_empty_scope_leaves_the_cache_alone` is what makes
        // that early return load-bearing rather than redundant.
        self.cache.invalidate_scope(scope).await;

        let next_cursor = if scanned < REEMBED_BATCH {
            None
        } else {
            Some(ReembedCursor {
                offset: offset + scanned,
            })
        };

        Ok(ReembedReport {
            scanned,
            embedded,
            still_pending,
            next_cursor,
        })
    }
}

// `reembed`'s own `backend.apply` changes no item count and no byte total —
// each item is evicted and re-inserted at the same size — but it does write
// audit rows and it does go through `Backend::apply`, so it joins the surfaces
// `EngineCache`'s doc comment enumerates. A unit test rather than one in
// `tests/reembed.rs` for the same reason `maintain.rs`'s is: `Engine::cache`
// is `pub(crate)` and invalidation is not observable from outside the crate.
#[cfg(test)]
mod cache_invalidation_tests {
    use super::*;
    use crate::EngineConfig;
    use crate::write::RememberRequest;
    use memorysafe_backend_sqlite::SqliteBackend;
    use memorysafe_core::ScopeStats;
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

    /// The positive case. Its negative control is
    /// `a_pass_over_an_empty_scope_leaves_the_cache_alone` below — without
    /// one, "the cache was cleared" is satisfied by clearing it always.
    #[tokio::test]
    async fn reembed_invalidates_the_scopes_cached_stats() {
        let e = engine();
        e.remember(RememberRequest::new(scope(), "a memory to re-embed"))
            .await
            .unwrap();

        e.cache.put_stats(&scope(), sentinel()).await;
        assert!(
            e.cache.stats(&scope()).await.is_some(),
            "premise: cache seeded"
        );

        let report = e.reembed_scope(&scope(), None).await.unwrap();
        assert_eq!(report.embedded, 1, "premise: an actual re-embed happened");
        assert!(
            e.cache.stats(&scope()).await.is_none(),
            "re-embedding must invalidate its scope's cached stats"
        );
    }

    /// The negative control, and what makes `reembed`'s empty-batch early
    /// return load-bearing rather than a redundant shape copied from
    /// `maintain`.
    ///
    /// Invalidation is unconditional on any **non-empty** page — the same
    /// terms `remember` invalidates on, and for the same reason: at worst one
    /// needless refetch, never a stale statistic. The empty-batch arm returns
    /// before reaching it. Note the asymmetry with `maintain`, whose
    /// invalidation sits behind `!to_forget.is_empty() || released > 0` and
    /// so is skipped even on a full page that decided to do nothing; here a
    /// full page that embeds nothing still invalidates. Only a scope with no
    /// rows at all is exempt, because only then was nothing read.
    #[tokio::test]
    async fn a_pass_over_an_empty_scope_leaves_the_cache_alone() {
        let e = engine();

        e.cache.put_stats(&scope(), sentinel()).await;
        let report = e.reembed_scope(&scope(), None).await.unwrap();
        assert_eq!(report.scanned, 0, "premise: the scope really is empty");
        assert!(
            e.cache.stats(&scope()).await.is_some(),
            "a pass that read no rows must not invalidate a scope's cache"
        );
    }
}
