use crate::Engine;
use crate::error::EngineError;
use crate::outcome::{ForgetOutcome, PurgeOutcome, WriteOutcome};
use memorysafe_backend::{ItemWrite, Page, WriteTransaction};
use memorysafe_core::{
    Action, Actor, AuditEvent, AuditRecord, ItemId, ItemRef, Protection, Reason, ReasonCode, Scope,
    SubjectId, TenantId, features,
};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgetSelector {
    Ids(Vec<ItemId>),
    Tag(String),
    Kind(String),
}

/// **Page size** for the selector-based forget scan — not a bound on how much
/// a forget may remove.
///
/// It was the latter, and that was the defect: `ForgetSelector::Tag` and
/// `ForgetSelector::Kind` read one page at offset 0 and stopped, so a
/// 5,000-item scope forgot the matches in the first 1,000 and reported
/// success. In a product that sells erasure, a silent partial delete reported
/// as a complete one is the wrong failure mode — worse than an error, because
/// the caller has no way to discover it.
///
/// `forget` now pages to exhaustion instead of reporting truncation. The two
/// were the real alternatives, and this is why paging won: a `truncated` flag
/// on `ForgetOutcome` pushes the loop onto every caller Plan 3 adds — the HTTP
/// route, the CLI command, the MCP tool — and a caller that forgets to loop
/// reproduces exactly this bug one layer up, where it is harder to see. The
/// cost of paging is that a very large selector does more work in one call,
/// and it costs memory as well as time: `scan_all` reads one page at a time,
/// but every *matched* item is kept — as a whole `MemoryItem`, body included —
/// until the transaction is applied, because the audit record's `ItemRef`s are
/// built from the same items. Peak memory is therefore proportional to the
/// selector's total reach times the item size, not to the page size. That is
/// the price of the single **`WriteTransaction`** below, which is what makes
/// the erasure atomic, and it wants a bound before Plan 3 exposes `forget`
/// over HTTP. A paged *write* would leave a
/// partially-forgotten scope observable between transactions, which is the
/// defect this fixes, not a smaller version of it.
const FORGET_SCAN_LIMIT: usize = 1000;

impl Engine {
    /// The existence pre-check on `ForgetSelector::Ids` below
    /// (`self.backend.get(scope, &id).await?.is_some()`) looks like
    /// defence-in-depth against an already-safe design — `ForgetOutcome` is
    /// identical with or without it, since it is built from
    /// `AppliedWrite::evicted` (what the backend actually removed, itself
    /// scope-filtered), never from the raw selector. **It is exactly that:
    /// defence in depth against a `Backend` whose eviction does not cascade
    /// a removed item's other rows — its vector row, most concretely —
    /// under the same scope predicate as the row itself.** A backend
    /// failing that property would let naming an id from a different scope
    /// in the same tenant leave that item's row untouched but permanently
    /// strip its vector, silently: `ForgetOutcome` would report nothing
    /// forgotten, and the audit trail would show nothing happened. See
    /// `forgetting_an_id_from_another_scope_does_not_delete_its_vector`.
    pub async fn forget(
        &self,
        scope: &Scope,
        selector: ForgetSelector,
    ) -> Result<ForgetOutcome, EngineError> {
        // The matched items themselves, not just their ids: the audit record
        // below needs an `ItemRef` per target, and an `ItemRef` is an id *and*
        // a content digest, which only the item can produce.
        let matched: Vec<memorysafe_core::MemoryItem> = match selector {
            ForgetSelector::Ids(ids) => {
                let mut present = Vec::new();
                for id in ids {
                    if let Some(item) = self.backend.get(scope, &id).await? {
                        present.push(item);
                    }
                }
                present
            }
            ForgetSelector::Tag(tag) => self.scan_all(scope, |i| i.tags.contains(&tag)).await?,
            ForgetSelector::Kind(kind) => self.scan_all(scope, |i| i.kind == kind).await?,
        };
        let targets: Vec<ItemId> = matched.iter().map(|i| i.id.clone()).collect();

        // Populated, not left empty. Every other mutating path in this engine
        // names its subjects in the audit record, and a `Forgotten` row that
        // names nothing is the least useful of them all to have that gap: the
        // items it is about are gone, so the row is the only remaining
        // evidence of *which* memories were erased. `ItemRef` carries an id
        // and a digest and never a body, so this discloses nothing a
        // `Recalled` row does not.
        let refs: Vec<ItemRef> = matched.iter().map(ItemRef::from_item).collect();
        // `Actor::system()`, not a caller-identified human. Plan 3's Task 2
        // threaded a real `Actor` through `purge_subject` below,
        // `set_tenant_policy_config`/`set_tenant_retention` and
        // `export_ndjson_as`/`import_ndjson_as` (`lib.rs`) — but not through
        // `forget`, which was out of that task's scope and still has no actor
        // parameter to attribute this row to.
        let audit = AuditRecord::new(
            scope.clone(),
            AuditEvent::Forgotten,
            refs,
            Actor::system(),
            OffsetDateTime::now_utc(),
        );
        let mut txn = WriteTransaction::new(scope.clone(), audit);
        txn.evictions = targets;

        let applied = self.backend.apply(txn).await?;
        // Any write invalidates its scope (see `write.rs`'s own call for the
        // full rationale): a forget changes `item_count`/`total_bytes` for
        // `scope` exactly as an admission does.
        self.cache.invalidate_scope(scope).await;
        Ok(ForgetOutcome {
            forgotten: applied.evicted,
            audit_id: applied.audit_id,
        })
    }

