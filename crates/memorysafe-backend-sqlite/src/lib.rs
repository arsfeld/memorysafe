//! SQLite backend: one database file per tenant.
//!
//! Isolation is structural rather than a query-layer invariant — backup is
//! `cp`, tenant deletion is `rm`, and per-tenant encryption is a key per file.
//!
//! **`rm` has a precondition.** A pooled connection keeps writing to an
//! unlinked inode: the writes land nowhere visible and reads return rows that
//! were deleted. Call [`SqliteBackend::forget_tenant`] first, let every
//! in-flight call for that tenant return, and only then remove `<tenant>.db`
//! together with its `-wal` and `-shm` sidecars.

pub mod aggregates;
pub mod audit;
pub mod items;
pub mod schema;
pub mod tenant;

use async_trait::async_trait;
use memorysafe_backend::{
    AppliedWrite, AuditAggregate, AuditAggregateFilter, Backend, BackendError, CandidateQuery,
    ExportStream, ImportReport, ImportStream, Page, PurgeReport, ScopeSelector, WriteTransaction,
};
use memorysafe_core::{
    AuditFilter, AuditId, AuditRecord, Budget, CapacityState, Embedding, ItemId, MemoryItem,
    PurgeCascade, Scope, ScopeStats, ScoredCandidate, SubjectId, TenantId,
};
use rusqlite::params;
use std::path::PathBuf;
use tenant::{SqlResultExt, TenantManager};

pub struct SqliteBackend {
    pub(crate) tenants: TenantManager,
}

impl SqliteBackend {
    pub fn open(root: PathBuf) -> Self {
        Self::with_max_open(root, 64)
    }

    pub fn with_max_open(root: PathBuf, max_open: usize) -> Self {
        Self {
            tenants: TenantManager::new(root, max_open),
        }
    }

    /// Drops this backend's pooled connection for `tenant`. The operator
    /// half of tenant deletion — see the module doc, and
    /// [`TenantManager::forget`] for what it does and does not close.
    pub fn forget_tenant(&self, tenant: &TenantId) {
        self.tenants.forget(tenant);
    }
}

/// **Partial, and deliberately so.** Task 20 implements `get`, `list`,
/// `audit`, `record_recall` and a first `apply` covering insert, evictions and
/// the audit row. Vectors and `neighbours` arrive in Task 21, keyword and
/// hybrid retrieval in Task 22, merge and capacity and idempotency in
/// Task 23, and purge/export/import plus the aggregate *read* in Task 24.
/// The methods those tasks own return `Ok` defaults here so the crate
/// compiles and the isolation and atomicity conformance tests can run at
/// all; each is marked, and none is bound by a conformance test in this
/// crate's `tests/conformance.rs` yet.
#[async_trait]
impl Backend for SqliteBackend {
    async fn get(&self, scope: &Scope, id: &ItemId) -> Result<Option<MemoryItem>, BackendError> {
        let (scope, id) = (scope.clone(), id.clone());
        self.tenants
            .with_conn(&scope.tenant.clone(), move |c| items::get(c, &scope, &id))
            .await
    }

    async fn list(&self, scope: &Scope, page: &Page) -> Result<Vec<MemoryItem>, BackendError> {
        let (scope, page) = (scope.clone(), *page);
        self.tenants
            .with_conn(&scope.tenant.clone(), move |c| {
                items::list(c, &scope, &page)
            })
            .await
    }

    async fn audit(
        &self,
        scope: &Scope,
        filter: &AuditFilter,
    ) -> Result<Vec<AuditRecord>, BackendError> {
        let (scope, filter) = (scope.clone(), filter.clone());
        self.tenants
            .with_conn(&scope.tenant.clone(), move |c| {
                audit::query(c, &scope, &filter)
            })
            .await
    }

    async fn record_recall(&self, record: AuditRecord) -> Result<AuditId, BackendError> {
        let tenant = record.scope.tenant.clone();
        self.tenants
            .with_write(&tenant, move |conn| {
                let tx = conn
                    .transaction()
                    .map_err(|e| tenant::storage_error(e, false))?;
                let id = audit::insert(&tx, &record)?;
                // A `Recalled` row is an audit row: it increments, like every
                // other. See the write-path rule on `crate::aggregates`.
                aggregates::increment(&tx, &record)?;
                // And the access statistics ride this same write rather than a
                // second one — `Backend::record_recall` says why: the record
                // already carries exactly the ids that were recalled, and two
                // writes would let a backend audit a recall without counting
                // it. `record.at`, never a clock read, so a replay produces
                // comparable rows.
                for item in &record.items {
                    tx.execute(
                        "UPDATE items
                            SET access_count = access_count + 1, last_access = ?1
                          WHERE id = ?2 AND subject = ?3 AND namespace = ?4",
                        params![
                            record.at.unix_timestamp(),
                            item.id().as_str(),
                            record.scope.subject.as_str(),
                            record.scope.namespace.as_str(),
                        ],
                    )
                    .sql()?;
                }
                tx.commit().map_err(|e| tenant::storage_error(e, false))?;
                Ok(id)
            })
            .await
    }

