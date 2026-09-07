use crate::error::EngineError;
use memorysafe_backend::Backend;
use memorysafe_core::{
    AdmitContext, AssessContext, Decision, Embedding, MaintenanceCandidate, ReasonCode, Scope,
};
use time::OffsetDateTime;

/// Gathers everything `assess` may see. Not one I/O pass: this makes up to
/// two backend round trips (`neighbours`, when there is an embedding to
/// probe with, and `scope_stats`, always) before `assess` ever runs.
pub async fn assess_context(
    backend: &dyn Backend,
    scope: &Scope,
    embedding: Option<&Embedding>,
    k: usize,
) -> Result<AssessContext, EngineError> {
    // `?`, matching `scope_stats` on the next line — not `unwrap_or_default()`.
    // A transient failure here must not silently degrade into "this scope has
    // no neighbours": that reading feeds three consequences downstream, none
    // of them visible in the outcome. `assess` sees no near-duplicates, so an
    // identical rewrite is admitted as novel instead of rejected;
    // `mean_neighbour_similarity` becomes `0.0`; and `fragility::score`
    // short-circuits to `Score::ONE` on an empty neighbour list, so the very
    // duplicate that should have been rejected is instead stored `Protected`
    // for a full protection window. Nothing in `WriteOutcome`, the stored
    // item, or the audit row would distinguish "this scope genuinely has no
    // neighbours" from "the neighbour query failed" — a governance *input*
    // failing closed is the safer default here, the same asymmetry the
    // embedding path resolves the other way for a governance *output*
    // (`pending_embedding` degrades gracefully because losing one embedding
    // costs one item's recall quality, not a duplicate-detection guarantee).
    let neighbours = match embedding {
        Some(e) => backend.neighbours(scope, e, k).await?,
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
            // OPEN: every field below except `item` is fabricated, not
            // measured, and this is the whole comment, not just the
            // access-statistics half of it.
            //
            // `value` and `fragility` are both a hardcoded `Score::clamped(0.5)`
            // for every candidate — computing the real ones needs a neighbour
            // query per candidate, a design decision this task cannot make
            // (see `BaselinePolicy::assess`, which the policy crate spends a
            // whole module on for exactly one candidate at a time), and
            // `MaintenanceCandidate::value`/`::fragility` are plain `Score`,
            // not `Option<Score>` — the type gives this function no way to
            // say "unknown" instead of handing over a number. Two
            // consequences follow from the tie. `eviction::cost` is `value *
            // fragility`, so every candidate here ties at `0.25`, and
            // `admit`'s stable sort then leaves the order exactly as
            // `Backend::list` returned it — ascending `created_at` — so
            // "evict the lowest value-weighted retention cost" degrades to
            // "evict the oldest" without anything saying so; that ordering
            // effect is unchanged by anything below and is not this task's to
            // fix. The second consequence — `admit` writing `"value" => 0.5,
            // "fragility" => 0.5, "eviction_cost" => 0.25` into the eviction
            // `Reason`'s evidence, three fabricated constants presented as
            // measurements in a compliance record — IS fixed, but not here:
            // `memorysafe-policy` builds that evidence map from these two
            // fields, and this task does not touch that crate. Instead
            // `strip_fabricated_eviction_evidence` below scrubs the
            // `Decision` `admit` hands back, once, in `write.rs`, right
            // before it is written into an audit row — turning "0.5 asserted
            // as measured" into an explicit "not computed" rather than
            // leaving a silent placeholder in the trail. It does not, and
            // cannot, touch `eviction::cost` or the stable sort above: by the
            // time it runs, which items were evicted and in what order is
            // already decided.
            //
            // `last_accessed_at`/`access_count` are the second, narrower gap:
            // `Backend::list` returns bare `MemoryItem`s, and the access
            // statistics deliberately do not live on that type, so this path
            // has no source for them. `(None, 0)` is NOT "unknown" here — the
            // ruling recorded at compose's staleness fallback (see
            // `replay_due` in Task 28) makes `(None, 0)` mean the item has
            // never been accessed, definitively, everywhere this pair
            // appears. Passing it for an item whose access history is merely
            // unavailable therefore states something false, not merely
            // something imprecise: a frequently-recalled item offered for
            // eviction reads to the policy as indistinguishable from one
            // nobody has ever touched. Before `admit` is allowed to weigh
            // staleness, the listing path needs to carry the real statistics
            // — a `list` that returns them beside each item, or a dedicated
            // read.
            //
            // Flagged by the contract task that added these fields; not
            // solved there. Recorded separately for the final whole-branch
            // review as a design gap, not something this task is asked to
            // close — the job here is that this comment stop hiding half of
            // what it names.
            .map(|item| MaintenanceCandidate {
                value: memorysafe_core::Score::clamped(0.5),
                fragility: memorysafe_core::Score::clamped(0.5),
                item,
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

/// Evidence keys whose value comes straight from the `value`/`fragility`
/// placeholder `admit_context` hands every eviction candidate above — see
/// that function's `OPEN` comment. `memorysafe-policy`'s `admit` (a separate,
/// independently owned crate this task does not modify) copies those two
/// numbers, plus their product, verbatim into a `CapacityPressure` eviction's
/// `Reason::evidence` under exactly these three keys.
const FABRICATED_EVICTION_EVIDENCE: [&str; 3] = ["value", "fragility", "eviction_cost"];

/// Scrubs the fabricated eviction evidence `admit` returns before a
/// `Decision` is written into an audit row.
///
/// An evidence field carrying a placeholder is a false attestation, and it is
/// worse than an absent one because a reader of the audit trail cannot tell
/// them apart. Since the placeholder originates here (`admit_context`, above)
/// but the evidence map it taints is assembled downstream, in a crate this
/// task must not touch, the fix has to be a scrub applied to what that crate
/// hands back, not a change to how it builds the map. `write.rs`'s `remember`
/// is the only caller, applied once, right before `AuditRecord::with_decision`
/// — everywhere else `decision` is used (`txn.evictions`, the returned
/// `WriteOutcome`), only item ids and top-level `reasons` are read, never an
/// eviction's evidence map, so scrubbing it here changes nothing else.
///
/// Deliberately narrow: only a `CapacityPressure` eviction's evidence is
/// touched, since that is the one reason code `admit` builds from these two
/// fields today. Replaces the three fabricated keys with a single explicit
/// `"value_fragility_computed" => 0.0` flag — a reader can tell "computed:
/// no" from a genuine `0.0` measurement, which a silently-absent key could
/// not.
///
/// Does not, and must not, touch `eviction::cost`, the stable sort in
/// `admit`, or `decision.evictions`'s order or membership: all three are
/// already fixed by the time this runs, so which items were evicted and in
/// what order is unaffected — only the evidence recorded about each one
/// changes.
pub(crate) fn strip_fabricated_eviction_evidence(decision: &mut Decision) {
    for eviction in &mut decision.evictions {
        if eviction.reason.code != ReasonCode::CapacityPressure {
            continue;
        }
        for key in FABRICATED_EVICTION_EVIDENCE {
            eviction.reason.evidence.remove(key);
        }
        eviction
            .reason
            .evidence
            .insert("value_fragility_computed".to_string(), 0.0);
    }
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
        /// When set, `neighbours()` returns this error instead of `Ok`.
        neighbours_error: bool,
        scope_stats: ScopeStats,
        capacity: CapacityState,
        list: Mutex<Vec<MemoryItem>>,
    }

    impl Default for FakeBackend {
        fn default() -> Self {
            Self {
                neighbours: vec![],
                neighbours_error: false,
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
            if self.neighbours_error {
                return Err(BackendError::Storage {
                    message: "vector store unavailable".into(),
                    retryable: true,
                });
            }
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
    async fn a_neighbour_lookup_failure_propagates_rather_than_degrading_to_no_neighbours() {
        // `unwrap_or_default()` here would silently read a transient backend
        // failure as "this scope has no neighbours" — indistinguishable from
        // the genuine case, and one that cascades into admitting a duplicate
        // as novel and protecting it for a full window (see the doc comment
        // on the call site). `?`, matching `scope_stats`, must surface it.
        let backend = FakeBackend {
            neighbours_error: true,
            ..Default::default()
        };
        let err = assess_context(&backend, &scope(), Some(&embedding()), 16)
            .await
            .expect_err("a neighbour lookup failure must not be swallowed");
        assert!(matches!(
            err,
            EngineError::Backend(BackendError::Storage { .. })
        ));
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
