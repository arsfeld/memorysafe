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
pub mod capacity;
pub mod items;
pub mod keyword;
pub mod portability;
pub mod purge;
pub mod retrieve;
pub mod schema;
pub mod tenant;
pub mod vectors;

use async_trait::async_trait;
use memorysafe_backend::{
    AppliedWrite, AuditAggregate, AuditAggregateFilter, Backend, BackendError, CandidateQuery,
    ExportStream, HardFilters, ImportReport, ImportStream, Page, PurgeReport, ScopeSelector,
    WriteTransaction,
};
use memorysafe_core::{
    AuditFilter, AuditId, AuditRecord, Budget, CapacityState, Embedding, ItemId, MemoryItem,
    PurgeCascade, Scope, ScopeStats, ScoredCandidate, SensitivityLevel, SubjectId, TenantId,
};
use rusqlite::{OptionalExtension, params};
use std::path::PathBuf;
use tenant::{SqlResultExt, TenantManager};

/// Rough token count for budget packing: ~4 bytes per token, the usual
/// English approximation. Deliberately cheap — the budget is a guide, not a
/// contract with a specific tokenizer.
pub(crate) fn estimate_tokens(body: &str) -> u32 {
    ((body.len() as f32 / 4.0).ceil() as u32).max(1)
}

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

    /// The number of `vectors` rows stored for `scope`.
    ///
    /// **Why this exists: neither failure shape a `vectors`/`items`
    /// mismatch can take is visible any other way.** There are two, and
    /// they are opposite:
    ///
    /// - A **widowed item** — an `items` row that survives while its
    ///   `vectors` row is gone. This is what D1 actually produced: a live
    ///   item, fully readable through `get`/`list`, permanently invisible to
    ///   vector search. `vector_row_count` dropping for a scope that made no
    ///   eviction of its own is what makes this observable, since nothing
    ///   else in the crate counts `vectors` at all — `capacity` never does,
    ///   and `vectors::search` (so `neighbours`) joins `vectors` to `items`
    ///   on `item_id`, which cannot show a row that used to exist there.
    /// - An **orphaned vector** — a `vectors` row with no matching `items`
    ///   row. `ON DELETE CASCADE` with `foreign_keys=ON` forbids this from
    ///   existing at all in the schema as written, so it is reachable only
    ///   if that enforcement itself fails (a future schema without the
    ///   cascade, or a connection with the pragma off) — not by D1. When it
    ///   does exist, `vector_row_count` staying nonzero after every `items`
    ///   row in that scope is gone is what shows it; `export` round-trips
    ///   through the same `items` join `search` does, so it would silently
    ///   *drop* an orphan on export/import rather than failing on it.
    ///
    /// `PurgeReport::vectors_removed` is this codebase's only other
    /// vector-count surface, and it is purge-specific, not a general
    /// observer. This is called directly by D1's eviction tests
    /// (`tests::eviction_out_of_scope_does_not_widow_another_scopes_item`,
    /// `tests::eviction_in_scope_still_deletes_the_vector_row`), which is
    /// the entire reason it exists — see them for the property it makes
    /// assertable without reaching into raw SQL from a test.
    ///
    /// It has few or no other callers today. That is deliberate, not an
    /// oversight: documented here so it doesn't read, later, as the kind of
    /// unexplained public accessor `known-gaps.md` already flags for
    /// `forget_tenant`. A later referential-integrity invariant (no vector
    /// row without a live item row in the same scope) is expected to build
    /// on it — this fix does not implement that invariant, only the
    /// observer it will need.
    ///
    /// On the concrete type deliberately, **not** on `Backend`: a trait
    /// method binds every backend to a contract, which is a decision for
    /// whoever owns that trait, not a side effect of one crate's defect fix.
    pub async fn vector_row_count(&self, scope: &Scope) -> Result<u64, BackendError> {
        let scope = scope.clone();
        self.tenants
            .with_conn(&scope.tenant.clone(), move |c| vectors::count(c, &scope))
            .await
    }
}

