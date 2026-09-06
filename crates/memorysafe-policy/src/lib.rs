//! `BaselinePolicy` — the open-source governance policy.
//!
//! Simple and documented, not deliberately crippled. The proprietary policy
//! earns its keep with learned scorers and cross-tenant calibration, not by
//! this one being bad.

pub mod admit;
pub mod compose;
pub mod config;
pub mod eviction;
pub mod fragility;
pub mod maintain;
pub mod redundancy;
pub mod sensitivity;
pub mod similarity;
pub mod value;

#[cfg(test)]
pub(crate) mod testkit;

pub use config::{BaselineConfig, Verdict};

use memorysafe_core::{
    AdmitContext, AssessContext, Assessed, Assessment, AssessorId, Candidate, ComposeContext,
    Decision, GovernancePolicy, MaintainContext, PolicyError, PolicyId, RecallRequest,
    ScoredCandidate, WorkingSet, features,
};

pub const BASELINE_VERSION: &str = "0.1.0";

pub struct BaselinePolicy {
    pub config: BaselineConfig,
}

impl BaselinePolicy {
    pub fn new(config: BaselineConfig) -> Self {
        Self { config }
    }

    pub fn policy_id(&self) -> PolicyId {
        PolicyId::new("baseline", BASELINE_VERSION)
    }
}

impl Default for BaselinePolicy {
    fn default() -> Self {
        Self::new(BaselineConfig::default())
    }
}

impl GovernancePolicy for BaselinePolicy {
    fn id(&self) -> PolicyId {
        self.policy_id()
    }

    fn assess(&self, cand: &Candidate, ctx: &AssessContext) -> Result<Assessment, PolicyError> {
        let redundancy = redundancy::assess(&ctx.neighbours, &self.config);
        let fragility = fragility::score(&ctx.neighbours, &ctx.stats);
        let value = value::score(cand, &ctx.stats, &self.config);
        let sensitivity = sensitivity::assess(cand, &self.config);

        Ok(Assessment {
            features: features! {
                "neighbour_count" => ctx.neighbours.len() as f64,
                "best_similarity" => redundancy.score.get(),
                "corpus_mean_similarity" => ctx.stats.mean_neighbour_similarity,
                "corpus_item_count" => ctx.stats.item_count as f64,
                "byte_size" => cand.byte_size as f64,
                "has_embedding" => if cand.embedding.is_some() { 1.0 } else { 0.0 },
            },
            value,
            fragility,
            sensitivity,
            redundancy,
            assessor: AssessorId::new("baseline", BASELINE_VERSION),
        })
    }

