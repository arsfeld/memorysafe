//! The MCP adapter. Five tools and two resources over one engine.
//!
//! Serial tool calls are where agent memory dies, so the surface stays small
//! and each call does real work. Nothing here decides anything: every action
//! and every reason in a result came out of the engine.

pub mod dto;
pub mod scope;
mod tools_write;

pub use scope::{Resolved, ScopeSource};

use memorysafe_engine::Engine;
use rmcp::ServerHandler;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::tool_handler;
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
            tool_router: Self::write_router(),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MemorySafeServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Governed memory. `memory_remember` writes and returns the governance decision \
                 — a rejection or a merge is a successful call, not an error. `memory_recall` \
                 returns a working set composed under a token budget, with a reason for every \
                 item selected and every item omitted."
                .to_string(),
        )
    }
}