    /// Every item in `scope` matching `keep`, read a page at a time until the
    /// scope is exhausted.
    ///
    /// **Termination.** `Backend::list` is contractually a *total* order over
    /// the whole scope with disjoint, complete pages, so advancing `offset` by
    /// the number of rows returned reaches the end of a finite scope; the loop
    /// stops on the first short page, which is the same "there is nothing
    /// after it" signal `maintain` and `reembed` derive their cursors from.
    /// Nothing is deleted while this runs, so no page shifts under the offset
    /// — the hazard `maintain`'s own `next_offset` arithmetic exists to
    /// handle, and which does not arise here precisely because scan and
    /// removal are separated.
    async fn scan_all(
        &self,
        scope: &Scope,
        keep: impl Fn(&memorysafe_core::MemoryItem) -> bool,
    ) -> Result<Vec<memorysafe_core::MemoryItem>, EngineError> {
        let mut matched = Vec::new();
        let mut offset = 0usize;
        loop {
            let page = self
                .backend
                .list(
                    scope,
                    &Page {
                        offset,
                        limit: FORGET_SCAN_LIMIT,
                    },
                )
                .await?;
            let read = page.len();
            matched.extend(page.into_iter().filter(&keep));
            if read < FORGET_SCAN_LIMIT {
                return Ok(matched);
            }
            offset += read;
        }
    }

