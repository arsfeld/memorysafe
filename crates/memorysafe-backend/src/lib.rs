//! The storage seam. One trait covering persistence and retrieval, because
//! pgvector searches inside the database and a separate index trait would
//! bake the SQLite shape into the interface.

pub mod aggregates;
pub mod conformance;
pub mod portability;
pub mod query;
pub mod write;

use memorysafe_core::{
    AuditFilter, AuditId, AuditRecord, CapacityState, Embedding, ItemId, MemoryItem, Scope,
    ScopeStats, ScoredCandidate, SubjectId, TenantId,
};
use thiserror::Error;

pub use aggregates::{
    AggregateKey, AuditAggregate, AuditAggregateFilter, SCORE_HISTOGRAM_BUCKETS,
    SCORE_HISTOGRAM_EDGES, SCORE_HISTOGRAM_VERSION,
};
pub use portability::{
    ExportRecord, ExportStream, ExportVector, FORMAT_VERSION, ImportReport, ImportStream,
    ScopeSelector,
};
pub use query::{CandidateQuery, HardFilters, MAX_PAGE_LIMIT, Page};
pub use write::{AppliedWrite, ItemWrite, MergeWrite, PurgeReport, WriteTransaction};

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("storage failure: {message} (retryable: {retryable})")]
    Storage { message: String, retryable: bool },
    #[error("item {0} not found")]
    ItemNotFound(ItemId),
    #[error("merge target {0} does not exist")]
    MergeTargetMissing(ItemId),
    #[error("idempotency key reused with a different payload")]
    IdempotencyConflict,
    #[error("query is invalid: {0}")]
    InvalidQuery(String),
    #[error("transaction is invalid: {0}")]
    InvalidTransaction(String),
    #[error("vector uses embedder {got}, scope uses {expected}")]
    EmbedderMismatch { got: String, expected: String },
    #[error("import stream is malformed: {0}")]
    MalformedImport(String),
}

#[async_trait::async_trait]
pub trait Backend: Send + Sync {
    /// Ordered descending by `relevance`, ties broken by ascending `ItemId`.
    /// This must be a total order: `retrieve_candidates` truncates at
    /// `query.limit`, so tied relevance at the limit boundary means two
    /// backends that each conform to "descending by relevance" alone can
    /// return different *candidate sets* — not the same set in a different
    /// order. The policy then assesses different neighbours and can reach a
    /// different admit/merge/reject decision. Worse, `Reason` carries a
    /// `FeatureMap`, and those feature numbers are written into the audit
    /// record, so two backends would record different evidence for the same
    /// input — an auditor replaying a decision against the other backend
    /// could not reproduce it.
    ///
    /// Populates `ScoredCandidate::last_accessed_at` and `access_count` from
    /// each item's stored access statistics — the values `record_recall`
    /// maintains. An item never recalled comes back as `(None, 0)`, never
    /// `(Some(created_at), 0)`; see `ScoredCandidate::last_accessed_at` for
    /// why the two must stay distinguishable and why no fixture in this suite
    /// could tell them apart if they were not.
    async fn retrieve_candidates(
        &self,
        scope: &Scope,
        query: &CandidateQuery,
    ) -> Result<Vec<ScoredCandidate>, BackendError>;

    /// Ordered descending by `relevance`, ties broken by ascending `ItemId`
    /// — the same rule as `retrieve_candidates`, for the same reason:
    /// `neighbours` truncates too, at `k`, so tied similarity at the k-th
    /// position means two backends that each conform to "descending by
    /// relevance" alone can return different *neighbour sets* — not the
    /// same set in a different order. The tie-break must be applied before
    /// truncating at `k`, not after. Concretely, the policy's `best()`
    /// neighbour — the merge target — becomes nondeterministic on tied
    /// similarity if two backends break ties differently.
    ///
    /// Populates the access statistics on every returned candidate, on the
    /// same terms as `retrieve_candidates`.
    async fn neighbours(
        &self,
        scope: &Scope,
        embedding: &Embedding,
        k: usize,
    ) -> Result<Vec<ScoredCandidate>, BackendError>;

    async fn capacity_state(&self, scope: &Scope) -> Result<CapacityState, BackendError>;

    async fn scope_stats(&self, scope: &Scope) -> Result<ScopeStats, BackendError>;

