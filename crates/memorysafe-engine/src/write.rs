use crate::error::EngineError;
use crate::outcome::WriteOutcome;
use crate::validate::{self, FailureStance, PolicyFailure};
use crate::{Engine, gather};
use memorysafe_backend::{ItemWrite, MergeWrite, WriteTransaction};
use memorysafe_core::{
    Action, Actor, ActorKind, AssessContext, Assessed, AuditEvent, AuditRecord, Candidate, ItemId,
    ItemRef, MemoryItem, Reason, ReasonCode, Scope, SensitivityLevel, Source, SourceKind, TenantId,
    features,
};
use memorysafe_embed::QuantizedVector;
use serde_json::Value;
use std::collections::BTreeMap;
use time::{Duration, OffsetDateTime};

#[derive(Debug, Clone, PartialEq)]
pub struct RememberRequest {
    pub scope: Scope,
    pub body: String,
    pub kind: String,
    pub source: Source,
    pub occurred_at: Option<OffsetDateTime>,
    pub tags: Vec<String>,
    pub attrs: BTreeMap<String, Value>,
    pub sensitivity_hint: Option<SensitivityLevel>,
    pub ttl: Option<Duration>,
    pub idempotency_key: Option<String>,
    pub actor: Actor,
}

impl RememberRequest {
    pub fn new(scope: Scope, body: &str) -> Self {
        Self {
            scope,
            body: body.to_string(),
            kind: "fact".into(),
            source: Source {
                kind: SourceKind::Agent,
                id: None,
            },
            occurred_at: None,
            tags: vec![],
            attrs: BTreeMap::new(),
            sensitivity_hint: None,
            ttl: None,
            idempotency_key: None,
            actor: Actor {
                kind: ActorKind::Agent,
                id: None,
            },
        }
    }
}

const MAX_BODY_BYTES: usize = 64 * 1024;