    /// The only path that changes `protection` outside admission.
    ///
    /// Its own audit record's actor is `Actor::system()`, for the same
    /// engine-wide, deferred reason `purge_subject`'s doc comment below gives
    /// in full.
    ///
    /// **This method's brief (the plan document's Task 34 note, and the
    /// design spec it echoes) claims this audit record makes "why is this
    /// pinned?" answerable from the trail alone. It does not deliver that
    /// today.** The record written below is `AuditEvent::Admitted` carrying
    /// no assessment and no decision — `with_assessment`/`with_decision` are
    /// never called on it, because there is no `Assessment` or `Decision` to
    /// attach: this call never runs the admission pipeline that produces
    /// either. A reader of the trail sees that the item's protection changed
    /// and when, but not why an operator or caller pinned it — the one
    /// question the brief promises is answerable. The gap is not a missing
    /// call in this function; it is that `memorysafe_core::AuditEvent` has no
    /// protection-related variant to record a reason against in the first
    /// place, and adding one is a `memorysafe-core` change, out of reach from
    /// this crate. **Deferred to Plan 3 Task 9**, bundled there with
    /// `AuditFilter.subject`'s inexpressible compliance query ("every audit
    /// row for subject X") for the same reason: both are core additions the
    /// engine can consume but not make itself. Until that lands, this
    /// method's own claim to answer "why is this pinned?" is aspirational,
    /// not delivered.
    pub async fn protect(
        &self,
        scope: &Scope,
        id: &ItemId,
        protection: Protection,
    ) -> Result<WriteOutcome, EngineError> {
        let Some(mut item) = self.backend.get(scope, id).await? else {
            return Err(EngineError::NotFound(id.to_string()));
        };
        item.protection = protection;

        // Deleting the row (the eviction below) cascades its vector away, so
        // the item must be re-embedded before it is written back. One rule,
        // already used on the write path (`write.rs`'s `remember`:
        // `let embedding = self.embed_cached(&req.body).await; let
        // pending_embedding = embedding.is_none();`): `pending_embedding` is
        // derived from the SAME `Option` the vector comes from, not left at
        // whatever the fetched item already carried. Skipping this — as an
        // earlier version of this method did — let an embedder failure here
        // leave the item with no vector row (the delete above cascaded it
        // away) AND `pending_embedding: false`, which is invisible to both
        // vector search and the backfill job that exists to repair exactly
        // that gap.
        let embedding = self.embedder.embed(&item.body).ok();
        item.pending_embedding = embedding.is_none();
        let vector = embedding
            .as_ref()
            .map(memorysafe_embed::QuantizedVector::from_embedding);

        let audit = AuditRecord::new(
            scope.clone(),
            AuditEvent::Admitted,
            vec![ItemRef::from_item(&item)],
            Actor::system(),
            OffsetDateTime::now_utc(),
        );
        let mut txn = WriteTransaction::new(scope.clone(), audit);
        // Replace the row: delete then insert, in one transaction.
        // Delete-then-insert is required, not incidental — mutation B7 (see
        // this task's report) confirmed a plain re-insert over an existing
        // id fails with a UNIQUE constraint, so this shape is load-bearing.
        //
        // A stored row also carries columns that do not live on `MemoryItem`
        // and that `items::insert` therefore cannot write, so this shape used
        // to reset an item's whole accumulated recall history the moment it
        // was pinned. **That is now a `Backend::apply` requirement rather than
        // a cost this method pays:** a transaction whose `evictions` and
        // `upsert` name the same id is a *replacement*, and `apply` must carry
        // the item's `last_accessed_at`/`access_count` across it. See that
        // method's contract, and `items::AccessHistory` in
        // `memorysafe-backend-sqlite` for the two columns and who reads them
        // (`value_score` and `fragility_score` are the other two, and are
        // written by nothing in the workspace — preserving them would preserve
        // zeros). Pinned by
        // `protecting_an_item_preserves_its_accumulated_access_history`.
        txn.evictions = vec![id.clone()];
        txn.upsert = Some(ItemWrite { item, vector });

        let applied = self.backend.apply(txn).await?;
        // Same rationale as `forget` above: `protect`'s delete-then-reinsert
        // is a write to `scope`, so its cached stats must not outlive it.
        self.cache.invalidate_scope(scope).await;
        Ok(WriteOutcome {
            item_id: applied.item_id,
            action: Action::Retain { protection },
            reasons: vec![Reason::new(
                ReasonCode::Pinned,
                "protection set by explicit request",
                features! {},
            )],
            merged_into: None,
            evicted: vec![],
            audit_id: applied.audit_id,
        })
    }

