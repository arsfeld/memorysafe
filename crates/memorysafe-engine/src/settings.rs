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

    // `admit::decide` computes `ctx.now + Duration::days(cfg.protection_window_days)`
    // unchecked (`memorysafe-policy`'s `admit.rs`). `time::OffsetDateTime`'s
    // component range tops out around the year 9999 — a `protection_window_days`
    // large enough to walk `ctx.now` past that panics inside the policy call.
    // `validate::call_policy` catches that panic and the engine degrades to
    // `fallback_policy` under `FailSafe` rather than crashing the process, so
    // this is not the difference between "up" and "down" — but a single admin
    // `PUT .../policy` could otherwise silently move a tenant's entire write
    // path onto the untuned fallback the moment a fragile item is admitted.
    // 36,500 days (100 years) is far beyond any legitimate retention window
    // (the documented default is 30) and leaves `ctx.now` nowhere near the
    // year-9999 boundary.
    if !(0..=36_500).contains(&cfg.protection_window_days) {
        return Err(EngineError::Validation(format!(
            "protection_window_days must be between 0 and 36500, got {}",
            cfg.protection_window_days
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_config_validates() {
        assert!(validate(&BaselineConfig::default()).is_ok());
    }

    /// Pins the bound that closes the `protection_window_days` overflow gap
    /// (see this function's own comment above): a config an admin could PUT
    /// over HTTP must not be able to walk `admit::decide`'s
    /// `ctx.now + Duration::days(cfg.protection_window_days)` past
    /// `OffsetDateTime`'s representable range.
    #[test]
    fn a_protection_window_large_enough_to_risk_a_date_overflow_is_rejected() {
        let cfg = BaselineConfig {
            protection_window_days: 10_000_000,
            ..Default::default()
        };
        let err = validate(&cfg).expect_err("an absurd protection window must be refused");
        assert!(
            err.to_string().contains("protection_window_days"),
            "wrong rejection reason: {err}"
        );
    }

    /// The bound also has a floor: `Duration::days` on a negative count
    /// backdates the window rather than merely doing nothing, so a negative
    /// value is rejected too, not silently accepted as "no protection".
    #[test]
    fn a_negative_protection_window_is_rejected() {
        let cfg = BaselineConfig {
            protection_window_days: -1,
            ..Default::default()
        };
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn a_generous_but_bounded_protection_window_is_accepted() {
        let cfg = BaselineConfig {
            protection_window_days: 36_500,
            ..Default::default()
        };
        assert!(validate(&cfg).is_ok());
    }
}