    async fn apply(&self, txn: WriteTransaction) -> Result<AppliedWrite, BackendError>;

    /// Writes the recall's audit row **and**, in the same transaction, updates
    /// the access statistics of every item `record.items` references: each
    /// item's `access_count` increments by one and its `last_accessed_at`
    /// becomes `record.at`.
    ///
    /// `record.at` rather than a clock read, for the reason `AuditRecord::new`
    /// gives for `at` being supplied and never sampled: a replay has to
    /// produce comparable rows, and a statistic stamped from the wall clock
    /// would differ on every run.
    ///
    /// No separate trait method for this, and no separate write. `record`
    /// already carries exactly the ids that were recalled — that is what
    /// `ItemRef` is — so the increment rides the write that is happening
    /// anyway. A second method would let a backend audit a recall without
    /// counting it, or count it without auditing it, and the pair is the same
    /// fact.
    ///
    /// Only the referenced items are touched. A backend that bumps every item
    /// in the scope makes `last_accessed_at` mean "the scope was read", which
    /// is not what the replay quota needs to know.
    async fn record_recall(&self, record: AuditRecord) -> Result<AuditId, BackendError>;

    async fn get(&self, scope: &Scope, id: &ItemId) -> Result<Option<MemoryItem>, BackendError>;

    /// Ordered ascending by `created_at`, ties broken by ascending `id`
    /// (`id` is a ULID — itself a total, time-sortable order — so breaking
    /// ties by it never contradicts the primary sort). This must be a
    /// *total* order: ordering by timestamp alone is insufficient, since a
    /// bulk import can leave many rows with an identical `created_at`, and
    /// an unstable sort under `LIMIT`/`OFFSET` paging can then return the
    /// same row on two pages while silently dropping another. Every backend
    /// must use this same key and direction — SQLite paging ascending while
    /// Postgres paged descending would each "conform" to a total order
    /// stated without one, which is exactly the cross-backend drift this
    /// suite exists to prevent.
    async fn list(&self, scope: &Scope, page: &Page) -> Result<Vec<MemoryItem>, BackendError>;

    /// Ordered by `AuditId`, descending (newest first). The choice of key —
    /// `AuditId` rather than `at` — is `AuditFilter::after`'s rule; see its
    /// doc comment in `memorysafe-core` rather than restating the reasoning
    /// here.
    ///
    /// `filter.after`, when set, continues a previous page. Because the
    /// list is descending, "after" names a *position* in that returned
    /// order, not a point in time: the next page is restricted to
    /// `id < after` (strictly smaller), not `id > after` — the temporal
    /// reading would instead re-request rows already returned. Neither
    /// draft `Backend::audit` implementation wires up `after` yet, so this
    /// doc comment is the only place the direction is pinned down.
    ///
    /// `filter.since` and `filter.until` are both **inclusive** bounds: a
    /// record timestamped exactly at either edge matches. The conformance
    /// suite's window test deliberately places both bounds off every
    /// record's timestamp, so it cannot tell an inclusive backend from an
    /// exclusive one — this doc comment is the only place the choice is
    /// pinned down.
    ///
    /// **Truncation is detectable from the page size, so there is no
    /// `truncated` flag.** An implementation must return exactly
    /// `min(filter.limit, rows still matching after the cursor)` — never
    /// fewer for an internal batch size, a statement timeout, or a partial
    /// read, and never more. Given that, `returned.len() < filter.limit`
    /// means the log is exhausted, and a caller pages by re-issuing with
    /// `after` set to the last returned row's `AuditId` until a short page
    /// arrives. A flag on the result would be a second encoding of a fact the
    /// cursor already carries, and the two could disagree.
    ///
    /// This is the resolution of a `NOTE` that stood in
    /// `AuditFilter`: `limit` defaults to 100, so a compliance query built
    /// from `AuditFilter::default()` stops at 100 rows. It still does — but
    /// the caller can now tell, because a full page means "ask again", not
    /// "that was everything". Note that `filter.after` appears in no
    /// conformance test: the cursor is entirely untested, so this doc comment
    /// is the only thing pinning down both its direction and this rule.
    async fn audit(
        &self,
        scope: &Scope,
        filter: &AuditFilter,
    ) -> Result<Vec<AuditRecord>, BackendError>;

