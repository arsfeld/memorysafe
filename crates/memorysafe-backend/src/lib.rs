//! The storage seam. One trait covering persistence and retrieval, because
//! pgvector searches inside the database and a separate index trait would
//! bake the SQLite shape into the interface.

pub mod conformance;
pub mod portability;
pub mod query;
pub mod write;

use memorysafe_core::{
    AuditFilter, AuditId, AuditRecord, CapacityState, Embedding, ItemId, MemoryItem, Scope,
    ScopeStats, ScoredCandidate, SubjectId, TenantId,
};
use thiserror::Error;

pub use portability::{
    ExportRecord, ExportStream, ExportVector, ImportReport, ImportStream, ScopeSelector,
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
    async fn retrieve_candidates(
        &self,
        scope: &Scope,
        query: &CandidateQuery,
    ) -> Result<Vec<ScoredCandidate>, BackendError>;

    /// Ordered descending by `relevance`, ties broken by ascending `ItemId`
    /// — the same rule and reason as `retrieve_candidates`. Here the
    /// consequence is narrower but still real: the policy's `best()`
    /// neighbour — the merge target — becomes nondeterministic on tied
    /// similarity if two backends break ties differently.
    async fn neighbours(
        &self,
        scope: &Scope,
        embedding: &Embedding,
        k: usize,
    ) -> Result<Vec<ScoredCandidate>, BackendError>;

    async fn capacity_state(&self, scope: &Scope) -> Result<CapacityState, BackendError>;

    async fn scope_stats(&self, scope: &Scope) -> Result<ScopeStats, BackendError>;

    async fn apply(&self, txn: WriteTransaction) -> Result<AppliedWrite, BackendError>;

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
    async fn audit(
        &self,
        scope: &Scope,
        filter: &AuditFilter,
    ) -> Result<Vec<AuditRecord>, BackendError>;

    async fn purge_subject(
        &self,
        tenant: &TenantId,
        subject: &SubjectId,
    ) -> Result<PurgeReport, BackendError>;

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

    /// Order-tolerant: records may arrive in any order. This is deliberate
    /// — requiring exactly one `Header` would reject the concatenation of
    /// two exports, which migration tooling actually does, so the rule is
    /// "at least one".
    ///
    /// At least one `Header` must be present, and every `Header` present
    /// must carry a supported `format_version`. **This is a contract Task
    /// 24 must implement — today's code does not enforce it.** The draft
    /// `import` checks `format_version` only inside the `Header` match arm,
    /// so a stream that omits a `Header` entirely never reaches that check
    /// and is accepted with no version check at all, making the format
    /// version optional by omission.
    async fn import(&self, stream: ImportStream) -> Result<ImportReport, BackendError>;

    /// Set a namespace's budget. Used by the conformance suite and by admin APIs.
    async fn set_budget(
        &self,
        scope: &Scope,
        budget: memorysafe_core::Budget,
    ) -> Result<(), BackendError>;
}