    /// The `SubjectPurged` record is built **here**, not in the backend:
    /// `Backend::purge_subject` inserts the record it is handed and mints
    /// nothing (the echo rule on `Backend`), so whatever actor this method
    /// writes is what ends up in the log — which is now `actor`, the caller's
    /// own, rather than an anonymous placeholder.
    ///
    /// **Plumbing a real actor** (Plan 3's Task 2, "Engine — per-tenant
    /// policy and retention, actor-attributed governance events") is what
    /// this parameter is: every call site — the HTTP route
    /// (`ops::purge_subject`), the CLI command, and this crate's own tests —
    /// already has an `Actor` in scope to hand in. `forget` and `protect`
    /// above still write `Actor::system()`; that gap is engine-wide,
    /// pre-existing, and out of this task's scope, so it remains. An earlier
    /// draft of this method used `Actor { kind: ActorKind::Human, id: None }`
    /// as its placeholder, which the workspace's own idiom for "no
    /// attributed actor" (`Actor::system()`, used at dozens of sites across
    /// the backend crates) rightly avoided: a `Human` kind carrying no id
    /// affirmatively claims a person ordered the erasure while recording no
    /// identity for them — the sharper defect in a compliance record. That
    /// placeholder is gone now that a real actor is threaded through.
    ///
    /// The cascade is read from `self.retention_for(tenant)`, the tenant's
    /// configured `RetentionProfile` — not hard-coded, and not the engine's
    /// single global default, now that retention is per-tenant. `Preserve` is
    /// entirely the *backend's* behaviour, selected by this one argument: the
    /// engine reads no audit rows and replays none, for the reasons
    /// `Backend::purge_subject`'s own doc comment gives in full.
    pub async fn purge_subject(
        &self,
        tenant: &TenantId,
        subject: &SubjectId,
        actor: &Actor,
    ) -> Result<PurgeOutcome, EngineError> {
        // Computed once, up front, and reused for two purposes below: which
        // namespace the audit record is filed under (`purge_scope`), and
        // which namespaces' cached stats to invalidate afterwards. It must be
        // read here, before the purge — `Backend::purge_subject` erases
        // exactly the items this list is derived from (see `namespaces_of`),
        // so asking again afterwards would always answer "none".
        let namespaces = self.namespaces_of(tenant, subject).await?;
        let audit = AuditRecord::new(
            Self::purge_scope(tenant, subject, &namespaces),
            AuditEvent::SubjectPurged,
            vec![],
            actor.clone(),
            OffsetDateTime::now_utc(),
        );
        let cascade = self.retention_for(tenant).retention().purge_cascade;
        let report = self
            .backend
            .purge_subject(tenant, subject, cascade, audit)
            .await?;

        // Any write invalidates its scope — but a subject spans namespaces
        // while `invalidate_scope` takes one `Scope`. Every namespace the
        // subject owned (the list computed above, before the erasure) just
        // had its items removed, so each is invalidated individually; a
        // subject owning none (`namespaces` empty) invalidates nothing, which
        // is correct — no real `Scope` under this subject was ever populated
        // for a write to have cached stats for.
        for namespace in &namespaces {
            self.cache
                .invalidate_scope(&Scope {
                    tenant: tenant.clone(),
                    subject: subject.clone(),
                    namespace: namespace.clone(),
                })
                .await;
        }

        Ok(PurgeOutcome {
            items_removed: report.items_removed,
            audit_rows_removed: report.audit_rows_removed,
            audit_rows_preserved: report.audit_rows_preserved,
        })
    }

    /// Which namespace the `SubjectPurged` record is filed under. A subject
    /// spans namespaces and an `AuditRecord` carries exactly one `Scope`, so
    /// somebody has to choose; `Backend::purge_subject` states that it stores
    /// the choice as given and never rewrites it, which is why the choice is
    /// made here and made explicitly. The subject's lexicographically first
    /// namespace, so the record lands beside the rows it is about — and
    /// `memorysafe_core::PURGED_COMPONENT` (`_purged`) when the subject owns
    /// no items at all (`Namespace` forbids a leading dot but permits a
    /// leading underscore).
    ///
    /// **The fallback name is a plan-level choice, not a derived one**; a
    /// Task 35 executor may pick differently, but must pick, and must say so
    /// where the record is built.
    ///
    /// Takes `namespaces` already computed by the caller (ascending, per
    /// `namespaces_of`) rather than deriving them itself, so `purge_subject`
    /// can reuse the same list to invalidate every namespace's cache after
    /// the purge — the two purposes must see the same list, computed once,
    /// before the erasure that would otherwise make it unanswerable.
    fn purge_scope(
        tenant: &TenantId,
        subject: &SubjectId,
        namespaces: &[memorysafe_core::Namespace],
    ) -> Scope {
        let namespace = namespaces.first().cloned().unwrap_or_else(|| {
            // The constant, not a second copy of the literal: the name is
            // stored in audit rows that outlive the subject, so two
            // spellings is one silent divergence away from a compliance
            // query that finds nothing. See `PURGED_COMPONENT`'s doc for
            // what reserves it and what does not.
            memorysafe_core::Namespace::new(memorysafe_core::PURGED_COMPONENT)
                .expect("the reserved component is a valid namespace")
        });
        Scope {
            tenant: tenant.clone(),
            subject: subject.clone(),
            namespace,
        }
    }

