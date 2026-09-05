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
    /// Vectors from different models occupy different spaces. Comparing them
    /// produces silently meaningless similarities, so every comparison site
    /// must gate on this.
    pub fn is_comparable_to(&self, other: &Embedding) -> bool {
        self.embedder == other.embedder && self.dim == other.dim
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
}
