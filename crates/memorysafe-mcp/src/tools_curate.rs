use crate::MemorySafeServer;
use crate::dto::{
    ForgetParams, ForgetResult, MemoryView, ProtectParams, ProtectResult, ReasonView, ReviewParams,
    ReviewResult, ReviewedMemory, parse_protection,
};
use crate::tools_write::engine_error;
use memorysafe_backend::Page;
use memorysafe_core::{AuditFilter, ItemId, Protection};
use memorysafe_engine::ForgetSelector;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, tool, tool_router};
use std::collections::HashMap;

/// How many audit rows a review reads to find reasons for one page of items.
/// A page of 50 items may be spread over many rows — merges, evictions,
/// recalls — so the window is generous but bounded.
const REVIEW_AUDIT_WINDOW: usize = 500;

fn item_id(raw: &str) -> Result<ItemId, ErrorData> {
    ItemId::parse(raw)
        .map_err(|e| ErrorData::invalid_params(format!("bad item id '{raw}': {e}"), None))
}

#[tool_router(router = curate_router, vis = "pub(crate)")]
impl MemorySafeServer {
    /// List what is stored and why.
    #[tool(
        name = "memory_review",
        description = "List the memories stored in this scope with the governance reason each one is there for. Use this to show a person what the agent remembers about them. The reason is only available for decisions made within the recent audit window; an older item is still listed but reports reason_code and reason_detail as null rather than a guessed reason."
    )]
    async fn memory_review(
        &self,
        Parameters(params): Parameters<ReviewParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ReviewResult>, ErrorData> {
        let resolved = self.source.resolve(
            &ctx.extensions,
            params.subject.as_deref(),
            params.namespace.as_deref(),
        )?;
        let default_page = Page::default();
        let page = Page {
            offset: params.offset.unwrap_or(default_page.offset),
            limit: params.limit.unwrap_or(default_page.limit),
        };

        let items = self
            .engine
            .review(&resolved.scope, &page)
            .await
            .map_err(engine_error)?;

        // One audit query for the whole page, indexed by item id. The engine
        // returns rows newest first, so the first row naming an item is the
        // most recent decision about it.
        let rows = self
            .engine
            .audit(
                &resolved.scope,
                &AuditFilter {
                    limit: REVIEW_AUDIT_WINDOW,
                    ..Default::default()
                },
            )
            .await
            .map_err(engine_error)?;
        let mut reasons: HashMap<String, ReasonView> = HashMap::new();
        for row in &rows {
            let Some(decision) = row.decision.as_ref() else {
                continue;
            };
            let Some(reason) = decision.reasons.first() else {
                continue;
            };
            for item_ref in &row.items {
                reasons
                    .entry(item_ref.id().to_string())
                    .or_insert_with(|| ReasonView::from(reason));
            }
        }

        Ok(Json(ReviewResult {
            items: items
                .iter()
                .map(|item| {
                    let reason = reasons.get(item.id.as_str());
                    ReviewedMemory {
                        memory: MemoryView::from(item),
                        reason_code: reason.map(|r| r.code.clone()),
                        reason_detail: reason.map(|r| r.detail.clone()),
                    }
                })
                .collect(),
            offset: page.offset,
            // Not `page.limit`: the backend clamps to `Page::effective_limit()`
            // before running the query (`MAX_PAGE_LIMIT`), and this echo is
            // the only exhaustion signal a caller has — `AuditFilter::limit`'s
            // doc states the same convention for audit paging: no `truncated`
            // flag, `returned.len() < limit` is the sole exhaustion test. An
            // echo of the raw, unclamped request would make a caller asking
            // for more than the ceiling see fewer items than the (wrong)
            // limit it was told, and wrongly conclude the scope was
            // exhausted — silently hiding part of what is stored, which is
            // exactly what this tool exists to prevent.
            limit: page.effective_limit(),
        }))
    }

    /// Delete memories by id, tag, or kind.
    #[tool(
        name = "memory_forget",
        description = "Delete memories by id, by tag, or by kind. Exactly one selector must be given. Deleting something that is not there is a successful, empty result."
    )]
    async fn memory_forget(
        &self,
        Parameters(params): Parameters<ForgetParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ForgetResult>, ErrorData> {
        let resolved = self.source.resolve(
            &ctx.extensions,
            params.subject.as_deref(),
            params.namespace.as_deref(),
        )?;

        let selectors = [
            params.ids.is_some(),
            params.tag.is_some(),
            params.kind.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if selectors != 1 {
            return Err(ErrorData::invalid_params(
                "memory_forget takes exactly one of 'ids', 'tag', or 'kind'",
                None,
            ));
        }

        let selector = if let Some(ids) = params.ids {
            ForgetSelector::Ids(ids.iter().map(|r| item_id(r)).collect::<Result<_, _>>()?)
        } else if let Some(tag) = params.tag {
            ForgetSelector::Tag(tag)
        } else {
            ForgetSelector::Kind(params.kind.expect("checked above"))
        };

        let outcome = self
            .engine
            .forget(&resolved.scope, selector)
            .await
            .map_err(engine_error)?;
        Ok(Json(ForgetResult {
            forgotten: outcome.forgotten.iter().map(|i| i.to_string()).collect(),
            audit_id: outcome.audit_id.to_string(),
        }))
    }

    /// Pin or protect an existing memory.
    #[tool(
        name = "memory_protect",
        description = "Pin a memory so no policy can evict it, or protect it until a deadline. This is the only way protection changes outside admission, and it is recorded in the audit trail."
    )]
    async fn memory_protect(
        &self,
        Parameters(params): Parameters<ProtectParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ProtectResult>, ErrorData> {
        let resolved = self.source.resolve(
            &ctx.extensions,
            params.subject.as_deref(),
            params.namespace.as_deref(),
        )?;
        let id = item_id(&params.id)?;
        let protection = parse_protection(&params.level, params.until)?;

        let outcome = self
            .engine
            .protect(&resolved.scope, &id, protection)
            .await
            .map_err(engine_error)?;

        Ok(Json(ProtectResult {
            item_id: outcome.item_id.map(|i| i.to_string()).unwrap_or(params.id),
            protection: crate::dto::protection_name(&protection).to_owned(),
            protected_until: match protection {
                Protection::Protected { until } => Some(until.unix_timestamp()),
                _ => None,
            },
            audit_id: outcome.audit_id.to_string(),
        }))
    }
}
