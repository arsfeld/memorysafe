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
use axum::http::StatusCode;
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
        // ADD EVERY NEW `.route(...)` ABOVE THIS LINE, NEVER BELOW IT.
        //
        // `Router::fallback` only covers an unmatched *path*. A matched path
        // called with the wrong method (e.g. `POST /v1/health`) never reaches
        // `fallback` — axum's `MethodRouter` answers that itself, by default
        // an empty-bodied 405, bypassing the `Problem` envelope every other
        // failure on this API returns. Nearly invisible today with two GET
        // routes; Tasks 8-10 add POST/PUT/DELETE, where a wrong-method call
        // becomes a real, everyday client mistake.
        .fallback(not_found)
        // Ordering hazard, not just style: `method_not_allowed_fallback`
        // (`axum-0.8.9/src/routing/path_router.rs`'s
        // `PathRouter::method_not_allowed_fallback`, confirmed by reading the
        // vendored source, not just its doc) mutates only the
        // `MethodRouter`s already present in `self.routes` at the moment
        // it's called — `for (_, endpoint) in self.routes.iter_mut() { ...
        // }`, a one-time pass, not a router-wide default that new routes
        // inherit later. A `.route(...)` added BELOW this call silently
        // falls back to axum's own bare, empty-bodied 405 for that one
        // route — with no compile error, no panic, and no test failure
        // unless something specifically POSTs/PUTs/DELETEs that route and
        // checks the body, which is exactly the kind of gap `/v1/health`'s
        // own test (a GET-only route registered above this line) cannot
        // catch for a route added below it.
        .method_not_allowed_fallback(method_not_allowed)
        // `TraceLayer::new_for_http()`'s default span (`DefaultMakeSpan::new()`)
        // does NOT include headers (`include_headers: false`), so the
        // `Authorization` header — and any presented API key — is never
        // recorded. Left at the default deliberately; do not add
        // `.make_span_with(DefaultMakeSpan::new().include_headers(true))`
        // without a redaction step, or every presented key ends up in the
        // trace log.
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

/// A matched path called with a method it doesn't support. Not built through
/// `ApiError`: 405 is a routing artifact axum's `MethodRouter` produces
/// before any handler runs, not one of §9's engine/auth-driven error kinds
/// (`Validation`/`Auth`/`NotFound`/`Conflict`/`Backend`/`PolicyRefused`), so
/// it has no slot in that table. Still answers the same `Problem` shape every
/// other failure on this API does, for the same reason `not_found` does.
async fn method_not_allowed() -> (StatusCode, Json<Problem>) {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(Problem {
            error: "method_not_allowed",
            message: "method not allowed on this route".into(),
            retryable: false,
        }),
    )
}
