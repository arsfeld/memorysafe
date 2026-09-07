use super::{BackendFactory, fx};
use crate::{Backend, Page};
use memorysafe_core::{Budget, Scope};

pub async fn capacity_accounting_tracks_items_and_bytes<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    backend
        .set_budget(
            &scope,
            Budget {
                max_items: Some(100),
                max_bytes: Some(100_000),
            },
        )
        .await
        .unwrap();

    let before = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(before.used_items, 0);
    assert_eq!(before.used_bytes, 0);

    let item = fx::item(&scope, "a memory of some length");
    let size = item.byte_size();
    backend
        .apply(fx::admit_txn(&scope, item, None))
        .await
        .unwrap();

    let after = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(after.used_items, 1);
    assert_eq!(after.used_bytes, size);
    assert_eq!(after.budget.max_items, Some(100));
}

pub async fn eviction_releases_capacity<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    backend
        .set_budget(
            &scope,
            Budget {
                max_items: Some(10),
                max_bytes: None,
            },
        )
        .await
        .unwrap();

    for i in 0..3 {
        backend
            .apply(fx::admit_txn(
                &scope,
                fx::item(&scope, &format!("memory {i}")),
                None,
            ))
            .await
            .unwrap();
    }
    assert_eq!(backend.capacity_state(&scope).await.unwrap().used_items, 3);

    let items = backend.list(&scope, &Page::default()).await.unwrap();
    backend
        .apply(fx::evict_txn(&scope, vec![items[0].id.clone()]))
        .await
        .unwrap();

    let after = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(after.used_items, 2);
    assert_eq!(
        after.used_bytes,
        items[1].byte_size() + items[2].byte_size()
    );
}