    /// Namespaces the subject owns, ascending. Derived from its items, which
    /// is enough for v1: a namespace with no items has nothing for a purge to
    /// be about.
    ///
    /// **The `export` call below is a placeholder.** It pulls every item the
    /// subject owns, bodies and all, into memory in order to learn a list of
    /// namespace names — during an erasure, which is the one path where
    /// holding a subject's corpus in memory is least defensible, and whose
    /// cost grows with the corpus rather than with the answer. The correct
    /// shape is a namespace query on `Backend` that answers from an index;
    /// adding one is a trait change and belongs with the next batch of
    /// contract work, not here.
    async fn namespaces_of(
        &self,
        tenant: &TenantId,
        subject: &SubjectId,
    ) -> Result<Vec<memorysafe_core::Namespace>, EngineError> {
        let selector = memorysafe_backend::ScopeSelector {
            tenant: tenant.clone(),
            subject: Some(subject.clone()),
            namespace: None,
            include_audit: false,
        };
        let mut namespaces: Vec<memorysafe_core::Namespace> = self
            .backend
            .export(&selector)
            .await?
            .into_iter()
            .filter_map(|r| match r {
                memorysafe_backend::ExportRecord::Item { item, .. } => Some(item.scope.namespace),
                _ => None,
            })
            .collect();
        namespaces.sort();
        namespaces.dedup();
        Ok(namespaces)
    }
}

// Task 37's cache-invalidation ruling: the brief's literal wiring only
// invalidates `remember`'s own scope (`write.rs`), but `forget`, `protect`
// and `purge_subject` all write too, and each would otherwise leave stale
// `ScopeStats` behind for the policy to read on the next write to that scope.
// These are unit tests, not `tests/mutate.rs` integration tests, because they
// need to peek at `Engine`'s private `cache` field directly (`pub(crate)`,
// visible anywhere in this crate) rather than infer invalidation indirectly.
#[cfg(test)]
mod cache_invalidation_tests {
    use super::*;
    use crate::EngineConfig;
    use crate::write::RememberRequest;
    use memorysafe_backend_sqlite::SqliteBackend;
    use memorysafe_core::{Namespace, PURGED_COMPONENT, ScopeStats};
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

