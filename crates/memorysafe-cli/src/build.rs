use crate::config::MsafeConfig;
use anyhow::{Context, Result, bail};
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_embed::{DeterministicEmbedder, Embedder};
use memorysafe_engine::{Engine, EngineConfig};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

/// The one place a backend, an embedder, and a policy become an engine.
pub fn build_engine(config: &MsafeConfig) -> Result<Arc<Engine>> {
    let data_dir = config.data_dir();
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;

    let embedder = embedder(config)?;
    let mut engine_config = EngineConfig::new(
        Arc::new(SqliteBackend::open(data_dir)),
        embedder,
        Arc::new(BaselinePolicy::new(config.policy.clone())),
    );
    engine_config.retention = config.retention_profile();
    Ok(Arc::new(Engine::new(engine_config)))
}

fn embedder(config: &MsafeConfig) -> Result<Arc<dyn Embedder>> {
    match config.embedder.as_str() {
        "deterministic" => Ok(Arc::new(DeterministicEmbedder::new(config.embedding_dim))),
        // `Model2VecEmbedder` (Plan 1 Task 13) exposes no `load_default()` —
        // its only constructor is `from_pretrained(path, id)`, which needs a
        // downloaded model directory on disk. The brief anticipated this
        // mismatch and licensed the fix: use the real constructor and add a
        // `model_path` field to carry the path (`config.rs`).
        #[cfg(feature = "model2vec")]
        "model2vec" => {
            let path = config.model_path.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "embedder \"model2vec\" requires `model_path` to be set in msafe.toml"
                )
            })?;
            Ok(Arc::new(
                memorysafe_embed::Model2VecEmbedder::from_pretrained(path, "model2vec")
                    .context("loading the model2vec embedder")?,
            ))
        }
        #[cfg(not(feature = "model2vec"))]
        "model2vec" => bail!(
            "this build has no model2vec support; rebuild with `--features model2vec` or set \
             embedder = \"deterministic\""
        ),
        other => bail!("unknown embedder '{other}'; expected deterministic or model2vec"),
    }
}