    async fn apply(&self, txn: WriteTransaction) -> Result<AppliedWrite, BackendError> {
        // Validation first, and before any connection is taken: `Backend::apply`
        // requires an invalid transaction to be refused **having written
        // nothing**, and rejecting after writing half of it is the same defect
        // with an error attached.
        if !txn.is_valid() {
            return Err(BackendError::InvalidTransaction(
                "a transaction may not both insert and merge, and its scope-bearing \
                 fields must agree"
                    .into(),
            ));
        }
        // `is_valid()` accepts a merge-only transaction, and this task does not
        // implement merges — so without this guard `apply` performs the
        // evictions, writes the audit row, commits, and returns `Ok` for a
        // request whose merge half it silently discarded. A stub returning a
        // default is recoverable; reporting success for work not done is not.
        // The merge task removes this and implements the branch.
        if txn.merge.is_some() {
            return Err(BackendError::Storage {
                message: "merge is not implemented in this build; refusing rather than \
                          applying the rest of the transaction and reporting success"
                    .into(),
                retryable: false,
            });
        }
        let tenant = txn.scope.tenant.clone();
        self.tenants
            .with_write(&tenant, move |conn| {
                let tx = conn
                    .transaction()
                    .map_err(|e| tenant::storage_error(e, false))?;

                let mut evicted = Vec::new();
                for id in &txn.evictions {
                    items::delete(&tx, &txn.scope, id)?;
                    evicted.push(id.clone());
                }

                let mut item_id = None;
                if let Some(w) = &txn.upsert {
                    items::insert(&tx, &w.item)?;
                    item_id = Some(w.item.id.clone());
                }

                let audit_id = audit::insert(&tx, &txn.audit)?;
                // Same transaction as the audit row, never a follow-up write:
                // an aggregate that can diverge from the detail rows it
                // summarises is worse than no aggregate.
                aggregates::increment(&tx, &txn.audit)?;
                tx.commit().map_err(|e| tenant::storage_error(e, false))?;

                Ok(AppliedWrite {
                    item_id,
                    audit_id,
                    evicted,
                    replayed: false,
                    replayed_outcome: None,
                })
            })
            .await
    }

    // Implemented in Tasks 21-24.
    async fn retrieve_candidates(
        &self,
        _s: &Scope,
        _q: &CandidateQuery,
    ) -> Result<Vec<ScoredCandidate>, BackendError> {
        Ok(vec![])
    }
    async fn neighbours(
        &self,
        _s: &Scope,
        _e: &Embedding,
        _k: usize,
    ) -> Result<Vec<ScoredCandidate>, BackendError> {
        Ok(vec![])
    }
    async fn capacity_state(&self, _s: &Scope) -> Result<CapacityState, BackendError> {
        Ok(CapacityState {
            budget: Budget::UNBOUNDED,
            used_items: 0,
            used_bytes: 0,
        })
    }
    async fn scope_stats(&self, _s: &Scope) -> Result<ScopeStats, BackendError> {
        Ok(ScopeStats::default())
    }
    async fn set_budget(&self, _s: &Scope, _b: Budget) -> Result<(), BackendError> {
        Ok(())
    }
    async fn purge_subject(
        &self,
        _t: &TenantId,
        _s: &SubjectId,
        _c: PurgeCascade,
        _a: AuditRecord,
    ) -> Result<PurgeReport, BackendError> {
        Ok(PurgeReport {
            items_removed: 0,
            vectors_removed: 0,
            audit_rows_removed: 0,
            audit_rows_preserved: 0,
        })
    }
    async fn export(&self, _s: &ScopeSelector) -> Result<ExportStream, BackendError> {
        Ok(vec![])
    }
    async fn import(&self, _t: &TenantId, _s: ImportStream) -> Result<ImportReport, BackendError> {
        Ok(ImportReport::default())
    }
    // Stubbed here and replaced in the portability task, alongside
    // `purge_subject`, `export` and `import`. Three conformance tests depend on
    // the real read — `audit_aggregates_survive_a_cascading_purge`,
    // `audit_aggregates_page_in_the_documented_order` and
    // `audit_aggregates_narrow_by_day_window_and_policy` — and this stub
    // satisfies none of them.
    async fn audit_aggregates(
        &self,
        _t: &TenantId,
        _f: &AuditAggregateFilter,
    ) -> Result<Vec<AuditAggregate>, BackendError> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_backend::conformance::fx;
    use memorysafe_core::{
        Actor, ActorKind, AuditEvent, AuditRecord, ItemRef, PolicyId, Reason, ReasonCode,
    };
    use time::{Duration, OffsetDateTime};

