use crate::{EmbedError, Embedder};
use memorysafe_core::{EmbedderId, Embedding};

/// A bag-of-tokens hash embedder. Not semantic in any real sense, but
/// deterministic, dependency-free, and monotone in token overlap — which is
/// exactly what the test suite needs and nothing more.
pub struct DeterministicEmbedder {
    dim: u16,
    id: EmbedderId,
}

impl DeterministicEmbedder {
    /// Create a new deterministic embedder with the given dimension.
    ///
    /// # Panics
    ///
    /// Panics if `dim == 0`.
    pub fn new(dim: u16) -> Self {
        assert!(dim > 0, "dim must be positive");
        Self {
            dim,
            id: EmbedderId::new(&format!("deterministic-{dim}")),
        }
    }
}

impl Default for DeterministicEmbedder {
    fn default() -> Self {
        Self::new(256)
    }
}

impl Embedder for DeterministicEmbedder {
    fn id(&self) -> EmbedderId {
        self.id.clone()
    }

    fn dim(&self) -> u16 {
        self.dim
    }

    fn embed(&self, text: &str) -> Result<Embedding, EmbedError> {
        let tokens: Vec<&str> = text.split_whitespace().collect();
        if tokens.is_empty() {
            return Err(EmbedError::EmptyInput);
        }

        let n = self.dim as usize;
        let mut v = vec![0.0f32; n];

        for token in tokens {
            let lowered = token.to_lowercase();
            let hash = blake3::hash(lowered.as_bytes());
            let bytes = hash.as_bytes();
            // THREE independent probes per token, not one. With a single probe
            // at dim=256, two unrelated words collide into identical unit
            // vectors about once per 430 pairs — measured: `mat` and `river`
            // score cosine 1.0 with no shared tokens, and 21 of 9,316 common
            // word pairs are byte-identical. Requiring all three probes to
            // coincide drops that to roughly one in dim^3.
            for probe in 0..3 {
                let off = probe * 5;
                let index = u32::from_le_bytes([
                    bytes[off],
                    bytes[off + 1],
                    bytes[off + 2],
                    bytes[off + 3],
                ]) as usize
                    % n;
                let sign = if bytes[off + 4] & 1 == 0 { 1.0 } else { -1.0 };
                v[index] += sign;
            }
        }

        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        } else {
            // Every contribution cancelled. Derive the vector from the WHOLE
            // text rather than pinning a constant: a fixed fallback makes every
            // document that reaches this branch identical to every other one,
            // so `the repo` and `spoon valley` would score cosine 1.0. Two-word
            // inputs reach it readily — `the been`, `is long`, `it every`.
            let whole = blake3::hash(text.as_bytes());
            let hb = whole.as_bytes();
            for (i, slot) in v.iter_mut().enumerate() {
                *slot = (f32::from(hb[i % 32]) - 127.5) / 127.5;
            }
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in &mut v {
                    *x /= norm;
                }
            } else {
                v[0] = 1.0;
            }
        }

        Ok(Embedding::new(v, self.id()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeddings_are_deterministic_across_instances() {
        let a = DeterministicEmbedder::new(256);
        let b = DeterministicEmbedder::new(256);
        assert_eq!(
            a.embed("hello").unwrap().vector,
            b.embed("hello").unwrap().vector
        );
    }

    #[test]
    fn different_text_yields_different_vectors() {
        let e = DeterministicEmbedder::new(256);
        assert_ne!(
            e.embed("hello").unwrap().vector,
            e.embed("world").unwrap().vector
        );
    }

    #[test]
    fn vectors_are_l2_normalised() {
        let e = DeterministicEmbedder::new(256);
        let v = e.embed("some memory about cats").unwrap().vector;
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm was {norm}");
    }

    #[test]
    fn shared_tokens_produce_higher_similarity_than_disjoint_ones() {
        // Retrieval tests depend on this being a usable, if crude, semantic proxy.
        let e = DeterministicEmbedder::new(256);
        let cats1 = e.embed("the cat sat on the mat").unwrap();
        let cats2 = e.embed("the cat sat on a rug").unwrap();
        let cars = e.embed("quarterly revenue exceeded projections").unwrap();
        let sim = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
        let near = sim(&cats1.vector, &cats2.vector);
        let far = sim(&cats1.vector, &cars.vector);
        assert!(near > far, "near={near} far={far}");
    }

    #[test]
    fn empty_text_is_an_error_not_a_zero_vector() {
        let e = DeterministicEmbedder::new(256);
        assert!(matches!(e.embed(""), Err(EmbedError::EmptyInput)));
        assert!(matches!(e.embed("   "), Err(EmbedError::EmptyInput)));
    }

    #[test]
    fn the_embedder_reports_its_identity_and_dimension() {
        let e = DeterministicEmbedder::new(384);
        assert_eq!(e.dim(), 384);
        assert_eq!(e.id().as_str(), "deterministic-384");
        assert_eq!(e.embed("x").unwrap().embedder, e.id());
    }

    #[test]
    fn distinct_single_tokens_do_not_produce_identical_vectors() {
        // With one probe per token this failed outright: `mat` and `river`
        // scored cosine 1.0, and 21 of 9,316 common word pairs were identical.
        // Downstream conformance tests use short bodies, so a collision here
        // makes "did retrieval find the right item" vacuous.
        let e = DeterministicEmbedder::new(256);
        let words = [
            "mat",
            "river",
            "cat",
            "dog",
            "the",
            "repo",
            "spoon",
            "valley",
            "memory",
            "audit",
            "policy",
            "tenant",
            "subject",
            "namespace",
            "to",
            "was",
            "it",
            "there",
            "first",
            "at",
            "new",
            "one",
            "which",
        ];
        let vecs: Vec<Vec<f32>> = words.iter().map(|w| e.embed(w).unwrap().vector).collect();
        for i in 0..words.len() {
            for j in (i + 1)..words.len() {
                let sim: f32 = vecs[i].iter().zip(&vecs[j]).map(|(a, b)| a * b).sum();
                assert!(
                    sim < 0.99,
                    "{} and {} collided at cosine {sim}",
                    words[i],
                    words[j]
                );
            }
        }
    }

    #[test]
    fn documents_whose_tokens_cancel_do_not_collapse_together() {
        // The all-cancel branch used to pin a constant vector, so every
        // document reaching it became identical to every other.
        //
        // At dim=256 with three probes per token, driving the accumulator to
        // exactly zero is vanishingly rare -- a brute-force sweep of common
        // two-word inputs found none. Cancellation gets far more likely as
        // dim shrinks, so this test uses dim=5, where it is not merely
        // plausible but confirmed: a standalone reproduction of the
        // three-probe algorithm shows the raw (pre-normalisation)
        // accumulator for both "dog it" and "at river" is exactly
        // [0.0, 0.0, 0.0, 0.0, 0.0] at dim=5, so `embed` genuinely takes the
        // fallback branch for both, not just one.
        let e = DeterministicEmbedder::new(5);
        let a = e.embed("dog it").unwrap();
        let b = e.embed("at river").unwrap();
        let sim: f32 = a.vector.iter().zip(&b.vector).map(|(x, y)| x * y).sum();
        assert!(sim < 0.99, "disjoint documents collapsed together at {sim}");

        // Still unit length whichever branch produced them.
        for v in [&a, &b] {
            let norm: f32 = v.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!(
                (norm - 1.0).abs() < 1e-5,
                "fallback vector was not normalised: {norm}"
            );
        }

        // And still deterministic.
        assert_eq!(e.embed("dog it").unwrap().vector, a.vector);
    }
}
