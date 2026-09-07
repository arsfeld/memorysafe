//! The HTTP adapter: a thin axum mirror of the engine.
//!
//! Authentication is `Authorization: Bearer <api-key>`, one key to one tenant.
//! Subject and namespace arrive per request and are validated against the key's
//! tenant. A governance decision is never an HTTP error.

pub mod auth;
pub mod error;
pub mod query;
pub mod scope;

pub use error::{ApiError, Problem};
pub use query::ValidatedQuery;
pub use scope::ScopeParams;

use crate::auth::Auth;
use axum::Json;
use axum::Router;
use axum::routing::get;
use memorysafe_auth::ApiKeyStore;
use memorysafe_engine::Engine;
use std::sync::Arc;
use tower_http::trace::TraceLayer;

#[derive(Clone)]
pub struct AppState {
    pub engine: Arc<Engine>,
    pub keys: Arc<ApiKeyStore>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/whoami", get(whoami))
        .fallback(not_found)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Deliberately unauthenticated: a load balancer must be able to ask, and the
/// answer names no tenant and reveals no memory.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

/// Which tenant this credential is. A client holding a key it did not create
/// otherwise has no way to find out, and this is also the smallest route that
/// exercises authentication end to end.
async fn whoami(auth: Auth) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "tenant": auth.tenant().to_string(),
        "key_id": auth.0.key_id(),
    }))
}

/// Without this an unknown path returns an empty 404 with no body, and a client
/// parsing `Problem` on every failure would choke on the one failure it did not
/// cause.
async fn not_found() -> ApiError {
    ApiError::NotFound("no such route".into())
}
