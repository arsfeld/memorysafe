use crate::Engine;
use crate::error::EngineError;
use crate::validate;
use memorysafe_backend::{CandidateQuery, HardFilters};
use memorysafe_core::{
    Actor, ActorKind, AuditEvent, AuditRecord, ComposeContext, ItemRef, RecallRequest,
    ScoredCandidate, WorkingSet,
};
use time::OffsetDateTime;

/// Over-fetch factor: composition needs room to trade relevance for diversity
/// and to fill the replay quota, so it must see more than it will return.
const OVERFETCH: usize = 8;

/// Default item count when a caller's `RecallBudget::max_items` is `None`.
/// Matches `RecallBudget::default()`'s own `max_items: Some(20)` — an
/// unbounded item count would otherwise make an unbounded-looking recall
/// over-fetch without limit before any policy or budget check runs.
const DEFAULT_MAX_ITEMS: usize = 20;

/// Floor on the over-fetch limit. Below this, a tiny `max_items` (e.g. `1`)
/// combined with `OVERFETCH` could hand the policy too few candidates to
/// exercise diversity or the replay quota at all — composition needs a
/// minimum working pool regardless of how small the caller's budget is.
const MIN_CANDIDATES: usize = 10;

/// Ceiling on the over-fetch limit. Chosen as a conservative bound on how
/// much a single recall may pull from the backend and hand to the policy in
/// one call, not derived from a measured cost model — it exists so a caller
/// requesting a very large `max_items` cannot turn one recall into an
/// unbounded backend scan.
const MAX_CANDIDATES: usize = 500;

impl Engine {
    pub async fn recall(&self, req: RecallRequest) -> Result<WorkingSet, EngineError> {
        let limit = req
            .budget
            .max_items
            .unwrap_or(DEFAULT_MAX_ITEMS)
            .saturating_mul(OVERFETCH)
            .clamp(MIN_CANDIDATES, MAX_CANDIDATES);

        let embedding = req
            .query
            .as_deref()
            .filter(|q| !q.trim().is_empty())
            .and_then(|q| self.embedder.embed(q).ok());

        if embedding.is_none() && req.query.as_deref().unwrap_or("").trim().is_empty() {
            return Err(EngineError::Validation(
                "a recall needs a query; filter-only recall is not supported in v1".into(),
            ));
        }

        let query = CandidateQuery {
            embedding,
            text: req.query.clone(),
            // The security boundary: these run in SQL, below the policy.
            filters: HardFilters {
                tags_any: req.tags_any.clone(),
                kinds: req.kinds.clone(),
                occurred_after: req.occurred_after,
                occurred_before: req.occurred_before,
                sensitivity_ceiling: req.sensitivity_ceiling,
                exclude_pending_embedding: false,
            },
            limit,
        };

        let candidates: Vec<ScoredCandidate> =
            self.backend.retrieve_candidates(&req.scope, &query).await?;

        if candidates.is_empty() {
            let audit = AuditRecord::new(
                req.scope.clone(),
                AuditEvent::Recalled,
                vec![],
                Actor {
                    kind: ActorKind::Agent,
                    id: None,
                },
                OffsetDateTime::now_utc(),
            );
            let audit_id = self.backend.record_recall(audit).await?;
            return Ok(WorkingSet {
                audit_id: Some(audit_id),
                ..WorkingSet::empty()
            });
        }

        let ctx = ComposeContext {
            scope: req.scope.clone(),
            stats: self.backend.scope_stats(&req.scope).await?,
            now: OffsetDateTime::now_utc(),
        };

        let policy = self.policy.clone();
        let (r, c, x) = (req.clone(), candidates.clone(), ctx.clone());
        debug_assert!(
            candidates.iter().all(|c| c.item.scope == req.scope),
            "backend returned a candidate outside the requested scope"
        );
        let call = std::panic::AssertUnwindSafe(move || policy.compose(&r, &c, &x));
        let composed = match validate::call_policy(call) {
            Ok(ws) => ws,
            Err(failure) => match self.stance {
                validate::FailureStance::FailClosed => {
                    return Err(EngineError::PolicyRefused(failure.to_string()));
                }
                validate::FailureStance::FailSafe => self
                    .fallback_policy
                    .compose(&req, &candidates, &ctx)
                    .map_err(|e| EngineError::PolicyRefused(e.to_string()))?,
            },
        };

        // A policy may narrow the candidate set; it may never widen it.
        if let Err(invalid) = validate::working_set(&composed, &candidates) {
            return Err(EngineError::PolicyRefused(invalid.to_string()));
        }

        let refs: Vec<ItemRef> = composed
            .items
            .iter()
            .map(|s| ItemRef::from_item(&s.item))
            .collect();
        // Reuse `ctx.now`, already sampled above for the compose context,
        // rather than a second clock read for the same logical recall.
        let audit = AuditRecord::new(
            req.scope.clone(),
            AuditEvent::Recalled,
            refs,
            Actor {
                kind: ActorKind::Agent,
                id: None,
            },
            ctx.now,
        );
        let audit_id = self.backend.record_recall(audit).await?;

        Ok(WorkingSet {
            audit_id: Some(audit_id),
            ..composed
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the literal values of the four constants this module documents.
    /// `MAX_CANDIDATES` in particular has no practical integration-test
    /// counterpart: distinguishing it from a slightly different ceiling would
    /// require seeding hundreds of real candidates purely to observe a
    /// truncation effect, which is disproportionate to what the ceiling
    /// guards against (see the task report). This at least turns an
    /// accidental edit of any of the four into an immediate, obvious failure
    /// rather than a silent behavioural change — the same role
    /// `recall::tests::omitted_list_is_capped_so_a_wide_recall_cannot_blow_up_the_response`
    /// plays for `OMITTED_CAP`.
    #[test]
    fn the_overfetch_constants_have_not_drifted() {
        assert_eq!(OVERFETCH, 8);
        assert_eq!(DEFAULT_MAX_ITEMS, 20);
        assert_eq!(MIN_CANDIDATES, 10);
        assert_eq!(MAX_CANDIDATES, 500);
    }
}
