use crate::{EmbedError, Embedder};
use memorysafe_core::{EmbedderId, Embedding};
use model2vec_rs::model::StaticModel;
use std::path::Path;

/// Static distilled embeddings. No ONNX runtime, no GPU, microsecond-scale
/// encoding — cheap enough to run inline on every write, which the
/// caller-authored-item design requires.
pub struct Model2VecEmbedder {
    model: StaticModel,
    id: EmbedderId,
    dim: u16,
}

impl Model2VecEmbedder {
    pub fn from_pretrained(path: &Path, id: &str) -> Result<Self, EmbedError> {
        if !path.exists() {
            return Err(EmbedError::Unavailable(format!(
                "no model at {}",
                path.display()
            )));
        }
        let model = StaticModel::from_pretrained(path, None, None, None)
            .map_err(|e| EmbedError::Unavailable(e.to_string()))?;
        let probe = model.encode_single("dimension probe");
        let dim = u16::try_from(probe.len())
            .map_err(|_| EmbedError::Unavailable("model dimension exceeds u16".into()))?;
        Ok(Self {
            model,
            id: EmbedderId::new(id),
            dim,
        })
    }
}

impl Embedder for Model2VecEmbedder {
    fn id(&self) -> EmbedderId {
        self.id.clone()
    }

    fn dim(&self) -> u16 {
        self.dim
    }

    fn embed(&self, text: &str) -> Result<Embedding, EmbedError> {
        if text.trim().is_empty() {
            return Err(EmbedError::EmptyInput);
        }
        let mut v = self.model.encode_single(text);
        if v.len() != self.dim as usize {
            return Err(EmbedError::DimensionMismatch {
                got: v.len(),
                expected: self.dim,
            });
        }
        // Quantization assumes unit vectors; normalise here so every Embedder
        // upholds the same contract.
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        } else {
            return Err(EmbedError::Unavailable(
                "model produced a zero vector".into(),
            ));
        }
        Ok(Embedding::new(v, self.id()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_model_path_is_unavailable_not_a_panic() {
        let err = Model2VecEmbedder::from_pretrained(
            std::path::Path::new("/nonexistent/model"),
            "potion-base-8M",
        );
        assert!(matches!(err, Err(EmbedError::Unavailable(_))));
    }

    #[test]
    #[ignore = "requires a downloaded model; run with `MEMORYSAFE_MODEL_PATH=<path> cargo test -p memorysafe-embed --features model2vec -- --ignored real_embeddings_are_normalised_and_semantic`"]
    fn real_embeddings_are_normalised_and_semantic() {
        let path = std::env::var("MEMORYSAFE_MODEL_PATH").expect("set MEMORYSAFE_MODEL_PATH");
        let e = Model2VecEmbedder::from_pretrained(std::path::Path::new(&path), "potion-base-8M")
            .unwrap();

        let v = e.embed("the cat sat on the mat").unwrap();
        let norm: f32 = v.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "norm was {norm}");
        assert_eq!(v.vector.len(), e.dim() as usize);

        let sim = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
        let cat = e.embed("a small domestic cat").unwrap();
        let kitten = e.embed("a young kitten").unwrap();
        let finance = e.embed("quarterly revenue exceeded projections").unwrap();
        assert!(sim(&cat.vector, &kitten.vector) > sim(&cat.vector, &finance.vector));
    }
}