    // Deliberately not built from the backend's own stats: a sentinel makes
    // it unmistakable that what disappears is this test's planted cache
    // entry, not a coincidentally-identical value the backend recomputed.
    fn sentinel() -> ScopeStats {
        ScopeStats {
            item_count: 999_999,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn forget_invalidates_the_scopes_cached_stats() {
        let e = engine();
        let id = e
            .remember(RememberRequest::new(scope(), "a memory to delete"))
            .await
            .unwrap()
            .item_id
            .unwrap();
        // `remember` invalidates on admission (see `write.rs`); re-plant a
        // fresh sentinel afterwards so this test observes `forget`'s own
        // invalidation, not a leftover from the seeding write above.
        e.cache.put_stats(&scope(), sentinel()).await;
        assert!(
            e.cache.stats(&scope()).await.is_some(),
            "premise: cache seeded"
        );

        e.forget(&scope(), ForgetSelector::Ids(vec![id]))
            .await
            .unwrap();
        assert!(
            e.cache.stats(&scope()).await.is_none(),
            "forget must invalidate its scope's cached stats"
        );
    }

    #[tokio::test]
    async fn protect_invalidates_the_scopes_cached_stats() {
        let e = engine();
        let id = e
            .remember(RememberRequest::new(scope(), "worth protecting"))
            .await
            .unwrap()
            .item_id
            .unwrap();
        e.cache.put_stats(&scope(), sentinel()).await;
        assert!(
            e.cache.stats(&scope()).await.is_some(),
            "premise: cache seeded"
        );

        e.protect(&scope(), &id, Protection::Pinned).await.unwrap();
        assert!(
            e.cache.stats(&scope()).await.is_none(),
            "protect must invalidate its scope's cached stats"
        );
    }

    /// A subject can span several namespaces, each its own `Scope` and its
    /// own cache entry; `purge_subject` erases every namespace the subject
    /// owns, so it must invalidate every one of them — not just the single
    /// namespace its audit record happens to be filed under (see
    /// `purge_scope`) — and must leave an unrelated subject's cache alone.
    #[tokio::test]
    async fn purge_subject_invalidates_every_namespace_it_owned_and_nothing_else() {
        let e = engine();
        let tenant = TenantId::new("acme").unwrap();
        let subject = SubjectId::new("multi-ns-purge").unwrap();
        let ns_a = Scope {
            tenant: tenant.clone(),
            subject: subject.clone(),
            namespace: Namespace::new("aaa-namespace").unwrap(),
        };
        let ns_z = Scope {
            tenant: tenant.clone(),
            subject: subject.clone(),
            namespace: Namespace::new("zzz-namespace").unwrap(),
        };
        let unrelated = Scope::new("acme", "someone-else", "agent").unwrap();

        e.remember(RememberRequest::new(ns_a.clone(), "in namespace a"))
            .await
            .unwrap();
        e.remember(RememberRequest::new(ns_z.clone(), "in namespace z"))
            .await
            .unwrap();
        e.remember(RememberRequest::new(
            unrelated.clone(),
            "a different subject entirely",
        ))
        .await
        .unwrap();

        e.cache.put_stats(&ns_a, sentinel()).await;
        e.cache.put_stats(&ns_z, sentinel()).await;
        e.cache.put_stats(&unrelated, sentinel()).await;

        let report = e
            .purge_subject(&tenant, &subject, &Actor::system())
            .await
            .unwrap();
        assert_eq!(report.items_removed, 2, "premise: both namespaces purged");

        assert!(
            e.cache.stats(&ns_a).await.is_none(),
            "purge_subject must invalidate every namespace it owned (a)"
        );
        assert!(
            e.cache.stats(&ns_z).await.is_none(),
            "purge_subject must invalidate every namespace it owned (z)"
        );
        assert!(
            e.cache.stats(&unrelated).await.is_some(),
            "purge_subject must not invalidate an unrelated subject's cache"
        );
    }

    /// Negative control for the fallback path: a subject that owns no items
    /// has no real namespace to invalidate (`purge_scope` files the record
    /// under `PURGED_COMPONENT` instead), so there is nothing for this call
    /// to touch. Guards against a careless rewrite that invalidates the
    /// fallback scope itself — which no write ever populated, so nothing
    /// under it should ever be cached, let alone invalidated. An earlier
    /// version of this test only cached `unrelated` (a different subject
    /// entirely), which the invalidation loop cannot reach no matter what it
    /// does — every `Scope` it builds comes from `tenant`/`subject`, so a
    /// mutation that invalidated the fallback scope itself would still have
    /// passed. Seeding the fallback scope directly closes that: this is the
    /// one `Scope` a careless rewrite could plausibly reach.
    #[tokio::test]
    async fn purging_an_empty_subject_invalidates_nothing() {
        let e = engine();
        let tenant = TenantId::new("acme").unwrap();
        let subject = SubjectId::new("ghost-purge").unwrap();
        let fallback = Scope {
            tenant: tenant.clone(),
            subject: subject.clone(),
            namespace: Namespace::new(PURGED_COMPONENT).unwrap(),
        };
        let unrelated = Scope::new("acme", "someone-else", "agent").unwrap();
        e.cache.put_stats(&fallback, sentinel()).await;
        e.cache.put_stats(&unrelated, sentinel()).await;

        e.purge_subject(&tenant, &subject, &Actor::system())
            .await
            .unwrap();

        assert!(
            e.cache.stats(&fallback).await.is_some(),
            "purging an empty subject must not invalidate its own fallback scope"
        );
        assert!(
            e.cache.stats(&unrelated).await.is_some(),
            "purging an empty subject must not touch an unrelated scope's cache"
        );
    }
}
