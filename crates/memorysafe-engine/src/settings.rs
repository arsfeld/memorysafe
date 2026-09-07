use crate::error::EngineError;
use crate::retention::RetentionProfile;
use memorysafe_policy::BaselineConfig;

/// Everything a tenant can configure. Returned as a snapshot so a caller never
/// holds a lock.
#[derive(Debug, Clone, PartialEq)]
pub struct TenantSettings {
    pub policy_config: BaselineConfig,
    pub retention: RetentionProfile,
}

/// Rejects configurations that are self-contradictory rather than merely
/// unusual. A tenant may tune thresholds; it may not install one that makes a
/// decision path unreachable, because the resulting audit trail would be
/// inexplicable.
pub fn validate(cfg: &BaselineConfig) -> Result<(), EngineError> {
    let unit = |name: &str, v: f32| -> Result<(), EngineError> {
        if !v.is_finite() || !(0.0..=1.0).contains(&v) {
            return Err(EngineError::Validation(format!(
                "{name} must be a finite value in [0.0, 1.0], got {v}"
            )));
        }
        Ok(())
    };
    unit("duplicate_threshold", cfg.duplicate_threshold)?;
    unit("merge_threshold", cfg.merge_threshold)?;
    unit("near_duplicate_floor", cfg.near_duplicate_floor)?;
    unit("replay_quota", cfg.replay_quota)?;
    unit("mmr_lambda", cfg.mmr_lambda)?;

    if cfg.merge_threshold > cfg.duplicate_threshold {
        return Err(EngineError::Validation(format!(
            "merge_threshold ({}) above duplicate_threshold ({}) makes Merge unreachable",
            cfg.merge_threshold, cfg.duplicate_threshold
        )));
    }
    if cfg.near_duplicate_floor > cfg.merge_threshold {
        return Err(EngineError::Validation(format!(
            "near_duplicate_floor ({}) above merge_threshold ({}) hides the neighbours a merge \
             decision would cite as evidence",
            cfg.near_duplicate_floor, cfg.merge_threshold
        )));
    }
    for (name, v) in [
        ("value_half_life_days", cfg.value_half_life_days),
        ("source_trust_weight", cfg.source_trust_weight),
        ("replay_stale_days", cfg.replay_stale_days),
    ] {
        if !v.is_finite() || v < 0.0 {
            return Err(EngineError::Validation(format!(
                "{name} must be finite and non-negative, got {v}"
            )));
        }
    }
    Ok(())
}