/// A merge folds content into an existing row: `used_items` must not move,
/// and `used_bytes` must move by exactly the signed difference between the
/// merge target's size before and after — never by the merged item's
/// absolute new size, which double-counts the bytes the target already
/// held. `capacity::adjust` is delta-based and never self-heals (see
/// `docs/known-gaps.md`), so a backend that gets this wrong drifts
/// permanently on every merge, and nothing else in this suite would ever
/// notice: `capacity_accounting_tracks_items_and_bytes` and
/// `eviction_releases_capacity` only ever admit or evict, and the only
/// `MergeWrite` this crate's own `atomicity.rs` builds targets an item that
/// does not exist, so `capacity_state` after a *successful* merge is
/// checked nowhere else in the suite.
///
/// Found by a doc comment on `SqliteBackend`'s crate-local test
/// `a_merge_folds_the_item_and_adjusts_capacity_by_the_delta_not_the_new_size`,
/// which named this exact gap in its own module.
pub async fn a_merge_adjusts_capacity_by_the_delta_not_the_new_size<F: BackendFactory>(
    factory: &F,
) {
    use crate::write::MergeWrite;

    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    backend
        .set_budget(
            &scope,
            Budget {
                max_items: Some(100),
                max_bytes: Some(100_000),
            },
        )
        .await
        .unwrap();

    // Setup: two items, so a backend that recomputes the whole scope rather
    // than applying a delta has something else in the corpus to lose track
    // of.
    let target = fx::item(&scope, "a short body");
    let other = fx::item(&scope, "an unrelated second item, untouched throughout");
    for item in [target.clone(), other.clone()] {
        backend
            .apply(fx::admit_txn(&scope, item, None))
            .await
            .unwrap();
    }

    let baseline = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(baseline.used_items, 2);
    assert_eq!(
        baseline.used_bytes,
        target.byte_size() + other.byte_size(),
        "the baseline must be anchored to the two items' own byte_size, not assumed"
    );

    // Grow: merge a body longer than the target's current one.
    let grown_body = "a considerably longer body than the one this target started with";
    let mut grow_txn = fx::admit_txn(&scope, fx::item(&scope, "unused placeholder"), None);
    grow_txn.upsert = None;
    grow_txn.merge = Some(MergeWrite {
        target: target.id.clone(),
        body: grown_body.into(),
        tags: vec![],
        attrs: Default::default(),
        vector: None,
        byte_size: grown_body.len() as u64,
        // Paired with `vector: None` per `MergeWrite::pending_embedding`'s
        // biconditional, now enforced by `is_valid()`. This test is about the
        // capacity delta, not about embedding state; the pairing is what lets
        // the transaction reach the backend at all.
        pending_embedding: true,
    });
    assert!(
        grow_txn.is_valid(),
        "the premise: a merge-only transaction is valid"
    );
    backend.apply(grow_txn).await.unwrap();

    let grown = backend
        .get(&scope, &target.id)
        .await
        .unwrap()
        .expect("the merge target survives its own merge");
    assert_eq!(grown.body, grown_body);

    let after_grow = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(
        after_grow.used_items, 2,
        "a merge folds into an existing row; it must not add one"
    );
    assert_eq!(
        after_grow.used_bytes,
        baseline.used_bytes + (grown.byte_size() - target.byte_size()),
        "used_bytes must move by the target's own before/after size \
         difference, computed from the item fetched after the merge — not \
         by the merged body's raw length"
    );
    // The named mutant: a backend that adjusts capacity by the merged item's
    // absolute new size, as though the merge were a fresh admission, rather
    // than by the delta. Given its own assertion, because a value that
    // happens to land near the correct one by coincidence must not be
    // allowed to pass only the assertion above.
    assert_ne!(
        after_grow.used_bytes,
        baseline.used_bytes + grown.byte_size(),
        "used_bytes must not equal baseline plus the merged item's full new \
         size — that is what a backend gets if it double-counts a merge as \
         a new admission instead of applying the size delta"
    );

    // Shrink: merge again with a body shorter than the current one.
    let shrunk_body = "short";
    let mut shrink_txn = fx::admit_txn(&scope, fx::item(&scope, "unused placeholder"), None);
    shrink_txn.upsert = None;
    shrink_txn.merge = Some(MergeWrite {
        target: target.id.clone(),
        body: shrunk_body.into(),
        tags: vec![],
        attrs: Default::default(),
        vector: None,
        byte_size: shrunk_body.len() as u64,
        // Paired with `vector: None` per `MergeWrite::pending_embedding`'s
        // biconditional, now enforced by `is_valid()`. This test is about the
        // capacity delta, not about embedding state; the pairing is what lets
        // the transaction reach the backend at all.
        pending_embedding: true,
    });
    assert!(
        shrink_txn.is_valid(),
        "the premise: a merge-only transaction is valid"
    );
    backend.apply(shrink_txn).await.unwrap();

    let shrunk = backend
        .get(&scope, &target.id)
        .await
        .unwrap()
        .expect("the merge target survives a second merge");
    assert!(
        shrunk.byte_size() < grown.byte_size(),
        "the premise: the second merge's body must actually shrink the item"
    );

    let after_shrink = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(
        after_shrink.used_items, 2,
        "the second merge must not change used_items either"
    );
    assert!(
        after_shrink.used_bytes < after_grow.used_bytes,
        "used_bytes must decrease when a merge makes an item smaller — a \
         backend applying an unsigned or absolute-value delta, or one that \
         clamps at zero, fails only here"
    );
    assert_eq!(
        after_shrink.used_bytes,
        after_grow.used_bytes - (grown.byte_size() - shrunk.byte_size()),
        "used_bytes must move by exactly the signed before/after \
         difference, including when that difference is negative"
    );

    // The other item is untouched: it still contributes its own size to the
    // total. Both a backend that applies the delta correctly and one that
    // recomputes the whole scope's total from scratch produce the right
    // number here — this assertion does not separate the two, only rules
    // out a backend that lost track of the untouched item.
    assert_eq!(
        after_shrink.used_bytes,
        shrunk.byte_size() + other.byte_size(),
        "the untouched second item must still contribute its own size to \
         the scope's total"
    );
}

/// The correctness detail the spec calls out: without a lock on the accounting
/// row, concurrent admits both conclude there is room and the count drifts.
///
/// `F::B: 'static` is required, not part of the original test logic: without
/// it, `tokio::spawn` cannot accept a future closing over `Arc<F::B>`, since
/// nothing in `BackendFactory` otherwise promises the backend outlives the
/// borrow of `factory`. Every real backend (SQLite's own connection, a
/// Postgres pool) owns its state and satisfies this trivially.
pub async fn concurrent_admits_do_not_double_count<F: BackendFactory>(factory: &F)
where
    F::B: 'static,
{
    use std::sync::Arc;
    let backend = Arc::new(factory.create().await);
    let scope = Scope::new("t", "s", "n").unwrap();
    backend
        .set_budget(
            &scope,
            Budget {
                max_items: Some(1000),
                max_bytes: None,
            },
        )
        .await
        .unwrap();

    let mut handles = Vec::new();
    for i in 0..20 {
        let b = Arc::clone(&backend);
        let s = scope.clone();
        handles.push(tokio::spawn(async move {
            b.apply(fx::admit_txn(
                &s,
                fx::item(&s, &format!("concurrent {i}")),
                None,
            ))
            .await
        }));
    }
    for h in handles {
        h.await.unwrap().unwrap();
    }

    let state = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(
        state.used_items, 20,
        "capacity accounting drifted under concurrency"
    );

    let listed = backend
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
        listed.len() as u64,
        state.used_items,
        "accounting disagrees with reality"
    );
}