impl Engine {
    pub async fn remember(&self, req: RememberRequest) -> Result<WriteOutcome, EngineError> {
        if req.body.trim().is_empty() {
            return Err(EngineError::Validation("body must not be empty".into()));
        }
        if req.body.len() > MAX_BODY_BYTES {
            return Err(EngineError::Validation(format!(
                "body exceeds {MAX_BODY_BYTES} bytes"
            )));
        }

        // Embed. A missing or failed model must never cost a user their memory.
        let embedding = self.embed_cached(&req.body).await;
        let pending_embedding = embedding.is_none();

        let candidate = Candidate {
            body: req.body.clone(),
            kind: req.kind.clone(),
            tags: req.tags.clone(),
            attrs: req.attrs.clone(),
            sensitivity_hint: req.sensitivity_hint,
            embedding: embedding.clone(),
            // Same function `MemoryItem::byte_size` uses. A smaller estimate
            // here would let every admitted item overrun the budget by the
            // difference between what was checked and what is stored.
            byte_size: MemoryItem::charge(
                &req.body,
                &req.kind,
                &req.tags,
                &req.attrs,
                req.source.id.as_deref(),
                req.scope.subject.as_str(),
                req.scope.namespace.as_str(),
            ),
        };

        // Gathers everything the policy is allowed to see. Not one I/O pass:
        // `assess_context` and `admit_context` together make up to four
        // backend round trips before `apply` — `neighbours`, `scope_stats`,
        // `capacity_state`, and `list` (when the budget is bounded) — a
        // fifth (`get`) for a merge decision, and a sixth for `apply` itself.
        let ctx = gather::assess_context(
            self.backend.as_ref(),
            &req.scope,
            embedding.as_ref(),
            self.neighbour_k,
        )
        .await?;

        let assessment = self.run_assess(&candidate, &ctx, &req.scope.tenant)?;

        let admit_ctx = gather::admit_context(
            self.backend.as_ref(),
            &req.scope,
            &ctx,
            self.eviction_candidates,
        )
        .await?;

        let assessed = Assessed {
            candidate: &candidate,
            assessment: &assessment,
        };
        let mut decision = self.run_admit(&assessed, &admit_ctx, &req.scope.tenant)?;
        // `admit_context` (`gather.rs`) hands every eviction candidate a
        // hardcoded `value`/`fragility` placeholder, and `admit` copies both
        // — plus their product — verbatim into a `CapacityPressure`
        // eviction's evidence. Scrubbed here, once, before that evidence can
        // reach an audit row; see `strip_fabricated_eviction_evidence`'s own
        // doc for why the fix lives at this call site rather than at the
        // fabrication's source.
        gather::strip_fabricated_eviction_evidence(&mut decision);

        // Nothing a policy returns is applied until it passes validation.
        if let Err(invalid) = validate::decision(&decision, &admit_ctx) {
            return self
                .handle_invalid_decision(invalid, &req, &assessment)
                .await;
        }

        // Added requirement (Task 33 provenance, see the crate's task report):
        // `AdmitContext` carries no set of existing items, so `validate::decision`
        // has nothing to check `Action::Merge { into, .. }` against — the engine
        // is the first place with a backend in hand to ask. A single scoped
        // `Backend::get` answers both "does it exist" and "is it in this
        // request's scope" at once, since `get` is itself scope-filtered.
        let merge_target = match &decision.action {
            Action::Merge { into, .. } => self.backend.get(&req.scope, into).await?,
            _ => None,
        };
        if let Err(invalid) = validate::merge_target(&decision.action, merge_target.as_ref()) {
            return self
                .handle_invalid_decision(invalid, &req, &assessment)
                .await;
        }

        let now = OffsetDateTime::now_utc();
        let vector = embedding.as_ref().map(QuantizedVector::from_embedding);

        let (item, merge) = match &decision.action {
            Action::Reject => (None, None),
            Action::Retain { protection } => {
                let item = MemoryItem {
                    id: ItemId::new(),
                    scope: req.scope.clone(),
                    body: req.body.clone(),
                    kind: req.kind.clone(),
                    source: req.source.clone(),
                    occurred_at: req.occurred_at,
                    created_at: now,
                    tags: req.tags.clone(),
                    attrs: req.attrs.clone(),
                    // Applied by the ENGINE, not trusted from the policy. A
                    // closed scorer that forgot to call `raised_by` would
                    // silently downgrade a caller's declared `Restricted` to
                    // its own detector's level, and the item would then satisfy
                    // a lower `sensitivity_ceiling` — the leak the read path
                    // exists to prevent, caused by an omission in the closed
                    // crate.
                    sensitivity: assessment.sensitivity.level.raised_by(req.sensitivity_hint),
                    ttl: req.ttl,
                    protection: *protection,
                    pending_embedding,
                };
                (Some(item), None)
            }
            Action::Merge { into, .. } => (
                None,
                Some(MergeWrite {
                    target: into.clone(),
                    body: req.body.clone(),
                    tags: req.tags.clone(),
                    attrs: req.attrs.clone(),
                    vector: vector.clone(),
                    byte_size: req.body.len() as u64,
                    // Same `pending_embedding` this write already computed
                    // for the `Retain` arm above, from the same `embedding`
                    // — a merge target whose re-embed just failed needs the
                    // backfill job to find it exactly as a freshly admitted
                    // item does.
                    pending_embedding,
                }),
            ),
        };

        let event = match &decision.action {
            Action::Retain { .. } => AuditEvent::Admitted,
            Action::Merge { .. } => AuditEvent::Merged,
            Action::Reject => AuditEvent::Rejected,
        };

        let refs: Vec<ItemRef> = item
            .as_ref()
            .map(|i| vec![ItemRef::from_item(i)])
            .unwrap_or_default();
        // `now`, already sampled above for `created_at`, not a second clock
        // read — the item and its own audit row must agree on when this
        // happened.
        let audit = AuditRecord::new(req.scope.clone(), event, refs, req.actor.clone(), now)
            .with_assessment(assessment.clone())
            .with_decision(decision.clone());

        let mut txn = WriteTransaction::new(req.scope.clone(), audit);
        txn.evictions = decision.evictions.iter().map(|e| e.item.clone()).collect();
        txn.idempotency_key = req.idempotency_key.clone();
        txn.payload_digest = req
            .idempotency_key
            .as_ref()
            .map(|_| blake3::hash(req.body.as_bytes()).to_hex().to_string());
        if let Some(i) = item {
            txn.upsert = Some(ItemWrite { item: i, vector });
        }
        txn.merge = merge;

        let applied = self.backend.apply(txn).await?;
        // Any write invalidates its scope: `ScopeStats` feeds fragility
        // scoring, and a stale corpus mean would skew every assessment made
        // against it. Unconditional — even a replayed idempotent write below
        // invalidates, which is at worst an unnecessary refetch, never a
        // correctness gap.
        self.cache.invalidate_scope(&req.scope).await;

        // The backend short-circuited on this request's idempotency key and
        // returned the ORIGINAL `AppliedWrite`, having applied nothing this
        // time — but `decision` above is this call's own fresh evaluation,
        // computed against a corpus that (for a retried admit) now already
        // contains the item the first call created. An identical retried
        // body reads back as a near-duplicate of itself, so `decision.action`
        // here can be `Reject` while `applied.item_id` names a real, stored
        // row: a self-contradictory outcome that never happened. See
        // `replayed_outcome` for how the real one is recovered.
        if applied.replayed {
            return self.replayed_outcome(&req.scope, applied).await;
        }

        Ok(WriteOutcome {
            item_id: applied.item_id.clone(),
            action: decision.action.clone(),
            reasons: decision.reasons.clone(),
            merged_into: match &decision.action {
                Action::Merge { into, .. } => Some(into.clone()),
                _ => None,
            },
            evicted: applied.evicted,
            audit_id: applied.audit_id,
        })
    }

