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
    async fn retrieve_candidates(
        &self,
        scope: &Scope,
        query: &CandidateQuery,
    ) -> Result<Vec<ScoredCandidate>, BackendError>;

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

    async fn export(&self, sel: &ScopeSelector) -> Result<ExportStream, BackendError>;

    async fn import(&self, stream: ImportStream) -> Result<ImportReport, BackendError>;

    /// Set a namespace's budget. Used by the conformance suite and by admin APIs.
    async fn set_budget(
        &self,
        scope: &Scope,
        budget: memorysafe_core::Budget,
    ) -> Result<(), BackendError>;
}
