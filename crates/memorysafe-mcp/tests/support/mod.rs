// Shared test support, `mod`-included separately by every integration-test
// binary in this crate's `tests/` directory. No single binary uses every
// function here — `http_transport.rs` builds its own server/transport pair
// rather than `connect`'s duplex, and `stdio_transport.rs` needs only
// `engine` — so per-binary `dead_code` would otherwise fire depending on
// which test file is compiling this module.
#![allow(dead_code)]

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Namespace, SubjectId, TenantId};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig};
use memorysafe_mcp::MemorySafeServer;
use memorysafe_policy::BaselinePolicy;
use rmcp::{RoleClient, ServiceExt, service::RunningService};
use std::sync::Arc;
use tempfile::TempDir;

/// Returns the engine alongside the `TempDir` guard for its SQLite backend's
/// on-disk database. The guard must be held by the caller for as long as the
/// engine is in use — its `Drop` removes the directory, which is what
/// actually cleans up after each test now.
///
/// Not `dir.keep()`: that call disables `TempDir`'s cleanup entirely (it
/// exists for callers who want the directory to *outlive* the process), so
/// every test run using it leaked a directory under the OS temp root with no
/// path to ever get it back. Passing `dir.path().to_path_buf()` to
/// `SqliteBackend::open` and returning `dir` itself gets the same
/// still-alive-when-the-engine-needs-it behaviour without the leak — the
/// directory is removed the moment the caller's binding goes out of scope.
pub fn engine() -> (Arc<Engine>, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let backend = SqliteBackend::open(dir.path().to_path_buf());
    let engine = Arc::new(Engine::new(EngineConfig::new(
        Arc::new(backend),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    )));
    (engine, dir)
}

pub fn fixed_resolver() -> std::sync::Arc<dyn memorysafe_auth::ScopeResolver> {
    std::sync::Arc::new(
        memorysafe_auth::FixedScope::new(
            TenantId::new("acme").unwrap(),
            SubjectId::new("user-42").unwrap(),
            Namespace::new("coding-agent").unwrap(),
        )
        .expect("an ordinary test configuration"),
    )
}

/// A real MCP client talking to a real MCP server over an in-memory duplex.
/// Nothing is stubbed: the JSON-RPC framing, the tool schemas, and the
/// structured results all go over the wire the way a client would see them.
pub async fn connect(engine: Arc<Engine>) -> RunningService<RoleClient, ()> {
    let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
    let server = MemorySafeServer::new(engine, fixed_resolver());
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