    /// Erases a subject within a tenant. This is the *subject* erasure API,
    /// and it is the one with a conformance test; tenant erasure is an
    /// out-of-band operator action with no method here — see the
    /// `aggregates` module doc for why, and for what survives.
    async fn purge_subject(
        &self,
        tenant: &TenantId,
        subject: &SubjectId,
    ) -> Result<PurgeReport, BackendError>;

    /// Every audit aggregate held for `tenant` that matches `filter`.
    ///
    /// Mirrors `audit`'s shape rather than inventing a second idiom for a
    /// bounded, resumable read: `filter` carries `since`/`until` **day**
    /// bounds, an optional `policy` narrowing, a `limit`, and an `after`
    /// cursor. The feature's primary query is a day range against one policy
    /// version — "how did behaviour change across policy 1.4" — which the
    /// original `audit_aggregates(&self, tenant)`, unbounded, unpaginated and
    /// unfilterable, could not express at all.
    ///
    /// Aggregates are keyed by tenant + policy version + event class + day
    /// bucket, and by nothing finer — see the `aggregates` module doc, which
    /// carries the whole argument, the cost it accepts, and the residual it
    /// leaves. The consequence relevant to an implementer is this: **a
    /// cascading `purge_subject` must not delete aggregate rows.** They name
    /// no subject and no namespace, so there is nothing in them for the purge
    /// to be erasing; a backend that stores them in, or cascades them from,
    /// the audit detail table destroys the one artifact that was designed to
    /// outlive the detail.
    ///
    /// **Ordering.** Rows are ordered ascending by the full key — `day`, then
    /// `policy` (`None` sorts before every `Some`, and two `Some`s compare by
    /// `to_string()`), then the event's serialised snake_case name — so
    /// resumption via `after` is over a total order, not merely a mostly-total
    /// one. Stating the order costs nothing now and stops the same
    /// cross-backend drift `list` and `audit` each had to have pinned down
    /// after the fact.
    ///
    /// **The cursor.** `filter.after`, when set, continues a previous page:
    /// the next page is restricted to keys strictly greater than it in the
    /// order above. `AggregateKey` is a value comparable without existing —
    /// it names a coordinate in the key space, not a pointer to a stored row.
    /// That is precisely what makes it safe as a cursor here: it resumes
    /// correctly even when the row it names has since expired under
    /// `AuditRetention::aggregate`, whereas a row-id cursor would name
    /// nothing once its row was gone and would make the log look exhausted
    /// for a reason unrelated to the caller's query.
    ///
    /// **Truncation is detectable from the page size, exactly as `audit`
    /// documents — there is no `truncated` flag.** An implementation must
    /// return exactly `min(filter.limit, rows still matching after the
    /// cursor)` — never fewer for an internal batch size and never more.
    /// `returned.len() < filter.limit` means exhausted.
    ///
    /// Rows are produced by the write paths (Tasks 20 and 23) and expired by
    /// retention (Task 36). Neither is this method's business.
    async fn audit_aggregates(
        &self,
        tenant: &TenantId,
        filter: &AuditAggregateFilter,
    ) -> Result<Vec<AuditAggregate>, BackendError>;

    /// `Header` first, then `Item`s ascending by `ItemId`, then `Audit`
    /// rows ascending by `AuditId`. `ExportStream` carries no relevance and
    /// no single id type, so neither the retrieval tie-break nor `list`'s
    /// rule applies here — the key differs per record kind instead. Two
    /// backends exporting the same tenant with a different (or absent)
    /// order produce byte-different artifacts for identical data, so
    /// checksums do not match and a customer verifying a migration cannot.
    /// `export_import_round_trips_exactly` never inspects the export stream
    /// itself — it compares `list` output after re-sorting both sides by
    /// `ItemId`, so no conformance test observes the stream's order at all —
    /// this doc comment is the only thing pinning it down.
    async fn export(&self, sel: &ScopeSelector) -> Result<ExportStream, BackendError>;

