use crate::ShadowError;
use memorysafe_core::{Budget, Scope, SensitivityLevel};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One write, exactly as a caller would have made it. `Scenario` is a file
/// format — a fixture lives on disk and is read by a test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioWrite {
    pub scope: Scope,
    pub body: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub sensitivity_hint: Option<SensitivityLevel>,
    #[serde(default)]
    pub ttl_seconds: Option<i64>,
}

fn default_kind() -> String {
    "fact".into()
}

/// A decision that was recorded but cannot be replayed, and why. Reported
/// rather than silently dropped: coverage is the number that says how much of
/// a real audit log a shadow run actually exercised.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unreplayable {
    pub audit_id: String,
    pub event: String,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    /// Fixed so a fixture means the same thing on every machine.
    #[serde(default = "default_dim")]
    pub embedder_dim: u16,
    /// Applied before the first write.
    #[serde(default)]
    pub budgets: Vec<(Scope, Budget)>,
    pub writes: Vec<ScenarioWrite>,
    #[serde(default)]
    pub unreplayable: Vec<Unreplayable>,
}

fn default_dim() -> u16 {
    256
}

impl Scenario {
    pub fn load(path: &Path) -> Result<Self, ShadowError> {
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }

    pub fn save(&self, path: &Path) -> Result<(), ShadowError> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// How much of the source material this scenario actually replays.
    pub fn coverage(&self) -> f32 {
        let total = self.writes.len() + self.unreplayable.len();
        if total == 0 {
            return 1.0;
        }
        self.writes.len() as f32 / total as f32
    }
}
