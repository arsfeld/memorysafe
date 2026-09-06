use crate::error::EngineError;
use memorysafe_backend::Backend;
use memorysafe_core::{AdmitContext, AssessContext, Embedding, MaintenanceCandidate, Scope};
use time::OffsetDateTime;

/// One I/O pass producing everything `assess` may see.
pub async fn assess_context(
    backend: &dyn Backend,
    scope: &Scope,
    embedding: Option<&Embedding>,
    k: usize,
) -> Result<AssessContext, EngineError> {
    let neighbours = match embedding {
        Some(e) => backend.neighbours(scope, e, k).await.unwrap_or_default(),
        None => vec![],
    };
    let mut stats = backend.scope_stats(scope).await?;
    // The backend has no cheap way to compute this; the engine derives it from
    // the neighbours it just fetched.
    stats.mean_neighbour_similarity = if neighbours.is_empty() {
        0.0
    } else {
        neighbours.iter().map(|n| n.relevance).sum::<f32>() / neighbours.len() as f32
    };
    Ok(AssessContext {
        scope: scope.clone(),
        neighbours,
        stats,
        now: OffsetDateTime::now_utc(),
    })
}

/// Eviction candidates are filtered here, not in the policy: pinned items and
/// unexpired protection windows never reach it.
pub async fn admit_context(
    backend: &dyn Backend,
    scope: &Scope,
    assess: &AssessContext,
    limit: usize,
) -> Result<AdmitContext, EngineError> {
    let capacity = backend.capacity_state(scope).await?;
    let now = OffsetDateTime::now_utc();

    let eviction_candidates: Vec<MaintenanceCandidate> = if capacity.budget.is_bounded() {
        let page = memorysafe_backend::Page { offset: 0, limit };
        backend
            .list(scope, &page)
            .await?
            .into_iter()
            .filter(|i| i.protection.is_evictable(now))
            .map(|item| MaintenanceCandidate {
                value: memorysafe_core::Score::clamped(0.5),
                fragility: memorysafe_core::Score::clamped(0.5),
                item,
                // OPEN: `Backend::list` returns bare `MemoryItem`s, and the
                // access statistics deliberately do not live on that type, so
                // this path has no source for them. `(None, 0)` is NOT
                // "unknown" here — the ruling recorded at compose's staleness
                // fallback (see `replay_due` in Task 28) makes `(None, 0)`
                // mean the item has never been accessed, definitively,
                // everywhere this pair appears. Passing it for an item whose
                // access history is merely unavailable therefore states
                // something false, not merely something imprecise: a
                // frequently-recalled item offered for eviction reads to the
                // policy as indistinguishable from one nobody has ever
                // touched. Before `admit` is allowed to weigh staleness, the
                // listing path needs to carry the real statistics — a `list`
                // that returns them beside each item, or a dedicated read.
                // Flagged by the contract task that added these fields; not
                // solved there.
                last_accessed_at: None,
                access_count: 0,
            })
            .collect()
    } else {
        vec![]
    };

    Ok(AdmitContext {
        scope: scope.clone(),
        capacity,
        eviction_candidates,
        stats: assess.stats.clone(),
        now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_backend::{
        AppliedWrite, AuditAggregate, AuditAggregateFilter, BackendError, CandidateQuery,
        ExportStream, ImportReport, ImportStream, Page, PurgeReport, ScopeSelector,
        WriteTransaction,
    };
    use memorysafe_core::{
        AuditFilter, AuditId, AuditRecord, Budget, CapacityState, ItemId, MemoryItem, Protection,
        PurgeCascade, ScopeStats, ScoredCandidate, SubjectId, TenantId,
    };
    use std::sync::Mutex;

    /// A minimal, controllable `Backend`: every method the code under test
    /// does not call panics if reached (`unimplemented!`); the ones it does
    /// call return exactly what the test configured, not a plausible-looking
    /// default. `NullBackend` in `memorysafe-backend` is `#[cfg(test)]`-only
    /// there and not visible to this crate, so this is its own minimal
    /// double rather than a copy.
    struct FakeBackend {
        neighbours: Vec<ScoredCandidate>,
        scope_stats: ScopeStats,
        capacity: CapacityState,
        list: Mutex<Vec<MemoryItem>>,
    }

    impl Default for FakeBackend {
        fn default() -> Self {
            Self {
                neighbours: vec![],
                scope_stats: ScopeStats::default(),
                capacity: CapacityState {
                    budget: Budget::UNBOUNDED,
                    used_items: 0,
                    used_bytes: 0,
                },
                list: Mutex::new(vec![]),
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
            unimplemented!("not exercised by gather::assess_context/admit_context")
        }

        async fn neighbours(
            &self,
            _scope: &Scope,
            _embedding: &Embedding,
            _k: usize,
        ) -> Result<Vec<ScoredCandidate>, BackendError> {
            Ok(self.neighbours.clone())
        }

        async fn capacity_state(&self, _scope: &Scope) -> Result<CapacityState, BackendError> {
            Ok(self.capacity)
        }

        async fn scope_stats(&self, _scope: &Scope) -> Result<ScopeStats, BackendError> {
            Ok(self.scope_stats.clone())
        }

        async fn apply(&self, _txn: WriteTransaction) -> Result<AppliedWrite, BackendError> {
            unimplemented!("not exercised by gather::assess_context/admit_context")
        }

        async fn record_recall(&self, _record: AuditRecord) -> Result<AuditId, BackendError> {
            unimplemented!("not exercised by gather::assess_context/admit_context")
        }

        async fn get(
            &self,
            _scope: &Scope,
            _id: &ItemId,
        ) -> Result<Option<MemoryItem>, BackendError> {
            unimplemented!("not exercised by gather::assess_context/admit_context")
        }

        async fn list(
            &self,
            _scope: &Scope,
            _page: &Page,
        ) -> Result<Vec<MemoryItem>, BackendError> {
            Ok(self.list.lock().unwrap().clone())
        }

        async fn audit(
            &self,
            _scope: &Scope,
            _filter: &AuditFilter,
        ) -> Result<Vec<AuditRecord>, BackendError> {
            unimplemented!("not exercised by gather::assess_context/admit_context")
        }

        async fn purge_subject(
            &self,
            _tenant: &TenantId,
            _subject: &SubjectId,
            _cascade: PurgeCascade,
            _audit: AuditRecord,
        ) -> Result<PurgeReport, BackendError> {
            unimplemented!("not exercised by gather::assess_context/admit_context")
        }

        async fn audit_aggregates(
            &self,
            _tenant: &TenantId,
            _filter: &AuditAggregateFilter,
        ) -> Result<Vec<AuditAggregate>, BackendError> {
            unimplemented!("not exercised by gather::assess_context/admit_context")
        }

        async fn export(&self, _sel: &ScopeSelector) -> Result<ExportStream, BackendError> {
            unimplemented!("not exercised by gather::assess_context/admit_context")
        }

        async fn import(
            &self,
            _destination: &TenantId,
            _stream: ImportStream,
        ) -> Result<ImportReport, BackendError> {
            unimplemented!("not exercised by gather::assess_context/admit_context")
        }

        async fn set_budget(&self, _scope: &Scope, _budget: Budget) -> Result<(), BackendError> {
            unimplemented!("not exercised by gather::assess_context/admit_context")
        }
    }

    fn scope() -> Scope {
        Scope::new("t", "s", "n").unwrap()
    }

    fn item(protection: Protection) -> MemoryItem {
        MemoryItem {
            id: ItemId::new(),
            scope: scope(),
            body: "x".into(),
            kind: "fact".into(),
            source: memorysafe_core::Source {
                kind: memorysafe_core::SourceKind::Agent,
                id: None,
            },
            occurred_at: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            tags: vec![],
            attrs: Default::default(),
            sensitivity: memorysafe_core::SensitivityLevel::Internal,
            ttl: None,
            protection,
            pending_embedding: false,
        }
    }

    fn candidate(relevance: f32) -> ScoredCandidate {
        ScoredCandidate {
            item: item(Protection::Normal),
            relevance,
            vector_score: None,
            keyword_score: None,
            value: memorysafe_core::Score::clamped(0.5),
            fragility: memorysafe_core::Score::clamped(0.5),
            estimated_tokens: 1,
            last_accessed_at: None,
            access_count: 0,
        }
    }

    fn embedding() -> Embedding {
        Embedding::new(vec![1.0, 0.0], memorysafe_core::EmbedderId::new("test"))
    }

    #[tokio::test]
    async fn mean_neighbour_similarity_is_the_mean_not_the_sum() {
        // Two neighbours whose relevances sum to more than either alone —
        // `sum` and `mean` disagree here, unlike with a single neighbour,
        // where they are numerically identical.
        let backend = FakeBackend {
            neighbours: vec![candidate(0.2), candidate(0.8)],
            ..Default::default()
        };
        let ctx = assess_context(&backend, &scope(), Some(&embedding()), 16)
            .await
            .unwrap();
        assert_eq!(
            ctx.stats.mean_neighbour_similarity, 0.5,
            "expected the mean of 0.2 and 0.8, not their sum"
        );
    }

    #[tokio::test]
    async fn no_embedding_means_no_neighbours_are_fetched() {
        // `backend.neighbours` is configured to return a nonempty set; if
        // `assess_context` fetched it regardless of `embedding`, this would
        // come back nonempty despite `embedding: None`.
        let backend = FakeBackend {
            neighbours: vec![candidate(0.9)],
            ..Default::default()
        };
        let ctx = assess_context(&backend, &scope(), None, 16).await.unwrap();
        assert!(
            ctx.neighbours.is_empty(),
            "no embedding means nothing to compare neighbours against"
        );
        assert_eq!(ctx.stats.mean_neighbour_similarity, 0.0);
    }

    #[tokio::test]
    async fn an_unbounded_budget_offers_no_eviction_candidates_even_with_items_present() {
        let backend = FakeBackend {
            capacity: CapacityState {
                budget: Budget::UNBOUNDED,
                used_items: 5,
                used_bytes: 500,
            },
            list: Mutex::new(vec![item(Protection::Normal)]),
            ..Default::default()
        };
        let assess = AssessContext {
            scope: scope(),
            neighbours: vec![],
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        };
        let ctx = admit_context(&backend, &scope(), &assess, 128)
            .await
            .unwrap();
        assert!(
            ctx.eviction_candidates.is_empty(),
            "an unbounded budget has nothing to evict for, regardless of what `list` returns"
        );
    }

    #[tokio::test]
    async fn a_bounded_budget_offers_evictable_items_but_excludes_pinned_ones() {
        let evictable = item(Protection::Normal);
        let pinned = item(Protection::Pinned);
        let backend = FakeBackend {
            capacity: CapacityState {
                budget: Budget {
                    max_items: Some(10),
                    max_bytes: None,
                },
                used_items: 2,
                used_bytes: 200,
            },
            list: Mutex::new(vec![evictable.clone(), pinned]),
            ..Default::default()
        };
        let assess = AssessContext {
            scope: scope(),
            neighbours: vec![],
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        };
        let ctx = admit_context(&backend, &scope(), &assess, 128)
            .await
            .unwrap();
        assert_eq!(
            ctx.eviction_candidates.len(),
            1,
            "the pinned item must be excluded from the offered candidates"
        );
        assert_eq!(ctx.eviction_candidates[0].item.id, evictable.id);
    }
}
