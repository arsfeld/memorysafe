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
/// Matches `RecallBudget::default()`'s own `max_items: Some(20)`, so a
/// caller who omits a budget entirely gets the same default item count as
/// one who supplies `RecallBudget::default()` explicitly. This sets the
/// *value* multiplied by `OVERFETCH` below; it does not bound the fetch —
/// the `.clamp(MIN_CANDIDATES, MAX_CANDIDATES)` two lines down does that,
/// and caps the result at `MAX_CANDIDATES` regardless of this default.
const DEFAULT_MAX_ITEMS: usize = 20;

/// Floor on the over-fetch limit. Below this, a tiny `max_items` (e.g. `1`)
/// combined with `OVERFETCH` could hand the policy too few candidates to
/// exercise diversity or the replay quota at all — composition needs a
/// minimum working pool regardless of how small the caller's budget is.
/// `10` itself is a conservative choice, not derived from a measured
/// minimum working-set size.
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

        let query_text = req.query.as_deref().filter(|q| !q.trim().is_empty());
        let embedding = match query_text {
            Some(q) => self.embed_cached(q).await,
            None => None,
        };

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

        // A policy may narrow the candidate set; it may never widen it. A
        // policy caught doing so is a security event — it must leave an
        // audit trail even though nothing is returned to the caller, the
        // same way `write.rs`'s `handle_invalid_decision` audits an invalid
        // write decision as `Rejected` before deciding what to return.
        //
        // **Deliberately does not consult `self.stance` below, unlike
        // `handle_invalid_decision`, which does branch on it (`FailSafe`
        // there returns a usable `Action::Reject` outcome instead of
        // erroring).** The two checks look parallel — both fire when a
        // policy hands back a structurally invalid decision — but they are
        // not the same kind of failure. `handle_invalid_decision`, and the
        // `call_policy` failure just above this comment (a policy panic or
        // returned `PolicyError`), are availability problems: the policy
        // failed to produce a usable answer, and falling back to a
        // conservative default (`Action::Reject`; `self.fallback_policy`) is
        // a sensible way to keep serving traffic. This check is different in
        // kind: the policy DID produce an answer, and that answer names data
        // it was never offered — `ws.items`/`ws.omitted` containing an
        // unoffered id, or an offered id wearing a substituted body (see
        // `validate::working_set`'s own doc). That is a leak attempt, not a
        // crash, and "degrade to a safe-looking substitute and keep going"
        // is not obviously the safe move for a leak the way it is for a
        // panic — an operator needs FailClosed's hard stop to be exactly
        // that: a stop, not something a policy config can quietly widen back
        // into "return whatever composed, minus the offending items." Fixed
        // fail-closed here regardless of `self.stance`, on that basis. If you
        // revisit this, `remember`'s `handle_invalid_decision` (`write.rs`)
        // is the site whose behaviour would need to change in step, and this
        // comment is the reason it has not.
        //
        // `validate::working_set` also enforces `req.budget` and refuses a
        // repeated item, and both stay under the same fixed fail-closed rule.
        // A budget overrun is over-disclosure — more of the corpus reaching
        // the caller than the caller asked for — and the honest response to
        // "the policy returned 500 items to a request for 5" is a refusal,
        // not a silent truncation that would make the engine's answer differ
        // from the one the policy actually composed and the audit row
        // actually names. **The `req.budget` passed here is the caller's own,
        // not the clamped fetch limit above**: `limit` is a load control on
        // the backend, and validating against it would check the policy
        // against the engine's over-fetch rather than against what the caller
        // asked for.
        if let Err(invalid) = validate::working_set(&composed, &candidates, &req.budget) {
            let audit = AuditRecord::new(
                req.scope.clone(),
                AuditEvent::Rejected,
                vec![],
                Actor {
                    kind: ActorKind::Agent,
                    id: None,
                },
                ctx.now,
            );
            self.backend.record_recall(audit).await?;
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