    /// Reconstructs the outcome of a replayed idempotent write. `AppliedWrite`
    /// itself carries only `item_id`/`audit_id`/`evicted` from the original
    /// call — not the original `Decision` — so `action`/`reasons`/
    /// `merged_into` are not present on it at all; guessing them from this
    /// call's fresh (and, for a retried admit, actively misleading) decision
    /// is exactly the bug this exists to avoid.
    ///
    /// The original write's own audit row is then found by scanning the
    /// scope's audit page for the `audit_id` the backend already returned,
    /// recovering the real `Decision` and, with it, exact `action`/`reasons`/
    /// `merged_into`.
    ///
    /// **`AuditFilter::item` is set below and, against the only backend that
    /// exists, narrows nothing.** `memorysafe-backend-sqlite`'s `audit::query`
    /// honours `events`, `since`, `until`, `after` and `limit`, and silently
    /// ignores `item`, `subject` and `namespace` — so what actually comes back
    /// is the scope's most recent `AuditFilter::default().limit` (100) rows,
    /// newest first, not that item's history. The lookup still works, for a
    /// reason that has nothing to do with the filter: the row being searched
    /// for was written moments earlier in the same scope, so it is at or near
    /// the top of the newest-first page, and the scan below matches on
    /// `r.id == applied.audit_id` rather than trusting the filter to have
    /// isolated it. What is lost is the *bound*: on a scope with heavy
    /// concurrent write traffic, 100 newer rows can push the target off the
    /// page, and the fallback below then runs instead of the exact recovery.
    ///
    /// The field is left set rather than removed: it is correct against the
    /// contract, `AuditFilter::item`'s own doc says what it means, and a
    /// backend that implements it makes this both narrower and exact.
    /// Implementing it in SQLite is a change to the frozen conformance
    /// contract (`Backend::audit` says nothing about these three fields —
    /// `docs/known-gaps.md` ranks it second among the gaps the freeze locks
    /// in), so it belongs to the next contract batch, not here.
    ///
    /// **Known gap, not chased in this fix:** a replayed MERGE cannot be
    /// recovered exactly. A merge's own audit record carries no `ItemRef` (see
    /// `refs` above — `item` is `None` on the `Merge` arm), so even a backend
    /// that implemented `AuditFilter::item` would not match it, and the
    /// fallback below reports a protection-accurate `Retain` instead of the
    /// true `Merge`. That is an approximation, not a fabrication: the reported
    /// protection is read fresh from the stored item, never guessed, and the
    /// outcome still never contradicts `item_id`/`evicted`/`audit_id`, which
    /// stay exact.
    async fn replayed_outcome(
        &self,
        scope: &Scope,
        applied: memorysafe_backend::AppliedWrite,
    ) -> Result<WriteOutcome, EngineError> {
        // `item` is set when there is one — and is ignored by the only
        // backend that exists, so this is `AuditFilter`'s default page for
        // the whole scope either way, newest first. See this method's doc for
        // why the lookup still finds its row and what the unimplemented
        // narrowing actually costs. (`applied.item_id` can be `None` here:
        // idempotency rows are written for genuine rejects too, since
        // `txn.idempotency_key` is set unconditionally above.)
        let filter = memorysafe_core::AuditFilter {
            item: applied.item_id.clone(),
            ..Default::default()
        };
        let history = self.backend.audit(scope, &filter).await?;
        if let Some(decision) = history
            .into_iter()
            .find(|r| r.id == applied.audit_id)
            .and_then(|r| r.decision)
        {
            return Ok(WriteOutcome {
                item_id: applied.item_id.clone(),
                merged_into: match &decision.action {
                    Action::Merge { into, .. } => Some(into.clone()),
                    _ => None,
                },
                action: decision.action,
                reasons: decision.reasons,
                evicted: applied.evicted,
                audit_id: applied.audit_id,
            });
        }

        // The original decision could not be recovered — a replayed merge
        // (whose own audit row carries no item reference; see `refs` above),
        // or a row that fell outside the default audit page. Report only
        // what is safe not to contradict: `Reject` when nothing is stored,
        // otherwise `Retain` at the item's real, current protection rather
        // than a guess. `reasons`/`merged_into` are left unrecoverable
        // rather than fabricated.
        let action = match &applied.item_id {
            Some(id) => {
                let protection = self
                    .backend
                    .get(scope, id)
                    .await?
                    .map(|i| i.protection)
                    .unwrap_or(memorysafe_core::Protection::Normal);
                Action::Retain { protection }
            }
            None => Action::Reject,
        };
        Ok(WriteOutcome {
            item_id: applied.item_id.clone(),
            action,
            reasons: vec![],
            merged_into: None,
            evicted: applied.evicted,
            audit_id: applied.audit_id,
        })
    }

