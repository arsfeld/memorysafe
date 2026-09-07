use crate::MemorySafeServer;
use crate::dto::{
    RecallParams, RecallResult, RememberParams, RememberResult, parse_mode, parse_sensitivity,
};
use memorysafe_core::{RecallBudget, RecallRequest, SensitivityLevel};
use memorysafe_engine::RememberRequest;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, tool, tool_router};
use time::{Duration, OffsetDateTime};

/// Engine failures are the only tool errors. A rejection, a merge, or an empty
/// working set is a successful call carrying a decision.
fn engine_error(e: memorysafe_engine::EngineError) -> ErrorData {
    match &e {
        memorysafe_engine::EngineError::Validation(_)
        | memorysafe_engine::EngineError::NotFound(_)
        | memorysafe_engine::EngineError::Conflict(_) => {
            ErrorData::invalid_params(e.to_string(), None)
        }
        _ => ErrorData::internal_error(e.to_string(), None),
    }
}

fn timestamp(seconds: Option<i64>, field: &str) -> Result<Option<OffsetDateTime>, ErrorData> {
    seconds
        .map(|s| {
            OffsetDateTime::from_unix_timestamp(s).map_err(|_| {
                ErrorData::invalid_params(format!("{field} is not a valid Unix timestamp"), None)
            })
        })
        .transpose()
}

// `vis = "pub(crate)"`: not in the plan text as literally written (which
// leaves `vis` unset, so the macro emits a module-private `write_router()`).
// That does not compile — `MemorySafeServer::new` in `lib.rs` calls
// `Self::write_router()` from the crate root, which is an ANCESTOR of this
// `tools_write` module, not a descendant; Rust privacy only opens a private
// item to its defining module and that module's descendants. Verified with a
// minimal repro (`rustc` on an equivalent private-inherent-method-called-
// from-a-parent-module snippet: E0624) before changing this. `pub(crate)` is
// the least-visibility fix and matches the macro's own documented usage.
#[tool_router(router = write_router, vis = "pub(crate)")]
impl MemorySafeServer {
    /// Write a memory. Returns the governance decision: the memory may be
    /// retained, merged into an existing one, or rejected as redundant. All
    /// three are successful calls.
    #[tool(
        name = "memory_remember",
        description = "Store one discrete memory and return the governance decision — retained, merged into an existing memory, or rejected as redundant — with the reasons behind it."
    )]
    async fn memory_remember(
        &self,
        Parameters(params): Parameters<RememberParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<RememberResult>, ErrorData> {
        let resolved = self.source.resolve(
            &ctx.extensions,
            params.subject.as_deref(),
            params.namespace.as_deref(),
        )?;

        let mut req = RememberRequest::new(resolved.scope, &params.body);
        req.actor = resolved.actor;
        if let Some(kind) = params.kind {
            req.kind = kind;
        }
        req.tags = params.tags.unwrap_or_default();
        req.sensitivity_hint = params
            .sensitivity_hint
            .as_deref()
            .map(parse_sensitivity)
            .transpose()?;
        req.ttl = params.ttl_seconds.map(Duration::seconds);
        req.idempotency_key = params.idempotency_key;

        let outcome = self.engine.remember(req).await.map_err(engine_error)?;
        Ok(Json(RememberResult::from(&outcome)))
    }

    /// Read a governed working set under a token budget.
    #[tool(
        name = "memory_recall",
        description = "Return a governed working set of memories under a token budget, with a reason for every memory selected and every memory omitted. Set mode to 'search' for a raw ranked list."
    )]
    async fn memory_recall(
        &self,
        Parameters(params): Parameters<RecallParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<RecallResult>, ErrorData> {
        let resolved = self.source.resolve(
            &ctx.extensions,
            params.subject.as_deref(),
            params.namespace.as_deref(),
        )?;

        let default_budget = RecallBudget::default();
        let req = RecallRequest {
            scope: resolved.scope,
            query: params.query,
            tags_any: params.tags_any.unwrap_or_default(),
            kinds: params.kinds.unwrap_or_default(),
            occurred_after: timestamp(params.occurred_after, "occurred_after")?,
            occurred_before: timestamp(params.occurred_before, "occurred_before")?,
            mode: params
                .mode
                .as_deref()
                .map(parse_mode)
                .transpose()?
                .unwrap_or_default(),
            budget: RecallBudget {
                max_tokens: params.max_tokens.or(default_budget.max_tokens),
                max_items: params.max_items.or(default_budget.max_items),
            },
            sensitivity_ceiling: params
                .sensitivity_ceiling
                .as_deref()
                .map(parse_sensitivity)
                .transpose()?
                .unwrap_or(SensitivityLevel::Restricted),
        };

        let ws = self.engine.recall(req).await.map_err(engine_error)?;
        Ok(Json(RecallResult::build(&ws)?))
    }
}
