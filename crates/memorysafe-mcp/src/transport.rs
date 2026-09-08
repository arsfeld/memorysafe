//! The two real transports: stdio, for a single local client, and streamable
//! HTTP, for many concurrent tenants behind a bearer token.

use crate::MemorySafeServer;
use memorysafe_auth::ScopeResolver;
use memorysafe_engine::Engine;
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService, stdio};
use std::sync::Arc;

/// Serve one client over stdin/stdout and return when it disconnects.
///
/// Nothing may be written to stdout except MCP frames — stdout *is* the
/// transport. Logging goes to stderr; the CLI configures that before calling
/// this.
pub async fn serve_stdio(
    engine: Arc<Engine>,
    resolver: Arc<dyn ScopeResolver>,
) -> anyhow::Result<()> {
    let running = MemorySafeServer::new(engine, resolver)
        .serve(stdio())
        .await?;
    running.waiting().await?;
    Ok(())
}

/// Deployment knobs for the streamable-HTTP transport.
#[derive(Debug, Clone)]
pub struct HttpTransportConfig {
    /// Hostnames or `host:port` authorities this server answers for. Loopback
    /// only by default — accepting any `Host` is a DNS-rebinding hole against
    /// locally running servers.
    pub allowed_hosts: Vec<String>,
}

impl Default for HttpTransportConfig {
    fn default() -> Self {
        Self {
            allowed_hosts: vec!["localhost".into(), "127.0.0.1".into()],
        }
    }
}

pub fn http_service_with(
    engine: Arc<Engine>,
    resolver: Arc<dyn ScopeResolver>,
    config: HttpTransportConfig,
) -> StreamableHttpService<MemorySafeServer, LocalSessionManager> {
    let mut server_config = StreamableHttpServerConfig::default();
    server_config.allowed_hosts = config.allowed_hosts;
    StreamableHttpService::new(
        move || Ok(MemorySafeServer::new(engine.clone(), resolver.clone())),
        Arc::new(LocalSessionManager::default()),
        server_config
            .with_legacy_session_mode(false)
            .with_json_response(true),
    )
}

/// A tower service ready to be mounted, typically at `/mcp`.
pub fn http_service(
    engine: Arc<Engine>,
    resolver: Arc<dyn ScopeResolver>,
) -> StreamableHttpService<MemorySafeServer, LocalSessionManager> {
    http_service_with(engine, resolver, HttpTransportConfig::default())
}