    async fn handle_invalid_decision(
        &self,
        invalid: validate::Invalid,
        req: &RememberRequest,
        assessment: &memorysafe_core::Assessment,
    ) -> Result<WriteOutcome, EngineError> {
        let reason = Reason::new(
            ReasonCode::PolicyInvalid,
            &format!("policy returned an unusable decision: {invalid}"),
            features! {},
        );
        let audit = AuditRecord::new(
            req.scope.clone(),
            AuditEvent::Rejected,
            vec![],
            req.actor.clone(),
            OffsetDateTime::now_utc(),
        )
        .with_assessment(assessment.clone());

        let txn = WriteTransaction::new(req.scope.clone(), audit);
        let applied = self.backend.apply(txn).await?;

        // Branches on `self.stance`; `read.rs`'s analogous check
        // (`validate::working_set`'s failure inside `recall`) deliberately
        // does not, and stays `FailClosed` unconditionally — see the comment
        // at that call site for why the two are not held to the same rule.
        // Short version: `FailSafe`'s substitute here is safe to hand back
        // regardless of what made the decision invalid, because
        // `Action::Reject` discloses nothing — the write simply does not
        // happen. `recall`'s equivalent failure is the opposite shape: the
        // policy handed back items it was never offered, which is a leak
        // attempt, not an availability gap, and no substitute working set is
        // obviously safe to serve in its place the way "reject this one
        // write" is here.
        match self.stance {
            FailureStance::FailClosed => Err(EngineError::PolicyRefused(invalid.to_string())),
            FailureStance::FailSafe => Ok(WriteOutcome {
                item_id: None,
                action: Action::Reject,
                reasons: vec![reason],
                merged_into: None,
                evicted: vec![],
                audit_id: applied.audit_id,
            }),
        }
    }

    fn run_assess(
        &self,
        cand: &Candidate,
        ctx: &AssessContext,
        tenant: &TenantId,
    ) -> Result<memorysafe_core::Assessment, EngineError> {
        let policy = self.policy_for(tenant);
        let (c, x) = (cand.clone(), ctx.clone());
        // `Arc<dyn GovernancePolicy>` is not `RefUnwindSafe` — the compiler
        // cannot see into a closed-source policy to know it holds no interior
        // mutability, so it will not infer the closure `UnwindSafe` on its
        // own. `AssertUnwindSafe` is the standard escape hatch: this call
        // never hands the policy a `&mut` to anything shared, so a panic here
        // leaves no half-mutated state for the caller to observe.
        match validate::call_policy(std::panic::AssertUnwindSafe(move || policy.assess(&c, &x))) {
            Ok(a) => Ok(a),
            Err(e) => self.policy_fallback(e, || self.fallback_policy.assess(cand, ctx)),
        }
    }

    fn run_admit(
        &self,
        assessed: &Assessed,
        ctx: &memorysafe_core::AdmitContext,
        tenant: &TenantId,
    ) -> Result<memorysafe_core::Decision, EngineError> {
        let policy = self.policy_for(tenant);
        let (c, a, x) = (
            assessed.candidate.clone(),
            assessed.assessment.clone(),
            ctx.clone(),
        );
        let call = std::panic::AssertUnwindSafe(move || {
            let assessed = Assessed {
                candidate: &c,
                assessment: &a,
            };
            policy.admit(&assessed, &x)
        });
        match validate::call_policy(call) {
            Ok(d) => Ok(d),
            Err(e) => self.policy_fallback(e, || self.fallback_policy.admit(assessed, ctx)),
        }
    }

    fn policy_fallback<T, F>(&self, failure: PolicyFailure, fallback: F) -> Result<T, EngineError>
    where
        F: FnOnce() -> Result<T, memorysafe_core::PolicyError>,
    {
        match self.stance {
            FailureStance::FailClosed => Err(EngineError::PolicyRefused(failure.to_string())),
            FailureStance::FailSafe => {
                fallback().map_err(|e| EngineError::PolicyRefused(e.to_string()))
            }
        }
    }
}
