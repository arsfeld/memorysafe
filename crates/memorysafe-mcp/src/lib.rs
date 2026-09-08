//! The MCP adapter. Five tools and two resources over one engine.
//!
//! Serial tool calls are where agent memory dies, so the surface stays small
//! and each call does real work. Nothing here decides anything: every action
//! and every reason in a result came out of the engine.

pub mod dto;
pub mod resources;
pub mod scope;
mod tools_curate;
mod tools_write;
mod transport;

pub use scope::{auth_error, resolve_call};
pub use transport::{HttpTransportConfig, http_service, http_service_with, serve_stdio};

use crate::resources::{ResourceKind, resource_uri, uri_template};
use memorysafe_auth::ScopeResolver;
use memorysafe_core::AuditFilter;
use memorysafe_engine::Engine;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{
    ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ResourceTemplate, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::tool_handler;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use std::sync::Arc;

#[derive(Clone)]
pub struct MemorySafeServer {
    pub(crate) engine: Arc<Engine>,
    pub(crate) resolver: Arc<dyn ScopeResolver>,
    tool_router: ToolRouter<Self>,
}

impl MemorySafeServer {
    pub fn new(engine: Arc<Engine>, resolver: Arc<dyn ScopeResolver>) -> Self {
        Self {
            engine,
            resolver,
            tool_router: Self::write_router() + Self::curate_router(),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MemorySafeServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_instructions(
            "Governed memory. `memory_remember` writes and returns the governance decision \
                 — a rejection or a merge is a successful call, not an error. `memory_recall` \
                 returns a working set composed under a token budget, with a reason for every \
                 item selected and every item omitted."
                .to_string(),
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        // Over HTTP the scope is a property of the request, not of the server,
        // so there is nothing concrete to list — the templates carry the shape.
        let Some(scope) = self.resolver.default_scope() else {
            return Ok(ListResourcesResult::default());
        };
        Ok(ListResourcesResult::with_all_items(
            ResourceKind::ALL
                .into_iter()
                .map(|kind| {
                    Resource::new(
                        resource_uri(&scope, kind),
                        format!("{} {}", scope.namespace, kind.as_str()),
                    )
                    .with_description(kind.description())
                    .with_mime_type("application/json")
                })
                .collect(),
        ))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(
            ResourceKind::ALL
                .into_iter()
                .map(|kind| {
                    ResourceTemplate::new(uri_template(kind), kind.as_str())
                        .with_description(kind.description())
                        .with_mime_type("application/json")
                })
                .collect(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let parsed = resources::parse_uri(&request.uri)?;

        // The URI names a scope; the credential decides which scope this
        // caller may name. Resolving through the same path the tools use
        // means a resource URI can never reach further than a tool call could.
        // The URI also carries a tenant and subject, and neither is
        // caller-supplied scope. Both are compared, not trusted: without the
        // subject comparison a caller could read another subject's audit
        // trail through a URI while every tool-level check still held.
        let resolved = crate::scope::resolve_call(
            self.resolver.as_ref(),
            &context.extensions,
            Some(parsed.namespace),
        )?;
        if resolved.scope.tenant.as_str() != parsed.tenant
            || resolved.scope.subject.as_str() != parsed.subject
        {
            return Err(ErrorData::resource_not_found(
                format!("no such resource: {}", request.uri),
                None,
            ));
        }

        let body = match parsed.kind {
            ResourceKind::Audit => {
                let filter = AuditFilter::default();
                let records = self
                    .engine
                    .audit(&resolved.scope, &filter)
                    .await
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                serde_json::json!({
                    "scope": resolved.scope,
                    "records": records,
                    // `AuditFilter::limit`'s own doc: truncation is
                    // detectable only via `returned.len() < limit`, since
                    // there is no separate `truncated` flag. This resource
                    // has no cursor in its URI grammar and always uses the
                    // default filter, so without echoing the limit here a
                    // client at exactly the cap has no way to tell "this is
                    // everything" from "this is the first page" — the
                    // signal the rest of the workspace relies on never
                    // reaches it.
                    "limit": filter.limit,
                })
            }
            ResourceKind::Stats => {
                let capacity = self
                    .engine
                    .capacity_state(&resolved.scope)
                    .await
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let stats = self
                    .engine
                    .scope_stats(&resolved.scope)
                    .await
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                serde_json::json!({
                    "scope": resolved.scope,
                    "budget": capacity.budget,
                    "used_items": capacity.used_items,
                    "used_bytes": capacity.used_bytes,
                    "item_count": stats.item_count,
                    "total_bytes": stats.total_bytes,
                    "median_item_bytes": stats.median_item_bytes,
                    // `Backend::scope_stats` never computes a real value here:
                    // `memorysafe-backend-sqlite::capacity::stats` leaves it at
                    // `ScopeStats`'s `0.0` "no data" sentinel unconditionally
                    // (see that field's own doc — "any consumer reading it
                    // directly must check `item_count` first"). Only the
                    // engine's in-flight assessment path
                    // (`gather::assess_context`) ever computes a real figure,
                    // and `Engine::scope_stats` does not go through it — so
                    // this key would read `0.0` on *every* scope, populated or
                    // not, which is not a low similarity, it is an absent one.
                    // `null`, not omitted: the field is named explicitly in
                    // this resource's specified JSON shape, so dropping it
                    // silently would remove a specified key rather than
                    // honestly saying "not available from this read". Publish
                    // a real number once a path exists to compute one outside
                    // admission.
                    "mean_neighbour_similarity": serde_json::Value::Null,
                })
            }
        };

        let text = serde_json::to_string_pretty(&body)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        // `ResourceContents::text` defaults its own `mimeType` to
        // `text/plain`; without this override the per-read content block
        // would contradict the `application/json` the listing advertises —
        // and the content block is what a client dispatches rendering on.
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, request.uri).with_mime_type("application/json"),
        ])
        .into())
    }
}