/// Every method real. Task 20 implements `get`, `list`, `audit`,
/// `record_recall` and a first `apply` covering insert, evictions and the
/// audit row. Task 21 adds vectors and a real `neighbours`. Task 22 adds
/// keyword search and a real `retrieve_candidates`. Task 23 adds merge,
/// capacity accounting and idempotent writes to `apply`, plus real
/// `capacity_state`, `scope_stats` and `set_budget`. Task 24 closes the last
/// four: `purge_subject`, `export`, `import` and the `audit_aggregates` read.
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
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
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
        let tenant = txn.scope.tenant.clone();
        self.tenants
            .with_write(&tenant, move |conn| {
                // Idempotency is checked inside the write lock, so a retry
                // racing the original cannot slip past. The lookup binds all
                // three columns of the key — `subject`, `namespace` and
                // `key` — never `key` alone: `idempotency`'s primary key is
                // `(subject, namespace, key)` precisely because two subjects
                // may choose the same caller-supplied key, and a lookup that
                // ignored subject/namespace would replay one subject's
                // stored `AppliedWrite` — `item_id` and `audit_id` included —
                // to another. See `schema::idempotency`'s DDL comment.
                if let Some(key) = &txn.idempotency_key {
                    let prior: Option<(String, String)> = conn
                        .query_row(
                            "SELECT payload_digest, outcome FROM idempotency
                             WHERE subject = ?1 AND namespace = ?2 AND key = ?3",
                            params![
                                txn.scope.subject.as_str(),
                                txn.scope.namespace.as_str(),
                                key
                            ],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        // `.optional()`, not `.ok()`: only "no such row" may
                        // fall through to a fresh apply below — a genuine
                        // storage error must still propagate rather than be
                        // read as "never applied before".
                        .optional()
                        .sql()?;
                    if let Some((digest, outcome)) = prior {
                        if txn.payload_digest.as_deref() != Some(digest.as_str()) {
                            return Err(BackendError::IdempotencyConflict);
                        }
                        let mut replay: AppliedWrite = serde_json::from_str(&outcome)
                            .map_err(|e| tenant::storage_error(e, false))?;
                        replay.replayed = true;
                        replay.replayed_outcome = Some(outcome);
                        return Ok(replay);
                    }
                }

                let tx = conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(|e| tenant::storage_error(e, false))?;
                // Stays the transaction's first statement, ahead of the
                // eviction loop below — but not because statement order
                // decides the lock any more. This transaction opens as
                // `TransactionBehavior::Immediate`, which takes the write
                // lock the moment it opens, before any statement runs, so
                // going first no longer determines how the transaction
                // opened. `capacity::adjust` also calls `ensure_row`
                // internally, so this call was always redundant for *its*
                // own correctness. It stays first anyway, as defence in
                // depth: insurance against a future path that opens this
                // transaction as `Deferred` again, at which point statement
                // order would matter exactly as it used to. See
                // `crate::aggregates`' module doc for the full history.
                capacity::ensure_row(&tx, &txn.scope)?;

                let mut delta_items: i64 = 0;
                let mut delta_bytes: i64 = 0;
                let mut evicted = Vec::new();

                for id in &txn.evictions {
                    let size = items::delete(&tx, &txn.scope, id)?;
                    // D1 (see the task report's appendices for the fix's
                    // full history): no separate `vectors::delete` call here
                    // — deliberately, and there is no such function in this
                    // crate any more. `vectors.item_id` is `REFERENCES
                    // items(id) ON DELETE CASCADE` and `schema::initialise`
                    // turns `foreign_keys` on for every connection this loop
                    // runs on, so `items::delete`'s own `DELETE FROM items
                    // ...` above already removes the matching `vectors` row
                    // whenever it actually deletes an `items` row — measured
                    // to fire inside exactly this shape of transaction by
                    // `tests::the_cascade_fires_inside_an_immediate_transaction_on_an_initialised_connection`.
                    //
                    // A second, manual `vectors::delete(&tx, id)` used to sit
                    // here, unconditionally and with no scope predicate of
                    // its own. That call is D1: `items::delete`'s own
                    // subject/namespace predicate correctly refused an
                    // out-of-scope id (`size` stayed `0`, so the cascade
                    // never had an `items` deletion to fire from), but the
                    // manual `vectors` delete ran anyway — widowing a live
                    // item in another scope, stripping its vector while the
                    // item itself stayed fully readable through `get`/`list`.
                    //
                    // A later fix round restored that call with a scope
                    // predicate of its own, as recorded defence-in-depth
                    // against the cascade failing (a future schema dropping
                    // `ON DELETE CASCADE`, or a connection with
                    // `foreign_keys` off). Retired again, deliberately: both
                    // failure modes the defence targeted are already caught
                    // loudly by the existing suite the moment they occur —
                    // dropping `ON DELETE CASCADE` from the DDL fails 4
                    // tests outright (`foreign_keys=ON` then rejects the
                    // `items` delete itself, `FOREIGN KEY constraint
                    // failed`), and flipping `schema::initialise`'s
                    // `foreign_keys` pragma to `"OFF"` with the cascade
                    // intact fails 5, including
                    // `tests::eviction_in_scope_still_deletes_the_vector_row`'s
                    // `scope_embedder` assertion specifically — see the task
                    // report for both runs. Three tests pin the cascade and
                    // the pragma today:
                    // `schema::tests::initialise_turns_foreign_keys_on_and_an_item_delete_cascades_to_its_vector`,
                    // `vectors::tests::deleting_an_item_cascades_to_its_vector`,
                    // `tenant::tests::a_freshly_opened_tenant_is_in_wal_mode_with_foreign_keys_on`.
                    // A defence that duplicates a check the suite already
                    // makes loudly, at the cost of a second, independently
                    // scoped `DELETE` statement that must itself stay
                    // correct forever, was paying more than it protected —
                    // and what it protected against, both times it existed,
                    // is exactly what this comment exists to make sure never
                    // comes back as a silent regression instead of a design
                    // decision.
                    //
                    // See `tests::eviction_out_of_scope_does_not_widow_another_scopes_item`
                    // and `tests::eviction_in_scope_still_deletes_the_vector_row`.
                    if size > 0 {
                        delta_items -= 1;
                        delta_bytes -= size as i64;
                        // Inside the guard, with the counters. `AppliedWrite::evicted`
                        // is the ids actually removed, not the ids the caller asked
                        // to remove — and an id naming no row is not forbidden. It
                        // was outside, so the counters and the report disagreed from
                        // inside one loop; and because `AppliedWrite` is what an
                        // idempotency row stores as its replayed outcome, a phantom
                        // entry was replayed for as long as the key lived.
                        evicted.push(id.clone());
                    }
                }

                let mut item_id = None;

                if let Some(w) = &txn.upsert {
                    items::insert(&tx, &w.item)?;
                    if let Some(v) = &w.vector {
                        vectors::insert(&tx, &w.item.id, &txn.scope, v)?;
                    }
                    delta_items += 1;
                    delta_bytes += w.item.byte_size() as i64;
                    item_id = Some(w.item.id.clone());
                }

                if let Some(m) = &txn.merge {
                    let diff =
                        items::merge(&tx, &txn.scope, &m.target, &m.body, &m.tags, &m.attrs)?;
                    if let Some(v) = &m.vector {
                        vectors::insert(&tx, &m.target, &txn.scope, v)?;
                    }
                    delta_bytes += diff;
                    item_id = Some(m.target.clone());
                }

                capacity::adjust(&tx, &txn.scope, delta_items, delta_bytes)?;
                let audit_id = audit::insert(&tx, &txn.audit)?;
                // Carried forward from Task 20's partial `apply`. This block
                // replaces that one wholesale, so an increment omitted here
                // is an aggregate table that is never written at all — which
                // is how it was lost once already.
                aggregates::increment(&tx, &txn.audit)?;

                let applied = AppliedWrite {
                    item_id,
                    audit_id,
                    evicted,
                    replayed: false,
                    replayed_outcome: None,
                };

                if let Some(key) = &txn.idempotency_key {
                    tx.execute(
                        "INSERT INTO idempotency (key, subject, namespace, payload_digest,
                             outcome, at)
                         VALUES (?1,?2,?3,?4,?5,?6)",
                        params![
                            key,
                            txn.scope.subject.as_str(),
                            txn.scope.namespace.as_str(),
                            txn.payload_digest.clone().unwrap_or_default(),
                            serde_json::to_string(&applied)
                                .map_err(|e| tenant::storage_error(e, false))?,
                            time::OffsetDateTime::now_utc().unix_timestamp(),
                        ],
                    )
                    .sql()?;
                }

                tx.commit().map_err(|e| tenant::storage_error(e, false))?;
                Ok(applied)
            })
            .await
    }

    async fn retrieve_candidates(
        &self,
        scope: &Scope,
        query: &CandidateQuery,
    ) -> Result<Vec<ScoredCandidate>, BackendError> {
        let (scope, query) = (scope.clone(), query.clone());
        self.tenants
            .with_conn(&scope.tenant.clone(), move |c| {
                retrieve::candidates(c, &scope, &query)
            })
            .await
    }
    async fn neighbours(
        &self,
        scope: &Scope,
        embedding: &Embedding,
        k: usize,
    ) -> Result<Vec<ScoredCandidate>, BackendError> {
        let (scope, embedding) = (scope.clone(), embedding.clone());
        self.tenants
            .with_conn(&scope.tenant.clone(), move |c| {
                // Refuse a probe from a model the scope was not indexed with.
                if let Some((stored, dim)) = vectors::scope_embedder(c, &scope)?
                    && (stored != embedding.embedder.to_string() || dim != embedding.dim)
                {
                    return Err(BackendError::EmbedderMismatch {
                        got: format!("{}:{}", embedding.embedder, embedding.dim),
                        expected: format!("{stored}:{dim}"),
                    });
                }
                let probe = memorysafe_embed::QuantizedVector::from_embedding(&embedding);
                // `neighbours` carries no `HardFilters` of its own — it is not
                // given a `CandidateQuery` — so this is a policy-free scan of
                // the scope, exactly as it was before `vectors::search` grew a
                // `filters` parameter. `sensitivity_ceiling` is the only
                // `HardFilters` field that is restrictive by default
                // (`Internal`, fail-closed for `retrieve_candidates`'s
                // caller), so it is the one field overridden to the most
                // permissive level; every other field's default already
                // excludes nothing.
                let filters = HardFilters {
                    sensitivity_ceiling: SensitivityLevel::Restricted,
                    ..HardFilters::default()
                };
                let hits = vectors::search(c, &scope, &probe, &filters, k)?;
                Ok(hits
                    .into_iter()
                    .map(|(item, access, score)| ScoredCandidate {
                        estimated_tokens: estimate_tokens(&item.body),
                        item,
                        relevance: score,
                        vector_score: Some(score),
                        keyword_score: None,
                        value: memorysafe_core::Score::ZERO,
                        fragility: memorysafe_core::Score::ZERO,
                        // `last_access`/`access_count` are the two columns the
                        // schema has declared since Task 19 and nothing read
                        // until now. Select them alongside `ITEM_COLUMNS` and
                        // return them from `vectors::search` as their own
                        // tuple element — they must NOT go on `MemoryItem`,
                        // which is exported and digested. A row that has never
                        // been recalled reads back `(None, 0)`, never
                        // `(created_at, 0)`.
                        last_accessed_at: access.last_accessed_at,
                        access_count: access.access_count,
                    })
                    .collect())
            })
            .await
    }
    async fn capacity_state(&self, scope: &Scope) -> Result<CapacityState, BackendError> {
        let scope = scope.clone();
        self.tenants
            .with_conn(&scope.tenant.clone(), move |c| capacity::state(c, &scope))
            .await
    }
    async fn scope_stats(&self, scope: &Scope) -> Result<ScopeStats, BackendError> {
        let scope = scope.clone();
        self.tenants
            .with_conn(&scope.tenant.clone(), move |c| capacity::stats(c, &scope))
            .await
    }
    async fn set_budget(&self, scope: &Scope, budget: Budget) -> Result<(), BackendError> {
        let scope = scope.clone();
        self.tenants
            .with_write(&scope.tenant.clone(), move |c| {
                capacity::set_budget(c, &scope, budget)
            })
            .await
    }
    async fn purge_subject(
        &self,
        tenant: &TenantId,
        subject: &SubjectId,
        cascade: PurgeCascade,
        audit: AuditRecord,
    ) -> Result<PurgeReport, BackendError> {
        let (tenant, subject) = (tenant.clone(), subject.clone());
        self.tenants
            .with_write(&tenant, move |c| {
                purge::subject(c, &subject, cascade, &audit)
            })
            .await
    }
    async fn export(&self, sel: &ScopeSelector) -> Result<ExportStream, BackendError> {
        let sel = sel.clone();
        self.tenants
            .with_conn(&sel.tenant.clone(), move |c| portability::export(c, &sel))
            .await
    }
    async fn import(
        &self,
        destination: &TenantId,
        stream: ImportStream,
    ) -> Result<ImportReport, BackendError> {
        // Deliberately no "a stream may not span tenants" check and no
        // "stream contains no items" rejection.
        //
        // The span check is strictly implied: `portability::import` compares
        // every record against `destination`, so two records cannot disagree
        // with each other without at least one of them disagreeing with the
        // destination first. A second rule that is true only by implication
        // has no test of its own, cannot fail today, and silently stops being
        // implied the day someone weakens the first.
        //
        // The empty-stream rejection existed only to derive a tenant from the
        // first item. The destination is now a parameter, so there is nothing
        // left to derive and nothing left to reject: a header-only stream is
        // a valid export of an empty tenant, and the round trip has to
        // survive it.
        let dest = destination.clone();
        self.tenants
            .with_write(destination, move |c| portability::import(c, &dest, stream))
            .await
    }
    async fn audit_aggregates(
        &self,
        tenant: &TenantId,
        filter: &AuditAggregateFilter,
    ) -> Result<Vec<AuditAggregate>, BackendError> {
        let (tenant, filter) = (tenant.clone(), filter.clone());
        self.tenants
            .with_conn(&tenant.clone(), move |c| {
                aggregates::query(c, &tenant, &filter)
            })
            .await
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

    /// `AppliedWrite::evicted` is the ids **actually removed**, not the ids the
    /// caller asked to remove — see its doc on `WriteTransaction`. Nothing
    /// forbids a transaction naming an eviction that matches no row, and the
    /// push sat outside the `size > 0` guard, so the report claimed a phantom
    /// while the capacity counters correctly ignored it: two answers out of one
    /// loop.
    ///
    /// It matters beyond a wrong field because `AppliedWrite` is what an
    /// idempotency row stores as its replayed outcome, so a phantom would be
    /// replayed identically for as long as the key lived.
    #[tokio::test]
    async fn a_phantom_eviction_is_not_reported_as_evicted() {
        let b = backend();
        let s = scope("s", "n");
        let real = fx::item(&s, "this one exists");
        b.apply(fx::admit_txn(&s, real.clone(), None))
            .await
            .unwrap();

        let ghost = fx::item(&s, "never inserted").id;
        let applied = b
            .apply(fx::evict_txn(&s, vec![real.id.clone(), ghost.clone()]))
            .await
            .unwrap();

        assert_eq!(
            applied.evicted,
            vec![real.id.clone()],
            "evicted must list what was removed; {ghost} matched no row"
        );
        // The premise: the real eviction did happen, so an empty vec cannot
        // satisfy the assertion above for the wrong reason.
        assert!(b.get(&s, &real.id).await.unwrap().is_none());
    }

    /// D1: an out-of-scope eviction **widows** a live item — not a
    /// cross-scope *orphan*, and not a tenant-isolation defect. Terminology,
    /// precisely, because the two failure shapes are opposite and this is
    /// the one D1 actually produces: with `vectors.item_id ... ON DELETE
    /// CASCADE` and `foreign_keys=ON`, a `vectors` row with no matching
    /// `items` row (an orphan) cannot exist at all — the FK forbids it. What
    /// the pre-fix code produced instead was an `items` row that stays
    /// fully readable through `get`/`list` while its `vectors` row is gone:
    /// a live item stripped of its vector, permanently invisible to vector
    /// search, with no error anywhere. One database file per tenant is
    /// structural and unaffected here; `subject`/`namespace` are query
    /// predicates within that one file, and this is where the eviction loop
    /// dropped one of them.
    ///
    /// The eviction loop used to call a `vectors::delete(&tx, id)` that
    /// carried no scope predicate of its own, unconditionally, outside the
    /// `if size > 0` guard that `items::delete`'s own subject/namespace
    /// predicate already computed. So a transaction scoped to B naming A's
    /// id as an eviction left A's `items` row alone (the scoped `DELETE`
    /// matched no row, `size == 0`, so the cascade never had anything to
    /// fire from) but still deleted A's `vectors` row — the refusal was
    /// computed and then thrown away, widowing A's item.
    ///
    /// `b.get` proves the item survives (the existing scoped guard already
    /// gets that right — this is not a duplicate of
    /// `a_phantom_eviction_is_not_reported_as_evicted`); `vector_row_count`
    /// is what actually distinguishes the buggy version from the fixed one
    /// — `home` holds exactly one vector throughout, so its count dropping
    /// to 0 while the item itself survives is precisely the widowing this
    /// test exists to catch.
    ///
    /// **One dimension varied at a time**, the same methodology
    /// `vectors::tests::search_and_scope_embedder_are_scoped_by_subject_and_namespace`
    /// documents and uses. After the fix, an out-of-scope eviction is
    /// refused by `items::delete`'s own predicate alone — `size` stays `0`,
    /// so the cascade (the fix's sole mechanism; there is no
    /// `vectors::delete` in this crate any more) never has an `items` row
    /// deleted for it to fire from, and nothing else touches `vectors` at
    /// all. A version of this test that varied only the subject would pass
    /// a fix whose predicate dropped the namespace half, and vice versa —
    /// so `attacker_subject` differs from home in subject alone and
    /// `attacker_namespace` differs in namespace alone, each sharing home's
    /// other coordinate.
    #[tokio::test]
    async fn eviction_out_of_scope_does_not_widow_another_scopes_item() {
        let b = backend();
        let home = scope("s", "n");
        let attacker_subject = scope("other-s", "n");
        let attacker_namespace = scope("s", "other-n");

        let item = fx::item(&home, "owned by home, embedded for search");
        b.apply(fx::admit_txn_embedded(&home, item.clone()))
            .await
            .unwrap();

        for attacker in [&attacker_subject, &attacker_namespace] {
            // Scoped to the attacker; its evictions name home's id.
            // `items::delete` refuses (home's item is out of scope for the
            // attacker) — on the pre-fix code, the bug ran an unscoped
            // `vectors::delete` anyway, widowing home's item regardless of
            // which half of the predicate the attacker differed on.
            b.apply(fx::evict_txn(attacker, vec![item.id.clone()]))
                .await
                .unwrap();
        }

        assert!(
            b.get(&home, &item.id).await.unwrap().is_some(),
            "an out-of-scope eviction deleted the item itself, not just its vector"
        );

        assert_eq!(
            b.vector_row_count(&home).await.unwrap(),
            1,
            "an eviction scoped to a different subject or namespace deleted \
             home's vector row for an id it was never allowed to touch"
        );
    }

    /// D1's companion: the in-scope path must still remove the vector row —
    /// via the cascade, which is the shipped fix's sole mechanism (see the
    /// task report's appendices for why an earlier round's explicit,
    /// scoped `vectors::delete` was tried and then retired). The fix must
    /// not satisfy the widow test above by deleting nothing ever — this
    /// proves the good path still works. No conformance test binds this
    /// today — every eviction call site in
    /// `crates/memorysafe-backend/src/conformance/` builds a single scope, so
    /// the in-scope cleanup path is unexercised there, and this is the only
    /// place left standing that fails if an over-broad fix quietly stops
    /// deleting the vector row on the *good* path.
    ///
    /// **Three readers, because none alone is diagnostic for every failure
    /// shape.** `b.get` proves the item itself is gone — necessary, but not
    /// sufficient. `neighbours` is the user-visible outcome, but on its own
    /// it **cannot** distinguish "the vector row was actually removed" from
    /// "it survived as an invisible orphan": `vectors::search`'s query is an
    /// `INNER JOIN` on `items` (see that function's doc), so once
    /// `items::delete` has removed the `items` row — which it does
    /// correctly here, on both the buggy and the fixed code, since this
    /// eviction is genuinely in scope — the join drops any orphaned
    /// `vectors` row regardless of whether one exists. Asserting
    /// `hits.is_empty()` alone would pass identically whether or not the fix
    /// actually cleaned up `vectors`.
    ///
    /// `vectors::scope_embedder` is what closes that gap: it reads
    /// `vectors`' own `subject`/`namespace` directly, with **no** join to
    /// `items`. This scope holds no other vector, so if the evicted item's
    /// row survived as an orphan, `scope_embedder` would still report
    /// `Some(("deterministic-256", 256))` — a stale model identity for a
    /// scope that (from `neighbours`' point of view) looks empty. Asserting
    /// it is `None` is the assertion that actually fails if the vector row
    /// lingers. `vector_row_count` is kept as the most direct instrument of
    /// the three, and the one every mutation below is checked against
    /// first — it is the same unjoined, table-own read as `scope_embedder`,
    /// just phrased as a count.
    #[tokio::test]
    async fn eviction_in_scope_still_deletes_the_vector_row() {
        use memorysafe_embed::Embedder;

        let b = backend();
        let s = scope("s", "n");
        let item = fx::item(&s, "owned by s, embedded for search");
        b.apply(fx::admit_txn_embedded(&s, item.clone()))
            .await
            .unwrap();

        b.apply(fx::evict_txn(&s, vec![item.id.clone()]))
            .await
            .unwrap();

        assert!(
            b.get(&s, &item.id).await.unwrap().is_none(),
            "the in-scope eviction did not remove the item itself"
        );

        let probe = fx::embedder().embed(&item.body).unwrap();
        let hits = b.neighbours(&s, &probe, 10).await.unwrap();
        assert!(
            hits.is_empty(),
            "the evicted item's vector still came back through neighbours"
        );

        let embedder_identity = b
            .tenants
            .with_conn(&s.tenant.clone(), {
                let s = s.clone();
                move |c| vectors::scope_embedder(c, &s)
            })
            .await
            .unwrap();
        assert_eq!(
            embedder_identity, None,
            "scope_embedder still reports a stored model after the scope's \
             only vector was evicted — the vectors row survived as an \
             orphan that neighbours/search cannot see, because their query \
             joins through items, which the eviction correctly deleted"
        );

        assert_eq!(
            b.vector_row_count(&s).await.unwrap(),
            0,
            "an in-scope eviction left the vector row behind"
        );
    }

    /// **Isolates one question: does the `ON DELETE CASCADE` on
    /// `vectors.item_id` fire inside a `TransactionBehavior::Immediate`
    /// transaction, on a connection that went through `schema::initialise`
    /// (which turns `foreign_keys` on)?** `apply` opens exactly this kind of
    /// transaction, so this measures the transaction-shape question that a
    /// bare, non-transactional unit test cannot answer.
    ///
    /// **What this test does *not* claim.** Unlike
    /// `schema::tests::initialise_turns_foreign_keys_on_and_an_item_delete_cascades_to_its_vector`,
    /// this test does not force `PRAGMA foreign_keys=OFF` before calling
    /// `initialise` — so it cannot tell an `initialise` with the
    /// `foreign_keys` pragma line deleted from one with it present; that
    /// regression is that other test's job. It also uses
    /// `Connection::open_in_memory`, not `Connection::open(&path)` — the
    /// pool's own connections are file-backed with WAL in effect
    /// (`tenant::Pool::handle`) — so this measures the cascade/transaction
    /// interaction on a connection built the same way (`initialise`, then a
    /// manual `Immediate` transaction), not on a connection that is
    /// byte-for-byte the one `apply` runs on. `foreign_keys` semantics do
    /// not depend on in-memory vs. file-backed storage, so the substance
    /// carries over, but the claim is scoped to what was actually run.
    ///
    /// This test calls `items::delete` directly — there is no
    /// `vectors::delete` in this crate any more to call instead of it. The
    /// cascade measured here is the *only* mechanism that removes a
    /// `vectors` row on eviction; see the task report's appendices for why
    /// an earlier fix round's explicit, scoped `vectors::delete` (a second,
    /// independent line of defence) was tried and then retired as
    /// redundant with what this test proves.
    #[test]
    fn the_cascade_fires_inside_an_immediate_transaction_on_an_initialised_connection() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        schema::initialise(&conn).unwrap();

        let s = scope("s", "n");
        let item = fx::item(&s, "cascade probe");
        items::insert(&conn, &item).unwrap();
        let q = fx::vector_for(&item.body);
        vectors::insert(&conn, &item.id, &s, &q).unwrap();

        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let size = items::delete(&tx, &s, &item.id).unwrap();
        assert!(size > 0, "the probe's own item::delete must have matched");
        tx.commit().unwrap();

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM vectors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            remaining, 0,
            "the ON DELETE CASCADE did not fire inside an Immediate transaction \
             on an initialise()-opened connection"
        );
    }

    /// **The aggregate write rule, on `apply`, and this test is the only thing
    /// that can see it.**
    ///
    /// No conformance test bound in this crate observes `audit_aggregates`:
    /// the read half is stubbed until the portability task, so
    /// `aggregates::increment` can be deleted from `apply` and all ten
    /// conformance tests still pass — measured, not assumed. That is the
    /// `foreign_keys` shape: a real property with a present mechanism and no
    /// test that can fail.
    ///
    /// **It establishes the absence before asserting the presence.** The table
    /// is asserted empty first, so "there is an aggregate row" cannot be
    /// satisfied by a row that was already there — the mistake the pragma case
    /// in `schema.rs` records.
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

    /// A merge folds into an existing item, adjusts capacity by the byte-size
    /// delta `items::merge` returns (not by the item's absolute size, which
    /// would double-count the bytes it already held), and writes one audit
    /// row and one aggregate increment — on the same terms `apply`'s insert
    /// path already had. This is the interaction no conformance test in this
    /// crate's `capacity.rs` module reaches: `capacity_accounting_tracks_items_and_bytes`
    /// and `eviction_releases_capacity` only ever admit or evict, never
    /// merge.
    #[tokio::test]
    async fn a_merge_folds_the_item_and_adjusts_capacity_by_the_delta_not_the_new_size() {
        let b = backend();
        let s = scope("s", "n");

        let target = fx::item(&s, "a short body");
        b.apply(fx::admit_txn(&s, target.clone(), None))
            .await
            .unwrap();
        let before_bytes = b.capacity_state(&s).await.unwrap().used_bytes;
        assert_eq!(before_bytes, target.byte_size());

        let new_body = "a considerably longer body than the one this item started with";
        let audit = memorysafe_core::AuditRecord::new(
            s.clone(),
            AuditEvent::Merged,
            vec![memorysafe_core::ItemRef::from_item(&target)],
            Actor::system(),
            target.created_at,
        );
        let mut txn = memorysafe_backend::WriteTransaction::new(s.clone(), audit);
        txn.merge = Some(memorysafe_backend::write::MergeWrite {
            target: target.id.clone(),
            body: new_body.into(),
            tags: vec!["merged-in".into()],
            attrs: Default::default(),
            vector: None,
            byte_size: new_body.len() as u64,
        });
        assert!(txn.is_valid(), "the premise: this transaction is valid");

        let applied = b.apply(txn).await.unwrap();
        assert_eq!(applied.item_id, Some(target.id.clone()));

        let merged = b
            .get(&s, &target.id)
            .await
            .unwrap()
            .expect("target survives");
        assert_eq!(merged.body, new_body);
        assert_eq!(merged.tags, vec!["merged-in".to_string()]);

        let after = b.capacity_state(&s).await.unwrap();
        assert_eq!(
            after.used_items, 1,
            "a merge must not change the item count"
        );
        assert_eq!(
            after.used_bytes,
            merged.byte_size(),
            "capacity must reflect the merged item's actual new size, which \
             is only true if the delta `items::merge` returned was applied \
             rather than the new size being added on top of the old"
        );

        let rows = aggregate_rows(&b, &s.tenant).await;
        assert!(
            rows.iter().any(|r| r.2 == "merged" && r.4 == 1),
            "a merge must write exactly one aggregate increment: {rows:?}"
        );
    }

    /// The companion the capacity-after-merge test above cannot see:
    /// `items::merge`'s own `UPDATE` writes `items.byte_size`, and that
    /// column — not the delta `merge` returns — is what a *later* eviction
    /// reads to compute its own capacity release (`items::delete`). A merge
    /// that computes the right delta at merge time but persists the
    /// pre-merge size into that column leaves `capacity.used_bytes` reading
    /// correctly right up until the item is evicted, at which point too few
    /// bytes are released and the drift becomes permanent — silent, because
    /// nothing reads `items.byte_size` again until then.
    ///
    /// Round-trips through `apply` end to end (admit, merge to a much larger
    /// body, evict) so it exercises exactly the column a unit test on
    /// `items::merge` alone cannot: `MemoryItem::byte_size()` recomputes from
    /// body/tags/attrs and never reads the stored column, so asserting
    /// against it — as the test above does, correctly, for the merge-time
    /// delta — cannot catch a stale column that only misbehaves on the next
    /// write.
    #[tokio::test]
    async fn evicting_a_merged_item_releases_its_post_merge_size_not_its_pre_merge_one() {
        let b = backend();
        let s = scope("s", "n");

        let target = fx::item(&s, "short");
        b.apply(fx::admit_txn(&s, target.clone(), None))
            .await
            .unwrap();

        let new_body = "a considerably longer body than the one this item \
                         started with, so the post-merge charge is \
                         unambiguously larger than the pre-merge one";
        let audit = memorysafe_core::AuditRecord::new(
            s.clone(),
            AuditEvent::Merged,
            vec![memorysafe_core::ItemRef::from_item(&target)],
            Actor::system(),
            target.created_at,
        );
        let mut txn = memorysafe_backend::WriteTransaction::new(s.clone(), audit);
        txn.merge = Some(memorysafe_backend::write::MergeWrite {
            target: target.id.clone(),
            body: new_body.into(),
            tags: vec![],
            attrs: Default::default(),
            vector: None,
            byte_size: new_body.len() as u64,
        });
        b.apply(txn).await.unwrap();

        // Sanity: the merge did grow the item, or the eviction below proves
        // nothing about which size was released.
        let merged = b.get(&s, &target.id).await.unwrap().unwrap();
        assert!(
            merged.byte_size() > target.byte_size(),
            "the premise: the merge must grow the item"
        );

        b.apply(fx::evict_txn(&s, vec![target.id.clone()]))
            .await
            .unwrap();

        let after_evict = b.capacity_state(&s).await.unwrap();
        assert_eq!(
            after_evict.used_items, 0,
            "eviction did not release the merged item's slot"
        );
        assert_eq!(
            after_evict.used_bytes, 0,
            "eviction released the wrong number of bytes: this only returns \
             to exactly 0 if `items.byte_size` was rewritten to the item's \
             post-merge size rather than left at its pre-merge value — a \
             nonzero residue here is exactly the permanent capacity drift a \
             stale column produces"
        );
    }

    /// `items::merge`'s lookup is scoped by subject and namespace through
    /// `items::get`, so a merge naming a target that exists but belongs to a
    /// different subject reports `MergeTargetMissing` rather than rewriting
    /// it. Mutated explicitly per the task's dispatch notes, which flagged
    /// this predicate as the one most likely to have shipped unenforced —
    /// two of the last three tasks each found exactly this shape of gap in a
    /// different scoped predicate.
    #[tokio::test]
    async fn a_merge_cannot_reach_a_target_in_a_different_scope() {
        let b = backend();
        let home = scope("s", "n");
        let elsewhere = scope("other-s", "n");

        let target = fx::item(&elsewhere, "not home's item");
        b.apply(fx::admit_txn(&elsewhere, target.clone(), None))
            .await
            .unwrap();

        let mut txn = fx::admit_txn(&home, fx::item(&home, "irrelevant upsert"), None);
        txn.upsert = None;
        txn.merge = Some(memorysafe_backend::write::MergeWrite {
            target: target.id.clone(),
            body: "a body the target must never acquire".into(),
            tags: vec![],
            attrs: Default::default(),
            vector: None,
            byte_size: 11,
        });
        assert!(txn.is_valid(), "the premise: this transaction is valid");

        let err = b.apply(txn).await.unwrap_err();
        assert!(
            matches!(err, BackendError::MergeTargetMissing(ref id) if *id == target.id),
            "a merge reached across a scope boundary instead of reporting the \
             target missing: {err:?}"
        );

        let survivor = b
            .get(&elsewhere, &target.id)
            .await
            .unwrap()
            .expect("the target must survive the rejected cross-scope merge");
        assert_eq!(
            survivor.body, target.body,
            "the cross-scope merge target was rewritten"
        );
    }

    /// Two subjects choosing the same idempotency key must not see each
    /// other's outcome. `schema::tests::two_subjects_may_use_the_same_idempotency_key`
    /// pins the DDL that makes this representable at all — the primary key
    /// is `(subject, namespace, key)`, not `key` alone — but nothing in the
    /// conformance suite pairs two subjects against one key (both idempotency
    /// tests there use a single scope), so this crate-local test is what
    /// guards `apply`'s lookup actually binding all three columns rather than
    /// `key` alone, until a conformance test for this is queued before the
    /// freeze.
    ///
    /// **Also asserts `audit_id`, not only `item_id`.** Neither conformance
    /// idempotency test compares `audit_id` at all, so a replay path that
    /// mints a fresh `AuditId` instead of returning the one actually stored
    /// — naming an audit row that does not exist, and disagreeing with
    /// `replayed_outcome`'s own JSON — passes the whole suite. Checked here
    /// on the same retry that already exists for `item_id`.
    #[tokio::test]
    async fn two_subjects_replaying_the_same_idempotency_key_each_get_their_own_outcome() {
        let b = backend();
        let a = scope("subject-a", "n");
        let z = scope("subject-z", "n");

        let item_a = fx::item(&a, "subject a's memory");
        let mut txn_a = fx::admit_txn(&a, item_a.clone(), None);
        txn_a.idempotency_key = Some("shared-key".into());
        txn_a.payload_digest = Some(item_a.digest());
        let applied_a = b.apply(txn_a).await.unwrap();
        assert!(!applied_a.replayed);

        // Subject z uses the same key with its own, different payload. If
        // the lookup bound `key` alone this would find subject a's row and
        // — since the digests differ — fail with `IdempotencyConflict`
        // rather than admitting subject z's own write.
        let item_z = fx::item(&z, "subject z's memory");
        let mut txn_z = fx::admit_txn(&z, item_z.clone(), None);
        txn_z.idempotency_key = Some("shared-key".into());
        txn_z.payload_digest = Some(item_z.digest());
        let applied_z = b.apply(txn_z).await.unwrap();
        assert!(
            !applied_z.replayed,
            "subject z's write was treated as a replay of subject a's outcome"
        );
        assert_ne!(
            applied_z.item_id, applied_a.item_id,
            "two subjects sharing an idempotency key collapsed into one outcome"
        );

        // And each subject, retried, replays its *own* stored outcome.
        let retry_a_item = {
            let mut i = fx::item(&a, "subject a's memory");
            i.id = ItemId::new();
            i
        };
        let mut retry_a = fx::admit_txn(&a, retry_a_item, None);
        retry_a.idempotency_key = Some("shared-key".into());
        retry_a.payload_digest = Some(item_a.digest());
        let replayed_a = b.apply(retry_a).await.unwrap();
        assert!(replayed_a.replayed);
        assert_eq!(
            replayed_a.item_id, applied_a.item_id,
            "subject a's retry replayed the wrong subject's outcome"
        );
        assert_eq!(
            replayed_a.audit_id, applied_a.audit_id,
            "a replay must return the audit id actually stored, not a \
             freshly minted one that names no row in the audit table"
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

    /// `neighbours` carries each hit's access statistics from storage, not a
    /// constant `(None, 0)`. Nothing in `tests/conformance.rs` pins this yet:
    /// `retrieval::recall_updates_access_statistics` — the conformance test
    /// that owns this property — reads through `retrieve_candidates`, which
    /// is still a stub, so it cannot bind until Task 22, and even then it
    /// exercises the *other* retrieval path. Mutating the `access.*` mapping
    /// in `neighbours` to hardcode `(None, 0)` was run against the ten bound
    /// conformance tests and all ten stayed green — a mutant no test caught —
    /// which is what this test exists to close.
    ///
    /// **Both directions are asserted, and the never-recalled one is the
    /// sharp half.** `AccessStats`'s own doc states, in bold, that a
    /// never-recalled row must read back `(None, 0)` and never
    /// `(Some(created_at), 0)`. A mutation in `vectors::access_stats` that
    /// falls back to `created_at` when `last_access` is `NULL` — the exact
    /// shape the doc warns about — left the whole workspace green until this
    /// bystander item and its `is_none()` assertion existed: `fx::item` pins
    /// `created_at` to `UNIX_EPOCH`, so a wrongly-defaulted
    /// `Some(created_at)` carries the same instant a real recall at the
    /// epoch would, and no assertion on the *timestamp* can tell them apart
    /// — only the `Option` itself can, which is why this checks `is_none()`
    /// rather than comparing against a value.
    #[tokio::test]
    async fn neighbours_reports_the_items_stored_access_statistics() {
        use memorysafe_embed::Embedder;

        let b = backend();
        let s = scope("s", "n");
        let recalled = fx::item(&s, "the cat sat on the mat");
        let bystander = fx::item(&s, "the cat sat on a rug");
        for item in [recalled.clone(), bystander.clone()] {
            b.apply(fx::admit_txn_embedded(&s, item)).await.unwrap();
        }

        let at = OffsetDateTime::UNIX_EPOCH + Duration::seconds(3_600);
        let record = AuditRecord::new(
            s.clone(),
            AuditEvent::Recalled,
            vec![ItemRef::from_item(&recalled)],
            Actor::system(),
            at,
        );
        b.record_recall(record).await.unwrap();

        let probe = fx::embedder().embed("the cat sat on the mat").unwrap();
        // k = corpus size, so both come back regardless of ranking — the
        // never-recalled assertion below needs the bystander present, not
        // merely ranked highly enough to survive a smaller k.
        let hits = b.neighbours(&s, &probe, 2).await.unwrap();
        assert_eq!(
            hits.len(),
            2,
            "both items must come back, or the never-recalled assertion below \
             proves nothing"
        );

        let recalled_hit = hits
            .iter()
            .find(|h| h.item.id == recalled.id)
            .expect("the recalled item must be among the hits");
        assert_eq!(
            recalled_hit.access_count, 1,
            "neighbours did not report the recalled item's access_count"
        );
        assert_eq!(
            recalled_hit.last_accessed_at,
            Some(at),
            "neighbours did not report the recalled item's last_accessed_at"
        );

        let bystander_hit = hits
            .iter()
            .find(|h| h.item.id == bystander.id)
            .expect("the never-recalled item must be among the hits");
        assert_eq!(
            bystander_hit.access_count, 0,
            "a never-recalled item must report access_count 0"
        );
        assert!(
            bystander_hit.last_accessed_at.is_none(),
            "a never-recalled item must report last_accessed_at as None, not \
             Some(created_at): got {:?}",
            bystander_hit.last_accessed_at
        );
    }

    /// Fix-round finding I3: `neighbours` builds its own `HardFilters` — with
    /// `sensitivity_ceiling` deliberately overridden to `Restricted` — rather
    /// than reusing `HardFilters::default()` (fail-closed at `Internal`),
    /// because `neighbours` carries no policy filters of its own and is used
    /// for merge-target search, not a policy-gated recall. That choice was
    /// argued in the task report but pinned by nothing: swapping the
    /// `Restricted` override for `HardFilters::default()` survives the whole
    /// workspace suite, because no other test here seeds an item above
    /// `Internal` through `neighbours`.
    ///
    /// This seeds one item at `SensitivityLevel::Restricted` — the top of the
    /// scale, and the level `HardFilters::default()`'s ceiling would exclude
    /// — and asserts `neighbours` still returns it.
    #[tokio::test]
    async fn neighbours_does_not_apply_a_policy_sensitivity_ceiling() {
        use memorysafe_embed::Embedder;

        let b = backend();
        let s = scope("s", "n");
        let item = fx::item_with(
            &s,
            "a restricted memory the merge search must still see",
            "fact",
            &[],
            memorysafe_core::SensitivityLevel::Restricted,
        );
        b.apply(fx::admit_txn_embedded(&s, item.clone()))
            .await
            .unwrap();

        let probe = fx::embedder().embed(&item.body).unwrap();
        let hits = b.neighbours(&s, &probe, 10).await.unwrap();
        assert_eq!(
            hits.len(),
            1,
            "neighbours must not apply a sensitivity ceiling of its own; a \
             HardFilters::default() (fail-closed at Internal) would exclude \
             this Restricted item"
        );
        assert_eq!(hits[0].item.id, item.id);
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
