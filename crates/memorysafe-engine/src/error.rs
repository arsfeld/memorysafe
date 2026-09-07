use memorysafe_backend::BackendError;
use memorysafe_embed::EmbedError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    /// Only ever constructed holding a [`BackendError::Storage`] — see
    /// `From<BackendError>` below, which is the only place this variant is
    /// built. Every other `BackendError` kind is remapped to a more specific
    /// `EngineError` variant there instead of collapsing into this one.
    #[error(transparent)]
    Backend(BackendError),
    #[error("embedder failed: {0}")]
    Embedder(#[from] EmbedError),
    #[error("policy failed and the engine is configured to fail closed: {0}")]
    PolicyRefused(String),
}

/// Hand-written rather than `#[from]` on the `Backend` variant: a blanket
/// `#[from] BackendError` collapses every one of `BackendError`'s seven kinds
/// into `EngineError::Backend`, which every adapter answers as 503. §9 of the
/// spec (the adapters' error contract) is explicit that an idempotency
/// conflict is 409 and a malformed query is 400 — both real `BackendError`
/// kinds the backend already raises (`BackendError::IdempotencyConflict` on
/// `Backend::apply`'s ordinary write path; `BackendError::InvalidQuery` on
/// `Backend::retrieve_candidates`) — so the blanket conversion was silently
/// wrong for every kind except `Storage`, which really is "the backend itself
/// is unavailable" and really is the only kind an adapter should answer 503
/// (with a retry hint) for.
impl From<BackendError> for EngineError {
    fn from(e: BackendError) -> Self {
        // Message text is `BackendError`'s own `Display`, computed once
        // before `e` is matched (and, for the last arm, moved) below — every
        // arm's message is exactly what `e.to_string()` would have produced,
        // so no wording drifts from `BackendError`'s own `#[error(...)]`
        // strings.
        let message = e.to_string();
        match e {
            BackendError::IdempotencyConflict => EngineError::Conflict(message),
            BackendError::InvalidQuery(_)
            | BackendError::InvalidTransaction(_)
            | BackendError::MalformedImport(_)
            | BackendError::EmbedderMismatch { .. } => EngineError::Validation(message),
            BackendError::ItemNotFound(_) | BackendError::MergeTargetMissing(_) => {
                EngineError::NotFound(message)
            }
            // `storage @ ...`, not a bare `BackendError::Storage { .. }`:
            // the arm needs the whole matched value (to hand to
            // `EngineError::Backend`), not just its fields.
            storage @ BackendError::Storage { .. } => EngineError::Backend(storage),
        }
    }
}

impl EngineError {
    /// Whether a caller should retry. Surfaced by adapters as a 503 hint.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            EngineError::Backend(BackendError::Storage {
                retryable: true,
                ..
            })
        )
    }
}