    fn backend() -> SqliteBackend {
        let dir = tempfile::tempdir().expect("tempdir");
        SqliteBackend::open(dir.keep())
    }

    fn scope(subject: &str, namespace: &str) -> Scope {
        Scope::new("t", subject, namespace).unwrap()
    }

    /// Every aggregate row held for a tenant, as
    /// `(policy_name, policy_version, event, day, count)`. Read with raw SQL
    /// because `Backend::audit_aggregates` is stubbed until the portability
    /// task — which is precisely why these tests exist.
    async fn aggregate_rows(
        b: &SqliteBackend,
        tenant: &TenantId,
    ) -> Vec<(Option<String>, Option<String>, String, i64, i64)> {
        b.tenants
            .with_conn(tenant, |c| {
                let mut stmt = c
                    .prepare(
                        "SELECT policy_name, policy_version, event, day, count
                         FROM audit_aggregates ORDER BY day, event",
                    )
                    .sql()?;
                let rows = stmt
                    .query_map([], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    })
                    .sql()?;
                rows.collect::<rusqlite::Result<Vec<_>>>().sql()
            })
            .await
            .unwrap()
    }

    async fn access_stats(b: &SqliteBackend, scope: &Scope, id: &ItemId) -> (i64, Option<i64>) {
        let (subject, namespace, id) = (
            scope.subject.as_str().to_string(),
            scope.namespace.as_str().to_string(),
            id.as_str().to_string(),
        );
        b.tenants
            .with_conn(&scope.tenant.clone(), move |c| {
                c.query_row(
                    "SELECT access_count, last_access FROM items
                     WHERE id=?1 AND subject=?2 AND namespace=?3",
                    params![id, subject, namespace],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .sql()
            })
            .await
            .unwrap()
    }

    /// **The aggregate write rule, on `apply`, and this test is the only thing
    /// that can see it.**
    ///
    /// No conformance test bound in this crate observes `audit_aggregates`:
    /// the read half is stubbed until the portability task, so
    /// `aggregates::increment` can be deleted from `apply` and all seven
    /// conformance tests still pass — measured, not assumed. That is the
    /// `foreign_keys` shape: a real property with a present mechanism and no
    /// test that can fail.
    ///
    /// **It establishes the absence before asserting the presence.** The table
    /// is asserted empty first, so "there is an aggregate row" cannot be
    /// satisfied by a row that was already there — the mistake the pragma case
    /// in `schema.rs` records.
    /// `WriteTransaction::is_valid` accepts a merge-only transaction, and this
    /// build implements no merge. The dangerous outcome is not that the merge
    /// fails — it is that the *rest* of the transaction succeeds and `apply`
    /// returns `Ok`, so the caller records a merge that never happened while
    /// the evictions it was bundled with are permanent.
    #[tokio::test]
    async fn a_merge_is_refused_whole_rather_than_applied_in_part() {
        let b = backend();
        let s = scope("s", "n");

        let victim = fx::item(&s, "collateral");
        b.apply(fx::admit_txn(&s, victim.clone(), None))
            .await
            .unwrap();
        let before = aggregate_rows(&b, &s.tenant).await;

        // Evictions *and* a merge: without the guard the evictions commit, the
        // audit row is written, and the merge is silently dropped.
        let mut txn = fx::evict_txn(&s, vec![victim.id.clone()]);
        txn.merge = Some(memorysafe_backend::write::MergeWrite {
            target: victim.id.clone(),
            body: "merged text".into(),
            tags: vec![],
            attrs: Default::default(),
            vector: None,
            byte_size: 11,
        });
        assert!(
            txn.is_valid(),
            "the premise: this transaction is well-formed"
        );

        let err = b.apply(txn).await.unwrap_err();
        assert!(
            matches!(
                err,
                BackendError::Storage {
                    retryable: false,
                    ..
                }
            ),
            "expected a non-retryable refusal, got {err:?}"
        );
        assert!(
            b.get(&s, &victim.id).await.unwrap().is_some(),
            "the refusal wrote nothing, so the eviction bundled with the merge \
             must not have taken effect"
        );
        assert_eq!(
            aggregate_rows(&b, &s.tenant).await,
            before,
            "a refused transaction wrote an audit row and an aggregate"
        );
    }

    #[tokio::test]
    async fn apply_increments_the_aggregates_in_the_same_write() {
        let b = backend();
        let s = scope("s", "n");

        assert!(
            aggregate_rows(&b, &s.tenant).await.is_empty(),
            "the absence has to be established before the presence means anything"
        );

        let first = fx::item(&s, "the first memory");
        b.apply(fx::admit_txn(&s, first.clone(), None))
            .await
            .unwrap();
        assert_eq!(
            aggregate_rows(&b, &s.tenant).await,
            vec![(None, None, "admitted".to_string(), 0, 1)],
            "apply wrote an audit row without incrementing its aggregate"
        );

        // A second admit on the same day and event class is the *same* key, so
        // it must raise the count rather than create a second row.
        b.apply(fx::admit_txn(&s, fx::item(&s, "the second memory"), None))
            .await
            .unwrap();
        assert_eq!(
            aggregate_rows(&b, &s.tenant).await,
            vec![(None, None, "admitted".to_string(), 0, 2)]
        );

        // A different event class is a different key.
        b.apply(fx::evict_txn(&s, vec![first.id.clone()]))
            .await
            .unwrap();
        let mut rows = aggregate_rows(&b, &s.tenant).await;
        rows.sort();
        assert_eq!(
            rows,
            vec![
                (None, None, "admitted".to_string(), 0, 2),
                (None, None, "forgotten".to_string(), 0, 1),
            ]
        );
    }

    /// A policy-carrying decision keys its aggregate on the policy's **two
    /// parts**, taken from the record and not from the `audit` table's
    /// rendered `policy` column.
    ///
    /// Nothing in the conformance suite reaches this: `fx::admit_txn` builds
    /// records with `decision: None`, so every aggregate row the suite
    /// produces is policy-less and the policied half of the key space is
    /// unexercised by it entirely.
    #[tokio::test]
    async fn a_policied_decision_keys_its_aggregate_on_both_policy_parts() {
        let b = backend();
        let s = scope("s", "n");
        let mut txn = fx::admit_txn(&s, fx::item(&s, "a governed memory"), None);
        txn.audit.decision = Some(memorysafe_core::Decision::reject(
            PolicyId::new("baseline", "1.4"),
            Reason::new(ReasonCode::LowValue, "", Default::default()),
        ));
        b.apply(txn).await.unwrap();

        assert_eq!(
            aggregate_rows(&b, &s.tenant).await,
            vec![(
                Some("baseline".to_string()),
                Some("1.4".to_string()),
                "admitted".to_string(),
                0,
                1
            )],
            "the policy must be stored as two columns, from the decision"
        );
    }

    /// `record_recall`'s three effects, none of which any conformance test
    /// bound in this crate can observe yet: the audit row under the id it was
    /// given, the aggregate increment, and the access-statistics bump.
    ///
    /// **Only the referenced items are touched.** A bystander in the same
    /// scope is asserted untouched — a backend that bumped every item in the
    /// scope would make `last_accessed_at` mean "the scope was read", which is
    /// not what the replay quota needs to know, and an assertion over the
    /// recalled item alone could not tell the two apart.
    #[tokio::test]
    async fn record_recall_writes_audit_bumps_access_statistics_and_increments() {
        let b = backend();
        let s = scope("s", "n");
        let recalled = fx::item(&s, "the recalled memory");
        let bystander = fx::item(&s, "never recalled");
        for item in [recalled.clone(), bystander.clone()] {
            b.apply(fx::admit_txn(&s, item, None)).await.unwrap();
        }

        // The absence, established: nothing has been accessed, and the day the
        // recall lands on holds no aggregate row.
        assert_eq!(access_stats(&b, &s, &recalled.id).await, (0, None));
        assert!(
            !aggregate_rows(&b, &s.tenant)
                .await
                .iter()
                .any(|r| r.2 == "recalled"),
            "a recalled aggregate existed before any recall"
        );

        let at = OffsetDateTime::UNIX_EPOCH + Duration::days(3) + Duration::seconds(7);
        let mut record = AuditRecord::new(
            s.clone(),
            AuditEvent::Recalled,
            vec![ItemRef::from_item(&recalled)],
            Actor {
                kind: ActorKind::Agent,
                id: Some("assistant".into()),
            },
            at,
        );
        record.id = memorysafe_core::AuditId::parse("01ARZ3NDEKTSV4RRFFQ69G5FE0").unwrap();

        let returned = b.record_recall(record.clone()).await.unwrap();
        assert_eq!(
            returned, record.id,
            "record_recall must persist and return the id it was given"
        );

        let rows = b.audit(&s, &AuditFilter::default()).await.unwrap();
        let stored = rows
            .iter()
            .find(|r| r.event == AuditEvent::Recalled)
            .expect("the recall's audit row");
        assert_eq!(stored, &record, "the recall row did not survive intact");

        // `record.at`, never a clock read: a replay has to produce comparable
        // rows. Asserting the exact stored value is what separates the two.
        assert_eq!(
            access_stats(&b, &s, &recalled.id).await,
            (1, Some(at.unix_timestamp()))
        );
        assert_eq!(
            access_stats(&b, &s, &bystander.id).await,
            (0, None),
            "record_recall bumped an item the record does not reference"
        );

        assert!(
            aggregate_rows(&b, &s.tenant).await.contains(&(
                None,
                None,
                "recalled".to_string(),
                3,
                1
            )),
            "a Recalled row is an audit row: it increments like every other, \
             on the UTC day it happened"
        );
    }

    /// `apply` is one transaction: a failure part-way through leaves no trace
    /// of any of it — not the eviction it had already performed, not the audit
    /// row, and not the aggregate increment.
    ///
    /// **The failure is real, not injected:** the second transaction reuses the
    /// first's `AuditId`, which the `audit` table's primary key rejects. That
    /// happens *after* the eviction delete, so a backend committing its parts
    /// separately would have destroyed the item and then failed.
    ///
    /// `atomicity::a_failed_transaction_leaves_no_trace` covers this through
    /// the trait, but only via a merge target that does not exist — a path
    /// this task does not implement — so nothing else exercises the rollback.
    #[tokio::test]
    async fn a_failure_after_the_eviction_rolls_the_whole_apply_back() {
        let b = backend();
        let s = scope("s", "n");
        let survivor = fx::item(&s, "must survive a failed transaction");
        let first = fx::admit_txn(&s, survivor.clone(), None);
        let reused_id = first.audit.id.clone();
        b.apply(first).await.unwrap();

        let before = aggregate_rows(&b, &s.tenant).await;
        assert_eq!(before, vec![(None, None, "admitted".to_string(), 0, 1)]);

        let mut doomed = fx::admit_txn(&s, fx::item(&s, "must never be written"), None);
        doomed.audit.id = reused_id;
        doomed.evictions = vec![survivor.id.clone()];
        let newcomer = doomed.upsert.as_ref().unwrap().item.id.clone();

        let err = b.apply(doomed).await.unwrap_err();
        assert!(
            matches!(err, BackendError::Storage { .. }),
            "expected a storage failure, got {err:?}"
        );

        assert!(
            b.get(&s, &survivor.id).await.unwrap().is_some(),
            "a failed transaction still performed its eviction"
        );
        assert!(
            b.get(&s, &newcomer).await.unwrap().is_none(),
            "a failed transaction still inserted its item"
        );
        assert_eq!(
            b.audit(&s, &AuditFilter::default()).await.unwrap().len(),
            1,
            "a failed transaction still wrote an audit row"
        );
        assert_eq!(
            aggregate_rows(&b, &s.tenant).await,
            before,
            "a failed transaction still incremented the aggregates — the one \
             artifact designed to outlive the detail rows, and the one whose \
             drift cannot be recomputed"
        );
    }

    /// Two tenants are two files, and the aggregate table is per-file. Nothing
    /// in the suite checks that the *aggregates* are tenant-scoped, because
    /// nothing in the suite can read them here.
    #[tokio::test]
    async fn aggregates_do_not_cross_a_tenant_boundary() {
        let b = backend();
        let a = Scope::new("tenant-a", "s", "n").unwrap();
        let z = Scope::new("tenant-z", "s", "n").unwrap();
        b.apply(fx::admit_txn(&a, fx::item(&a, "a's memory"), None))
            .await
            .unwrap();

        assert_eq!(aggregate_rows(&b, &a.tenant).await.len(), 1);
        assert!(
            aggregate_rows(&b, &z.tenant).await.is_empty(),
            "tenant z holds an aggregate row for a write it never made"
        );
    }
}
