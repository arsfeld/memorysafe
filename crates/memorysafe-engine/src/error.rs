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
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error("embedder failed: {0}")]
    Embedder(#[from] EmbedError),
    #[error("policy failed and the engine is configured to fail closed: {0}")]
    PolicyRefused(String),
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
