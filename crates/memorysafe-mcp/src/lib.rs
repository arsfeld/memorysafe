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

pub use scope::{Resolved, ScopeSource};

use crate::resources::{ResourceKind, resource_uri, uri_template};
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
    pub(crate) source: ScopeSource,
    tool_router: ToolRouter<Self>,
}

impl MemorySafeServer {
    pub fn new(engine: Arc<Engine>, source: ScopeSource) -> Self {
        Self {
            engine,
            source,
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
        let Some(scope) = self.source.default_scope() else {
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

        // The URI names a scope; the transport decides which scopes this caller
        // may name. Resolving through the same path the tools use means a
        // resource URI can never reach further than a tool call could.
        let resolved = self.source.resolve(
            &context.extensions,
            Some(parsed.subject),
            Some(parsed.namespace),
        )?;
        if resolved.scope.tenant.as_str() != parsed.tenant {
            return Err(ErrorData::resource_not_found(
                format!("no such resource: {}", request.uri),
                None,
            ));
        }

        let body = match parsed.kind {
            ResourceKind::Audit => {
                let records = self
                    .engine
                    .audit(&resolved.scope, &AuditFilter::default())
                    .await
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                serde_json::json!({
                    "scope": resolved.scope,
                    "records": records,
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
                    "mean_neighbour_similarity": stats.mean_neighbour_similarity,
                })
            }
        };

        let text = serde_json::to_string_pretty(&body)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(ReadResourceResult::new(vec![ResourceContents::text(text, request.uri)]).into())
    }
}