pub async fn scope_stats_reflect_the_corpus<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    assert_eq!(backend.scope_stats(&scope).await.unwrap().item_count, 0);

    for body in ["first memory", "second memory", "third memory"] {
        backend
            .apply(fx::admit_txn_embedded(&scope, fx::item(&scope, body)))
            .await
            .unwrap();
    }

    let stats = backend.scope_stats(&scope).await.unwrap();
    assert_eq!(stats.item_count, 3);
    assert!(stats.total_bytes > 0);
    assert!(stats.median_item_bytes > 0);
}

/// Mutation-tests `a_merge_adjusts_capacity_by_the_delta_not_the_new_size`
/// without touching `memorysafe-backend-sqlite`, which this task's boundary
/// puts out of bounds. A minimal in-process `Backend` stands in for it, with
/// a switch between the correct delta-based accounting and the two mutants
/// the task calls for. Running the conformance test against each mode is
/// what makes the mutation-testing claim **verified** rather than
/// **predicted**: this code actually executes, and the process actually
/// panics or does not.
#[cfg(test)]
mod mutation_check {
    use super::*;
    use crate::write::{ItemWrite, MergeWrite, WriteTransaction};
    use crate::{
        AppliedWrite, AuditAggregate, AuditAggregateFilter, BackendError, CandidateQuery,
        ExportStream, ImportReport, ImportStream, PurgeReport, ScopeSelector,
    };
    use memorysafe_core::{
        AuditFilter, AuditId, AuditRecord, CapacityState, Embedding, ItemId, MemoryItem,
        PurgeCascade, ScopeStats, ScoredCandidate, SubjectId, TenantId,
    };
    use std::collections::HashMap;
    use std::future::Future;
    use std::sync::Mutex;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        /// The rule this test is meant to pin: `used_bytes` moves by the
        /// signed before/after difference.
        Correct,
        /// The named mutant this whole task targets: adjusts by the merged
        /// item's absolute new size, as though a merge were a fresh
        /// admission — the double-counting bug the task exists to prevent.
        AbsoluteSize,
        /// The shrink-arm mutant: the delta is made unsigned, so a merge
        /// that shrinks an item still adds to `used_bytes`.
        UnsignedDelta,
    }

    struct FakeBackend {
        mode: Mode,
        items: Mutex<HashMap<ItemId, MemoryItem>>,
        // (used_items, used_bytes-as-signed, budget). Signed so a mutant can
        // be observed pushing it the wrong way before the real-backend-style
        // `MAX(0, ..)` clamp is applied on read — exactly what
        // `memorysafe-backend-sqlite`'s own `capacity::adjust` does.
        state: Mutex<(u64, i64, Budget)>,
    }

    impl FakeBackend {
        fn new(mode: Mode) -> Self {
            Self {
                mode,
                items: Mutex::new(HashMap::new()),
                state: Mutex::new((0, 0, Budget::UNBOUNDED)),
            }
        }
    }

    #[async_trait::async_trait]
    impl Backend for FakeBackend {
        async fn retrieve_candidates(
            &self,
            _scope: &Scope,
            _query: &CandidateQuery,
        ) -> Result<Vec<ScoredCandidate>, BackendError> {
            unimplemented!("not exercised by the mutation check")
        }

        async fn neighbours(
            &self,
            _scope: &Scope,
            _embedding: &Embedding,
            _k: usize,
        ) -> Result<Vec<ScoredCandidate>, BackendError> {
            unimplemented!("not exercised by the mutation check")
        }

        async fn capacity_state(&self, _scope: &Scope) -> Result<CapacityState, BackendError> {
            let (used_items, used_bytes, budget) = *self.state.lock().unwrap();
            Ok(CapacityState {
                budget,
                used_items,
                used_bytes: used_bytes.max(0) as u64,
            })
        }

        async fn scope_stats(&self, _scope: &Scope) -> Result<ScopeStats, BackendError> {
            unimplemented!("not exercised by the mutation check")
        }

        async fn apply(&self, txn: WriteTransaction) -> Result<AppliedWrite, BackendError> {
            if !txn.is_valid() {
                return Err(BackendError::InvalidTransaction("invalid".into()));
            }
            let mut items = self.items.lock().unwrap();
            let mut item_id = None;

            if let Some(ItemWrite { item, .. }) = txn.upsert {
                let mut state = self.state.lock().unwrap();
                state.0 += 1;
                state.1 += item.byte_size() as i64;
                item_id = Some(item.id.clone());
                items.insert(item.id.clone(), item);
            }

            if let Some(MergeWrite {
                target,
                body,
                tags,
                attrs,
                ..
            }) = txn.merge
            {
                let existing = items
                    .get(&target)
                    .cloned()
                    .ok_or_else(|| BackendError::MergeTargetMissing(target.clone()))?;
                let before = existing.byte_size() as i64;
                let mut updated = existing;
                updated.body = body;
                for t in tags {
                    if !updated.tags.contains(&t) {
                        updated.tags.push(t);
                    }
                }
                for (k, v) in attrs {
                    updated.attrs.insert(k, v);
                }
                let after = updated.byte_size() as i64;
                items.insert(target.clone(), updated);
                item_id = Some(target);

                let delta = match self.mode {
                    Mode::Correct => after - before,
                    Mode::AbsoluteSize => after,
                    Mode::UnsignedDelta => (after - before).abs(),
                };
                self.state.lock().unwrap().1 += delta;
            }

            for id in &txn.evictions {
                if let Some(removed) = items.remove(id) {
                    let mut state = self.state.lock().unwrap();
                    state.0 -= 1;
                    state.1 -= removed.byte_size() as i64;
                }
            }

            Ok(AppliedWrite {
                item_id,
                audit_id: txn.audit.id.clone(),
                evicted: vec![],
                replayed: false,
                replayed_outcome: None,
            })
        }

        async fn record_recall(&self, _record: AuditRecord) -> Result<AuditId, BackendError> {
            unimplemented!("not exercised by the mutation check")
        }

        async fn get(
            &self,
            _scope: &Scope,
            id: &ItemId,
        ) -> Result<Option<MemoryItem>, BackendError> {
            Ok(self.items.lock().unwrap().get(id).cloned())
        }

        async fn list(
            &self,
            _scope: &Scope,
            _page: &Page,
        ) -> Result<Vec<MemoryItem>, BackendError> {
            unimplemented!("not exercised by the mutation check")
        }

        async fn audit(
            &self,
            _scope: &Scope,
            _filter: &AuditFilter,
        ) -> Result<Vec<AuditRecord>, BackendError> {
            unimplemented!("not exercised by the mutation check")
        }

        async fn purge_subject(
            &self,
            _tenant: &TenantId,
            _subject: &SubjectId,
            _cascade: PurgeCascade,
            _audit: AuditRecord,
        ) -> Result<PurgeReport, BackendError> {
            unimplemented!("not exercised by the mutation check")
        }

        async fn audit_aggregates(
            &self,
            _tenant: &TenantId,
            _filter: &AuditAggregateFilter,
        ) -> Result<Vec<AuditAggregate>, BackendError> {
            unimplemented!("not exercised by the mutation check")
        }

        async fn export(&self, _sel: &ScopeSelector) -> Result<ExportStream, BackendError> {
            unimplemented!("not exercised by the mutation check")
        }

        async fn import(
            &self,
            _destination: &TenantId,
            _stream: ImportStream,
        ) -> Result<ImportReport, BackendError> {
            unimplemented!("not exercised by the mutation check")
        }

        async fn set_budget(&self, _scope: &Scope, budget: Budget) -> Result<(), BackendError> {
            self.state.lock().unwrap().2 = budget;
            Ok(())
        }
    }

    #[derive(Clone, Copy)]
    struct FakeFactory(Mode);

    impl BackendFactory for FakeFactory {
        type B = FakeBackend;
        fn create(&self) -> impl Future<Output = Self::B> + Send {
            std::future::ready(FakeBackend::new(self.0))
        }
    }

    /// Sanity check on the harness itself: a correct, delta-based
    /// implementation must pass the conformance test. Without this, a bug in
    /// `FakeBackend` could make every mode fail and the two mutant tests
    /// below would be meaningless.
    #[tokio::test]
    async fn the_test_passes_against_a_correct_delta_based_implementation() {
        a_merge_adjusts_capacity_by_the_delta_not_the_new_size(&FakeFactory(Mode::Correct)).await;
    }

    /// The named mutant this whole task targets: capacity adjusted by the
    /// merged item's absolute new size rather than the delta. Caught by the
    /// grow arm's exact-value assertion — the two values differ enough here
    /// that the dedicated `assert_ne!` a few lines below is never reached.
    #[tokio::test]
    #[should_panic(expected = "used_bytes must move by the target's own before/after size")]
    async fn the_absolute_size_mutant_is_caught() {
        a_merge_adjusts_capacity_by_the_delta_not_the_new_size(&FakeFactory(Mode::AbsoluteSize))
            .await;
    }

    /// The shrink-arm mutant: an unsigned (`abs`-wrapped) delta. The grow arm
    /// cannot distinguish this from the correct rule — a positive delta
    /// survives `abs` unchanged — so only the shrink assertion catches it.
    #[tokio::test]
    #[should_panic(expected = "used_bytes must decrease when a merge makes an item smaller")]
    async fn the_unsigned_delta_mutant_is_caught_by_the_shrink_arm() {
        a_merge_adjusts_capacity_by_the_delta_not_the_new_size(&FakeFactory(Mode::UnsignedDelta))
            .await;
    }
}
