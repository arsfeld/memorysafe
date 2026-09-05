//! Embedding generation, int8 quantization, and vectors.

pub mod quantize;
pub mod test_embedder;

use memorysafe_core::{EmbedderId, Embedding};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EmbedError {
    #[error("cannot embed empty or whitespace-only text")]
    EmptyInput,
    #[error("embedding model unavailable: {0}")]
    Unavailable(String),
    #[error("model returned {got} dimensions, expected {expected}")]
    DimensionMismatch { got: usize, expected: u16 },
}

pub trait Embedder: Send + Sync {
    fn id(&self) -> EmbedderId;
    fn dim(&self) -> u16;
    fn embed(&self, text: &str) -> Result<Embedding, EmbedError>;
}

pub use quantize::{QuantizeError, QuantizedVector};
pub use test_embedder::DeterministicEmbedder;
