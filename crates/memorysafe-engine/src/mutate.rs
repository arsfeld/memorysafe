use crate::Engine;
use crate::error::EngineError;
use crate::outcome::{ForgetOutcome, PurgeOutcome, WriteOutcome};
use memorysafe_backend::{ItemWrite, Page, WriteTransaction};
use memorysafe_core::{
    Action, Actor, AuditEvent, AuditRecord, ItemId, ItemRef, Protection, PurgeCascade, Reason,
    ReasonCode, Scope, SubjectId, TenantId, features,
};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgetSelector {
    Ids(Vec<ItemId>),
    Tag(String),
    Kind(String),
}

/// Paging bound for selector-based forget. Beyond this a caller should purge
/// the subject or narrow the selector.
const FORGET_SCAN_LIMIT: usize = 1000;

impl Engine {
    /// The existence pre-check on `ForgetSelector::Ids` below
    /// (`self.backend.get(scope, &id).await?.is_some()`) looks like
    /// defence-in-depth against an already-safe design — `ForgetOutcome` is
    /// identical with or without it, since it is built from
    /// `AppliedWrite::evicted` (what the backend actually removed, itself
    /// scope-filtered), never from the raw selector. **It is not
    /// defence-in-depth; it is load-bearing.** Two lines above the guard
    /// that builds `evicted`, `Backend::apply`'s eviction loop also calls
    /// `vectors::delete(&tx, id)` — unconditionally and unscoped, no subject
    /// or namespace predicate. Without this pre-check, naming an id from a
    /// different scope in the same tenant leaves that item's row untouched
    /// but permanently strips its vector, silently: `ForgetOutcome` reports
    /// nothing forgotten, and the audit trail shows nothing happened. See
    /// `forgetting_an_id_from_another_scope_does_not_delete_its_vector`.
    pub async fn forget(
        &self,
        scope: &Scope,
        selector: ForgetSelector,
    ) -> Result<ForgetOutcome, EngineError> {
        let targets: Vec<ItemId> = match selector {
            ForgetSelector::Ids(ids) => {
                let mut present = Vec::new();
                for id in ids {
                    if self.backend.get(scope, &id).await?.is_some() {
                        present.push(id);
                    }
                }
                present
            }
            ForgetSelector::Tag(tag) => self
                .backend
                .list(
                    scope,
                    &Page {
                        offset: 0,
                        limit: FORGET_SCAN_LIMIT,
                    },
                )
                .await?
                .into_iter()
                .filter(|i| i.tags.contains(&tag))
                .map(|i| i.id)
                .collect(),
            ForgetSelector::Kind(kind) => self
                .backend
                .list(
                    scope,
                    &Page {
                        offset: 0,
                        limit: FORGET_SCAN_LIMIT,
                    },
                )
                .await?
                .into_iter()
                .filter(|i| i.kind == kind)
                .map(|i| i.id)
                .collect(),
        };

        let refs: Vec<ItemRef> = Vec::new();
        // `Actor::system()`, not a caller-identified human: no engine method
        // below the boundary has one to attribute yet. See `purge_subject`'s
        // doc comment below for the full account of this gap and where it
        // closes (Plan 3 Task 2).
        let audit = AuditRecord::new(
            scope.clone(),
            AuditEvent::Forgotten,
            refs,
            Actor::system(),
            OffsetDateTime::now_utc(),
        );
        let mut txn = WriteTransaction::new(scope.clone(), audit);
        txn.evictions = targets.clone();

        let applied = self.backend.apply(txn).await?;
        Ok(ForgetOutcome {
            forgotten: applied.evicted,
            audit_id: applied.audit_id,
        })
    }

