use anyhow::{Context, Result, bail};
use memorysafe_auth::ApiKeyRecord;
use memorysafe_engine::RetentionProfile;
use memorysafe_policy::BaselineConfig;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_CONFIG_FILE: &str = "msafe.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MsafeConfig {
    /// Where the per-tenant SQLite files live. Relative paths resolve against
    /// the directory holding the config file, so a checked-in config means the
    /// same thing on every machine.
    pub data_dir: PathBuf,
    pub tenant: Option<String>,
    pub subject: Option<String>,
    pub namespace: Option<String>,
    /// `deterministic` (no model files, the default) or `model2vec`.
    pub embedder: String,
    pub embedding_dim: u16,
    /// Filesystem path to a downloaded model2vec model directory. Only
    /// consulted when `embedder = "model2vec"` and the crate is built with
    /// the `model2vec` feature; `memorysafe_embed::Model2VecEmbedder` has no
    /// bundled default model to fall back to (see `build::embedder`).
    pub model_path: Option<PathBuf>,
    pub retention: String,
    pub policy: BaselineConfig,
    #[serde(rename = "keys")]
    pub keys: Vec<ApiKeyRecord>,
    pub serve: ServeConfig,
    /// Filled in by `load`; never read from the file.
    #[serde(skip)]
    pub root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServeConfig {
    pub bind: String,
    pub mcp_path: String,
    /// Hostnames the streamable-HTTP MCP transport will answer for. Loopback
    /// only by default: accepting any `Host` is a DNS-rebinding hole, and a
    /// deployment behind a real name should have to say the name.
    pub allowed_hosts: Vec<String>,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8080".into(),
            mcp_path: "/mcp".into(),
            allowed_hosts: vec!["localhost".into(), "127.0.0.1".into()],
        }
    }
}

impl Default for MsafeConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from(".msafe/tenants"),
            tenant: None,
            subject: None,
            namespace: None,
            embedder: "deterministic".into(),
            embedding_dim: 256,
            model_path: None,
            retention: RetentionProfile::default().name().to_owned(),
            policy: BaselineConfig::default(),
            keys: Vec::new(),
            serve: ServeConfig::default(),
            root: PathBuf::from("."),
        }
    }
}

impl MsafeConfig {
    /// Explicit path, else `./msafe.toml`, else defaults. A missing file is not
    /// an error — `msafe remember` in an empty directory should work.
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let path = match explicit {
            Some(p) => Some(p.to_path_buf()),
            None => {
                let candidate = PathBuf::from(DEFAULT_CONFIG_FILE);
                candidate.exists().then_some(candidate)
            }
        };

        let Some(path) = path else {
            return Ok(Self::default());
        };

        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut config: MsafeConfig =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        config.root = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        if RetentionProfile::from_name(&config.retention).is_none() {
            bail!(
                "unknown retention profile '{}'; expected balanced, gdpr_strict, hipaa_retain, or forensic",
                config.retention
            );
        }
        Ok(config)
    }

    pub fn data_dir(&self) -> PathBuf {
        if self.data_dir.is_absolute() {
            self.data_dir.clone()
        } else {
            self.root.join(&self.data_dir)
        }
    }

    pub fn retention_profile(&self) -> RetentionProfile {
        RetentionProfile::from_name(&self.retention).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serialising configuration")?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }
}
