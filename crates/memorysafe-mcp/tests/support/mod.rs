use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Namespace, SubjectId, TenantId};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig};
use memorysafe_mcp::{MemorySafeServer, ScopeSource};
use memorysafe_policy::BaselinePolicy;
use rmcp::{RoleClient, ServiceExt, service::RunningService};
use std::sync::Arc;

pub fn engine() -> Arc<Engine> {
    let dir = tempfile::tempdir().expect("tempdir");
    Arc::new(Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    )))
}

pub fn stdio_source() -> ScopeSource {
    ScopeSource::Stdio {
        tenant: TenantId::new("acme").unwrap(),
        subject: SubjectId::new("user-42").unwrap(),
        default_namespace: Namespace::new("coding-agent").unwrap(),
    }
}

/// A real MCP client talking to a real MCP server over an in-memory duplex.
/// Nothing is stubbed: the JSON-RPC framing, the tool schemas, and the
/// structured results all go over the wire the way a client would see them.
pub async fn connect(engine: Arc<Engine>) -> RunningService<RoleClient, ()> {
    let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
    let server = MemorySafeServer::new(engine, stdio_source());
    tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve");
        let _ = running.waiting().await;
    });
    ().serve(client_transport).await.expect("client connects")
}

pub fn args(pairs: serde_json::Value) -> rmcp::model::JsonObject {
    match pairs {
        serde_json::Value::Object(map) => map,
        other => panic!("tool arguments must be an object, got {other}"),
    }
}
