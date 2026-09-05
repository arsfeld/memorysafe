use memorysafe_core::{EmbedderId, Embedding};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum QuantizeError {
    #[error("vectors are not comparable: {left} != {right}")]
    NotComparable { left: String, right: String },
    #[error("byte length {got} does not match dim {dim}")]
    LengthMismatch { got: usize, dim: u16 },
}

/// Symmetric int8 quantization of an L2-normalised embedding. Because inputs
/// are unit vectors, the dot product of two quantized vectors approximates
/// their cosine similarity directly.
#[derive(Debug, Clone, PartialEq)]
pub struct QuantizedVector {
    pub embedder: EmbedderId,
    pub dim: u16,
    pub scale: f32,
    pub q: Vec<i8>,
}

impl QuantizedVector {
    pub fn from_embedding(e: &Embedding) -> Self {
        let max_abs = e.vector.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        // A zero vector has no scale; use 1.0 so quantization is a no-op and
        // every dot product involving it is exactly zero.
        let scale = if max_abs > 0.0 { max_abs / 127.0 } else { 1.0 };
        let q = e
            .vector
            .iter()
            .map(|x| (x / scale).round().clamp(-127.0, 127.0) as i8)
            .collect();
        Self {
            embedder: e.embedder.clone(),
            dim: e.dim,
            scale,
            q,
        }
    }

    pub fn dot(&self, other: &QuantizedVector) -> Result<f32, QuantizeError> {
        if self.embedder != other.embedder || self.dim != other.dim {
            return Err(QuantizeError::NotComparable {
                left: format!("{}:{}", self.embedder, self.dim),
                right: format!("{}:{}", other.embedder, other.dim),
            });
        }
        // i32 accumulator: 127*127*65535 fits comfortably. Chunked to help
        // the autovectorizer; no intrinsics, no unsafe.
        let mut acc: i32 = 0;
        let (a, b) = (&self.q, &other.q);
        let chunks = a.len() / 16;
        for i in 0..chunks {
            let (x, y) = (&a[i * 16..i * 16 + 16], &b[i * 16..i * 16 + 16]);
            for k in 0..16 {
                acc += i32::from(x[k]) * i32::from(y[k]);
            }
        }
        for i in chunks * 16..a.len() {
            acc += i32::from(a[i]) * i32::from(b[i]);
        }
        Ok(acc as f32 * self.scale * other.scale)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.q.iter().map(|v| *v as u8).collect()
    }

    pub fn from_bytes(
        embedder: EmbedderId,
        dim: u16,
        scale: f32,
        bytes: &[u8],
    ) -> Result<Self, QuantizeError> {
        if bytes.len() != dim as usize {
            return Err(QuantizeError::LengthMismatch {
                got: bytes.len(),
                dim,
            });
        }
        Ok(Self {
            embedder,
            dim,
            scale,
            q: bytes.iter().map(|b| *b as i8).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{QuantizeError, QuantizedVector};
    use crate::{DeterministicEmbedder, Embedder};

    #[test]
    fn quantized_dot_approximates_cosine_of_normalised_vectors() {
        let e = DeterministicEmbedder::new(256);
        let a = e.embed("the cat sat on the mat").unwrap();
        let b = e.embed("the cat sat on a rug").unwrap();
        let exact: f32 = a.vector.iter().zip(&b.vector).map(|(x, y)| x * y).sum();

        let qa = QuantizedVector::from_embedding(&a);
        let qb = QuantizedVector::from_embedding(&b);
        let approx = qa.dot(&qb).unwrap();

        assert!(
            (exact - approx).abs() < 0.02,
            "exact={exact} approx={approx}"
        );
    }

    #[test]
    fn identical_vectors_score_near_one() {
        let e = DeterministicEmbedder::new(256);
        let a = e.embed("identical text").unwrap();
        let q = QuantizedVector::from_embedding(&a);
        let d = q.dot(&q).unwrap();
        assert!((d - 1.0).abs() < 0.02, "self-similarity was {d}");
    }

    #[test]
    fn vectors_from_different_embedders_refuse_to_compare() {
        let a =
            QuantizedVector::from_embedding(&DeterministicEmbedder::new(256).embed("x").unwrap());
        let b =
            QuantizedVector::from_embedding(&DeterministicEmbedder::new(384).embed("x").unwrap());
        assert!(matches!(
            a.dot(&b),
            Err(QuantizeError::NotComparable { .. })
        ));
    }

    #[test]
    fn bytes_round_trip_exactly() {
        let e = DeterministicEmbedder::new(256);
        let q = QuantizedVector::from_embedding(&e.embed("round trip me").unwrap());
        let bytes = q.to_bytes();
        assert_eq!(bytes.len(), 256);
        let back = QuantizedVector::from_bytes(q.embedder.clone(), q.dim, q.scale, &bytes).unwrap();
        assert_eq!(back.q, q.q);
        assert_eq!(back.dot(&q).unwrap(), q.dot(&q).unwrap());
    }

    #[test]
    fn from_bytes_rejects_a_length_that_contradicts_dim() {
        let e = DeterministicEmbedder::new(256);
        let q = QuantizedVector::from_embedding(&e.embed("x").unwrap());
        let err = QuantizedVector::from_bytes(q.embedder.clone(), 256, q.scale, &[0u8; 10]);
        assert!(matches!(err, Err(QuantizeError::LengthMismatch { .. })));
    }

    #[test]
    fn an_all_zero_vector_quantizes_without_dividing_by_zero() {
        let z = memorysafe_core::Embedding {
            vector: vec![0.0; 8],
            embedder: memorysafe_core::EmbedderId::new("z"),
            dim: 8,
        };
        let q = QuantizedVector::from_embedding(&z);
        // A zero scale would be persisted to SQLite and make every vector reconstructed
        // from bytes dot to exactly 0.0 forever, a silent retrieval failure. The guard
        // ensures scale stays 1.0 (or at least nonzero), keeping the struct sane.
        assert_eq!(q.scale, 1.0, "scale must be 1.0 for zero vectors, not 0.0");
        assert_eq!(q.dot(&q).unwrap(), 0.0);
    }
}
