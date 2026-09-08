//! The `msafe` command line, as a library so integration tests can assemble
//! the same pieces the binary does. `src/main.rs` is a thin `main` over this.

pub mod build;
pub mod cmd;
pub mod config;
pub mod render;

use memorysafe_auth::{ApiKeyScope, ApiKeyStore};
use std::path::Path;
use std::sync::Arc;

/// Assembles the router `msafe serve --transport http` serves, without binding
/// a socket. Exists so an integration test can exercise the composition
/// without spawning a process.
pub fn http_router_for_tests(root: &Path) -> axum::Router {
    let config = config::MsafeConfig {
        root: root.to_path_buf(),
        ..Default::default()
    };
    let engine = build::build_engine(&config).expect("engine");
    let resolver = Arc::new(ApiKeyScope::new(Arc::new(ApiKeyStore::default())));
    cmd::serve::http_router(engine, resolver, &config.serve)
}