    fn admit(&self, assessed: &Assessed, ctx: &AdmitContext) -> Result<Decision, PolicyError> {
        Ok(admit::decide(assessed, ctx, &self.config, self.policy_id()))
    }
    fn compose(
        &self,
        req: &RecallRequest,
        candidates: &[ScoredCandidate],
        ctx: &ComposeContext,
    ) -> Result<WorkingSet, PolicyError> {
        Ok(compose::working_set(req, candidates, ctx, &self.config))
    }
    fn maintain(&self, ctx: &MaintainContext) -> Result<Vec<Decision>, PolicyError> {
        Ok(maintain::decisions(ctx, &self.config, self.policy_id()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{candidate, scope};
    use memorysafe_core::{
        Action, Budget, CapacityState, RecallBudget, RecallMode, ScopeStats, Score,
        SensitivityLevel,
    };
    use time::{Duration, OffsetDateTime};

    #[test]
    fn compose_delegates_to_the_compose_module_with_the_policys_own_config() {
        // Rejects: a `compose` trait method that ignores its inputs (e.g.
        // `Ok(WorkingSet::empty())`) — nothing else in this package ever
        // calls `BaselinePolicy::compose`, so before this test existed that
        // stub-like implementation left all other tests, and mutation
        // testing's own baseline, passing. Also rejects a `compose` that
        // builds its own default config instead of forwarding `self.config`:
        // `cfg` below is a non-default config (`replay_quota: 0.5`), chosen
        // so a hardcoded `BaselineConfig::default()` inside the trait method
        // would reserve a different number of replay slots and diverge from
        // the direct call.
        // Vacuous if the direct call to `compose::working_set` and the call
        // through the trait used separately-constructed inputs that merely
        // happened to match — `req`, `candidates`, and `compose_ctx` are each
        // built once and shared by reference into both calls below.
        let cfg = BaselineConfig {
            replay_quota: 0.5,
            ..BaselineConfig::default()
        };
        let policy = BaselinePolicy::new(cfg.clone());

        let mut relevant = candidate("a relevant memory", 0.9);
        relevant.fragility = Score::ZERO;
        let mut stale = candidate("a rare fact nobody has read in a year", 0.05);
        stale.fragility = Score::ONE;
        stale.item.created_at = OffsetDateTime::UNIX_EPOCH;
        let candidates = vec![relevant, stale];

        let req = RecallRequest {
            scope: scope(),
            query: Some("anything".into()),
            tags_any: vec![],
            kinds: vec![],
            occurred_after: None,
            occurred_before: None,
            mode: RecallMode::WorkingSet,
            budget: RecallBudget {
                max_tokens: Some(10_000),
                max_items: Some(2),
            },
            sensitivity_ceiling: SensitivityLevel::Restricted,
        };
        let compose_ctx = ComposeContext {
            scope: scope(),
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH + Duration::days(365),
        };

        let via_trait = policy
            .compose(&req, &candidates, &compose_ctx)
            .expect("compose must not error on well-formed input");
        let direct = compose::working_set(&req, &candidates, &compose_ctx, &cfg);

        assert_eq!(via_trait, direct);
        // Not vacuously equal empty results either way: the fixture is built
        // so the stale candidate wins the reserved replay slot.
        assert!(!via_trait.items.is_empty());
        assert!(
            via_trait
                .items
                .iter()
                .any(|s| s.reason.code == memorysafe_core::ReasonCode::ReplayDue)
        );
    }
    #[test]
    fn maintain_delegates_to_the_maintain_module_with_the_policys_own_config() {
        // The same hole `compose_delegates_...` above exists to close, in the
        // one remaining trait method: nothing else in this package calls
        // `BaselinePolicy::maintain`, so a stub returning `Ok(vec![])` would
        // leave every other test in the workspace — and mutation testing's own
        // baseline — passing.
        //
        // `cfg` is a non-default config (`merge_threshold: 0.5`) and the
        // fixture's coverage is exactly 0.5, so a trait method that built its
        // own `BaselineConfig::default()` instead of forwarding `self.config`
        // would decline the merge and return an empty vector while the direct
        // call returned one. That is also why the non-emptiness assertion
        // below is load-bearing rather than decorative: `assert_eq!` alone is
        // satisfied by two empty vectors.
        let cfg = BaselineConfig {
            merge_threshold: 0.5,
            ..BaselineConfig::default()
        };
        let policy = BaselinePolicy::new(cfg.clone());
        let now = OffsetDateTime::UNIX_EPOCH + Duration::days(365);

        // "alpha beta gamma delta" and "alpha beta epsilon zeta" share 2 of
        // each side's 4 distinct tokens: coverage 0.5 in both directions.
        let mut older = crate::testkit::maintenance_candidate("alpha beta gamma delta", 0.5, 0.5);
        older.item.created_at = now - Duration::days(200);
        let mut newer = crate::testkit::maintenance_candidate("alpha beta epsilon zeta", 0.5, 0.5);
        newer.item.created_at = now - Duration::days(100);

        let ctx = MaintainContext {
            scope: scope(),
            batch: vec![older, newer],
            is_final_batch: true,
            capacity: CapacityState {
                budget: Budget::UNBOUNDED,
                used_items: 2,
                used_bytes: 0,
            },
            stats: ScopeStats {
                item_count: 100,
                mean_neighbour_similarity: 0.4,
                ..Default::default()
            },
            now,
        };

        let via_trait = policy
            .maintain(&ctx)
            .expect("maintain must not error on well-formed input");
        let direct = maintain::decisions(&ctx, &cfg, policy.policy_id());

        assert_eq!(via_trait, direct);
        assert!(
            via_trait
                .iter()
                .any(|d| matches!(d.action, Action::Merge { .. })),
            "the fixture is built to produce a merge under this config; \
             an empty result means the config did not reach the module"
        );
    }
}
