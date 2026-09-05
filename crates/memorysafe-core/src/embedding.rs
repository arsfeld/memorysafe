use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EmbedderId(String);

impl EmbedderId {
    pub fn new(raw: &str) -> Self {
        Self(raw.to_owned())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EmbedderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Embedding {
    pub vector: Vec<f32>,
    pub embedder: EmbedderId,
    pub dim: u16,
}

impl Embedding {
    /// Derives `dim` from the vector rather than accepting it separately, so
    /// the two cannot disagree.
    pub fn new(vector: Vec<f32>, embedder: EmbedderId) -> Self {
        let dim = u16::try_from(vector.len()).unwrap_or(u16::MAX);
        Self {
            vector,
            embedder,
            dim,
        }
    }

    /// Vectors from different models occupy different spaces. Comparing them
    /// produces silently meaningless similarities, so every comparison site
    /// must gate on this.
    ///
    /// Compares the actual vector lengths as well as the declared `dim`. The
    /// fields are public and `Deserialize`d, so `dim` can lie about the vector
    /// it describes — and a `dim` that agrees while the vectors differ in
    /// length is exactly the silent-nonsense case this guard exists to stop.
    pub fn is_comparable_to(&self, other: &Embedding) -> bool {
        self.embedder == other.embedder
            && self.dim == other.dim
            && self.vector.len() == other.vector.len()
            && self.vector.len() == self.dim as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emb(id: &str, dim: u16) -> Embedding {
        Embedding {
            vector: vec![0.0; dim as usize],
            embedder: EmbedderId::new(id),
            dim,
        }
    }

    #[test]
    fn vectors_from_different_embedders_are_not_comparable() {
        // Same embedder, same dim — should be comparable
        assert!(emb("model2vec-base", 256).is_comparable_to(&emb("model2vec-base", 256)));
        // Different embedder, same dim — should NOT be comparable
        assert!(!emb("model2vec-base", 256).is_comparable_to(&emb("nomic-v1.5", 256)));
        // Same embedder, different dim — should NOT be comparable
        assert!(!emb("model2vec-base", 256).is_comparable_to(&emb("model2vec-base", 384)));
    }

    #[test]
    fn a_dim_that_lies_about_its_vector_is_not_comparable() {
        // Fields are public and deserialized, so `dim` can disagree with the
        // vector it describes. Two such embeddings must not compare equal just
        // because their declared dims match.
        let honest = Embedding::new(vec![0.0; 4], EmbedderId::new("m"));
        assert_eq!(honest.dim, 4);
        assert!(honest.is_comparable_to(&honest));

        let liar = Embedding {
            vector: vec![0.0; 2],
            embedder: EmbedderId::new("m"),
            dim: 4,
        };
        assert!(!liar.is_comparable_to(&honest), "a lying dim was accepted");
        assert!(!honest.is_comparable_to(&liar));
        assert!(
            !liar.is_comparable_to(&liar),
            "self-comparison hid the inconsistency"
        );
    }
}