    /// The only path that changes `protection` outside admission.
    ///
    /// Its own audit record's actor is `Actor::system()`, for the same
    /// engine-wide, deferred reason `purge_subject`'s doc comment below gives
    /// in full.
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
        // the item must be re-embedded before it is written back.
        let vector = self
            .embedder
            .embed(&item.body)
            .ok()
            .map(|e| memorysafe_embed::QuantizedVector::from_embedding(&e));

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
        // Its cost: the `items` table also carries `value_score`,
        // `fragility_score`, `last_access` and `access_count`, none of which
        // live on `MemoryItem`. `items::insert` writes `MemoryItem`'s own
        // fields only, so those four columns cannot be carried forward and
        // silently revert to schema defaults on every `protect` call — an
        // item's entire accumulated recall history is reset the moment it is
        // pinned or protected. Not fixed here; see
        // `protecting_an_item_resets_its_accumulated_access_history` for what
        // this does today, and this task's report for whether preserving it
        // is the right long-term answer.
        txn.evictions = vec![id.clone()];
        txn.upsert = Some(ItemWrite { item, vector });

        let applied = self.backend.apply(txn).await?;
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
    /// writes is what ends up in the log — which today is `Actor::system()`,
    /// not the actor who ordered the erasure.
    ///
    /// `Actor::system()`, not the `ActorKind::Human` literal an earlier draft
    /// used: the workspace's own idiom for "no attributed actor" (used at
    /// dozens of sites across the backend crates, and what the conformance
    /// suite's own fixtures explicitly contrast a *real, identified* human
    /// actor against) is an honest absence, where a `Human` kind carrying no
    /// id affirmatively claims a person ordered the erasure while recording
    /// no identity for them — the sharper defect in a compliance record.
    ///
    /// **Plumbing a real actor is deferred to Plan 3's Task 2** ("Engine —
    /// per-tenant policy and retention, actor-attributed governance events"),
    /// which is where engine methods start taking an `Actor` from the
    /// boundary that has one. `Engine::purge_subject` has no actor parameter
    /// to thread, and adding one here would change a signature that Plan 3's
    /// HTTP route (`ops::purge_subject`), CLI command (`purge-subject`) and
    /// engine tests all already call — so the gap is recorded rather than
    /// closed. The same placeholder appears in `forget` and `protect` above;
    /// it is engine-wide and pre-existing, not specific to this method.
    /// `RememberRequest::actor` is already threaded through `remember`'s own
    /// audit record, so `remember` is actor-attributed today and these three
    /// are not — an inconsistency in the trail, not a symmetric gap.
    ///
    /// `PurgeCascade::Cascade` is hard-coded here — it is `balanced`, the
    /// default profile's behaviour. Task 38 replaces this one expression with
    /// `self.retention.retention().purge_cascade` and changes nothing else.
    pub async fn purge_subject(
        &self,
        tenant: &TenantId,
        subject: &SubjectId,
    ) -> Result<PurgeOutcome, EngineError> {
        let audit = AuditRecord::new(
            self.purge_scope(tenant, subject).await?,
            AuditEvent::SubjectPurged,
            vec![],
            Actor::system(),
            OffsetDateTime::now_utc(),
        );
        let report = self
            .backend
            .purge_subject(tenant, subject, PurgeCascade::Cascade, audit)
            .await?;
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
    async fn purge_scope(
        &self,
        tenant: &TenantId,
        subject: &SubjectId,
    ) -> Result<Scope, EngineError> {
        // PLACEHOLDER, not the intended shape. `namespaces_of` learns one
        // namespace by materialising the subject's entire corpus — item
        // bodies included — into a `Vec`, in the erasure path. It stands in
        // for a dedicated namespace query on `Backend` (something of the
        // shape `namespaces(&self, tenant, subject) -> Vec<Namespace>`),
        // which does not exist and is out of scope for this task. Do not ship
        // this as the design; see `namespaces_of` below.
        let namespace = self
            .namespaces_of(tenant, subject)
            .await?
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                // The constant, not a second copy of the literal: the name is
                // stored in audit rows that outlive the subject, so two
                // spellings is one silent divergence away from a compliance
                // query that finds nothing. See `PURGED_COMPONENT`'s doc for
                // what reserves it and what does not.
                memorysafe_core::Namespace::new(memorysafe_core::PURGED_COMPONENT)
                    .expect("the reserved component is a valid namespace")
            });
        Ok(Scope {
            tenant: tenant.clone(),
            subject: subject.clone(),
            namespace,
        })
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