    /// Imports `stream` into `destination`. The tenant is a parameter, not a
    /// property of the payload: an authorising layer must be able to decide
    /// where an import lands without parsing the blob, and a later plan puts
    /// tenant-scoped API keys in front of this engine.
    ///
    /// **The disagreement rule.** `destination` is compared against **every
    /// record's own tenant**, one by one. No record's tenant is authority for
    /// any other record's. A record whose tenant disagrees **rejects the
    /// whole import** with `MalformedImport` — it is never silently
    /// retargeted, because retargeting only moves the differential from "the
    /// caller does not know where this landed" to "the caller does not know
    /// what this became". The payload's `subject` and `namespace` are
    /// preserved exactly as written; only the tenant is checked, and it is
    /// checked rather than assigned.
    ///
    /// **Atomicity.** `import` is all-or-nothing: either every record in
    /// `stream` is applied or none are. A conformance test already requires
    /// this — no partial import survives a rejected stream — but until now
    /// nothing said so; the trait doc stated only that a disagreement
    /// "rejects the whole import", which reads as a property of that one
    /// rejection path rather than of `import` itself. This paragraph is the
    /// general statement: a validation failure (a disagreeing tenant, a
    /// missing or unsupported `Header`, a malformed record) must leave the
    /// destination exactly as it was before the call. What this does **not**
    /// cover is a mid-stream storage error on an otherwise-valid stream —
    /// whether such a backend leaves partial writes behind is
    /// under-determined by this contract; it must not silently report
    /// success over a partial result, but the specific guarantee in that
    /// case is out of scope here.
    ///
    /// Because every record is compared against `destination`, records
    /// cannot disagree with *each other* either — a separate "the stream may
    /// not span tenants" check is strictly implied by this one and must not
    /// be written. A second rule that is true only by implication has no test
    /// of its own, cannot fail today, and silently stops being implied the
    /// day someone weakens the first.
    ///
    /// **Audit rows follow from the same comparison, not from a rule of their
    /// own.** `ExportRecord::Audit` carries an `AuditRecord` whose `scope`
    /// has a tenant. Same tenant as `destination` → the row is preserved
    /// byte-exact. Different → the import is rejected, exactly as for an
    /// item. A scope is never rewritten to make a row fit: a rewritten audit
    /// row is a forged one.
    ///
    /// **Why this is not a `Scope` or a `ScopeSelector`.** Tenant is the
    /// authorisation unit and the isolation unit, and taking a `TenantId`
    /// makes the disagreement rule total. A `Scope` would force an answer to
    /// "what happens when only the subject differs?", reintroducing the
    /// ambiguity the parameter exists to remove — in a path where the wrong
    /// answer is a cross-subject write. A `ScopeSelector`'s optional fields
    /// would make "unset" mean something, and every available meaning for it
    /// is a silent retarget.
    ///
    /// **Why this is not symmetric with `export`, and must not be
    /// harmonised.** `export` takes a `ScopeSelector` because it *narrows a
    /// read* within one tenant; `import` takes a `TenantId` because it
    /// *authorises a write* into one. Different questions, different types.
    /// Harmonising in either direction reintroduces a defect: giving
    /// `export` a bare tenant removes the subject/namespace narrowing a
    /// partial export needs, and giving `import` a selector puts the two
    /// optional fields back.
    ///
    /// Order-tolerant: records may arrive in any order. This is deliberate
    /// — requiring exactly one `Header` would reject the concatenation of
    /// two exports, which migration tooling actually does, so the rule is
    /// "at least one".
    ///
    /// At least one `Header` must be present, and every `Header` present
    /// must carry `portability::FORMAT_VERSION`. **This is a contract Task
    /// 24 must implement — today's code does not enforce it.** The draft
    /// `import` checks `format_version` only inside the `Header` match arm,
    /// so a stream that omits a `Header` entirely never reaches that check
    /// and is accepted with no version check at all, making the format
    /// version optional by omission.
    ///
    /// A stream of a `Header` and nothing else is **valid** and imports
    /// nothing: `export` of an empty tenant produces exactly that, and the
    /// round trip has to survive it. With the destination supplied as a
    /// parameter there is no longer anything to derive from the first item,
    /// so there is no reason left to reject it.
    async fn import(
        &self,
        destination: &TenantId,
        stream: ImportStream,
    ) -> Result<ImportReport, BackendError>;

    /// Set a namespace's budget. Used by the conformance suite and by admin APIs.
    async fn set_budget(
        &self,
        scope: &Scope,
        budget: memorysafe_core::Budget,
    ) -> Result<(), BackendError>;
}
