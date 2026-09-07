use crate::ShadowError;
use crate::scenario::Scenario;
use crate::trace::{Trace, TracedAction, TracedDecision, TracedProtection};
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Action, GovernancePolicy, ItemId, Protection};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use std::collections::HashMap;
use std::sync::Arc;
use time::Duration;

/// Drive a scenario through a real engine and record what the policy chose.
///
/// The engine is real — a temporary SQLite file and the deterministic embedder
/// — because the contexts a policy sees (neighbours, capacity, corpus
/// statistics) are produced by the engine's own I/O. Hand-building them would
/// be a second, drifting implementation of the write pipeline, and a shadow
/// result computed from one would not predict production.
pub async fn run(
    scenario: &Scenario,
    policy: Arc<dyn GovernancePolicy>,
) -> Result<Trace, ShadowError> {
    // Held for the whole run; dropped, and cleaned up, when it ends.
    let dir = tempfile::tempdir()?;
    let engine = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.path().to_path_buf())),
        Arc::new(DeterministicEmbedder::new(scenario.embedder_dim)),
        policy.clone(),
    ));

    for (scope, budget) in &scenario.budgets {
        engine.set_budget(scope, *budget).await?;
    }

    let mut id_to_seq: HashMap<ItemId, usize> = HashMap::new();
    let mut decisions = Vec::with_capacity(scenario.writes.len());

    for (seq, write) in scenario.writes.iter().enumerate() {
        let mut req = RememberRequest::new(write.scope.clone(), &write.body);
        req.kind = write.kind.clone();
        req.tags = write.tags.clone();
        req.sensitivity_hint = write.sensitivity_hint;
        req.ttl = write.ttl_seconds.map(Duration::seconds);

        let outcome = engine.remember(req).await?;

        // Read the stored item back rather than trusting `outcome` for
        // anything beyond `action`/`reasons`/`evicted`. Two things need it:
        // the sensitivity level actually landed at (`outcome` does not carry
        // it, and a policy that downgraded a credential would otherwise leave
        // no trace), and — for a `Retain` — the item's own `created_at`, the
        // reference point `TracedProtection::Protected`'s window is measured
        // from (see that type's doc for why the absolute deadline itself
        // cannot go in the trace).
        //
        // `outcome.item_id` on a `Merge` names the EXISTING target, not a
        // freshly created item — `memorysafe-backend-sqlite`'s `apply` sets
        // it to `m.target.clone()` on the merge arm. Fetching it is still
        // correct (it answers "what does the target look like now"), but it
        // must never be recorded into `id_to_seq` below as if it were new.
        let item = match &outcome.item_id {
            Some(id) => engine.get(&write.scope, id).await?,
            None => None,
        };
        let sensitivity = item.as_ref().map(|i| i.sensitivity);

        let action = match &outcome.action {
            Action::Retain { protection } => {
                let protection = match protection {
                    Protection::Normal => TracedProtection::Normal,
                    Protection::Pinned => TracedProtection::Pinned,
                    Protection::Protected { until } => {
                        let created_at = item
                            .as_ref()
                            .map(|i| i.created_at)
                            .expect("a retained item was just read back from its own scope");
                        TracedProtection::Protected {
                            window_seconds: (*until - created_at).whole_seconds(),
                        }
                    }
                };
                TracedAction::Retain { protection }
            }
            Action::Merge { into, .. } => TracedAction::Merge {
                into_seq: id_to_seq.get(into).copied(),
            },
            Action::Reject => TracedAction::Reject,
        };

        // Only a `Retain` creates a genuinely new item. `outcome.item_id` on
        // a `Merge` is the target's id, already present in `id_to_seq` from
        // whichever earlier write actually created it (or absent, if the
        // target predates this scenario) — recording it here unconditionally
        // would overwrite that mapping with THIS write's own `seq`, making
        // every merge into an existing item report `into_seq` as itself
        // rather than the write that created its target.
        if matches!(outcome.action, Action::Retain { .. })
            && let Some(id) = &outcome.item_id
        {
            id_to_seq.insert(id.clone(), seq);
        }

        decisions.push(TracedDecision {
            seq,
            scope: write.scope.clone(),
            body_digest: blake3::hash(write.body.as_bytes()).to_hex().to_string(),
            action,
            reason_codes: outcome.reasons.iter().map(|r| r.code).collect(),
            evicted: outcome.evicted.len(),
            sensitivity,
        });
    }

    Ok(Trace {
        scenario: scenario.name.clone(),
        policy: policy.id(),
        decisions,
    })
}
