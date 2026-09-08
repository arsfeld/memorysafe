// Shared test support, `mod`-included separately by every integration-test
// binary in this crate's `tests/` directory. This task's own `tests/auth.rs`
// uses only `get`, `harness`, and `send` — `post`, `put`, `delete`, and
// `Harness::engine` exist for Tasks 8-10's route tests, which land in the
// same `tests/` directory afterward — so per-binary `dead_code` would
// otherwise fire depending on which test file is compiling this module.
#![allow(dead_code)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use memorysafe_api::{AppState, router};
use memorysafe_auth::{ApiKeyScope, ApiKeyStore, generate};
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{SubjectId, TenantId};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;
use tower::ServiceExt;

pub struct Harness {
    pub app: Router,
    pub key: String,
    pub engine: Arc<Engine>,
}

pub fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Arc::new(Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    )));
    let g = generate(
        TenantId::new("acme").unwrap(),
        SubjectId::new("user-42").unwrap(),
        "tests",
    )
    .unwrap();
    let key = g.secret.clone();
    let resolver = Arc::new(ApiKeyScope::new(Arc::new(ApiKeyStore::new(vec![g.record]))));
    let app = router(AppState {
        engine: engine.clone(),
        resolver,
    });
    Harness { app, key, engine }
}

pub struct Reply {
    pub status: StatusCode,
    pub body: serde_json::Value,
    pub text: String,
}

pub async fn send(app: &Router, request: Request<Body>) -> Reply {
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("the router responds");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    Reply { status, body, text }
}

pub fn get(uri: &str, key: Option<&str>) -> Request<Body> {
    build("GET", uri, key, Body::empty())
}

pub fn post(uri: &str, key: Option<&str>, json: serde_json::Value) -> Request<Body> {
    let mut request = build("POST", uri, key, Body::from(json.to_string()));
    request
        .headers_mut()
        .insert("content-type", "application/json".parse().unwrap());
    request
}

pub fn put(uri: &str, key: Option<&str>, json: serde_json::Value) -> Request<Body> {
    let mut request = build("PUT", uri, key, Body::from(json.to_string()));
    request
        .headers_mut()
        .insert("content-type", "application/json".parse().unwrap());
    request
}

pub fn delete(uri: &str, key: Option<&str>) -> Request<Body> {
    build("DELETE", uri, key, Body::empty())
}

fn build(method: &str, uri: &str, key: Option<&str>, body: Body) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(key) = key {
        builder = builder.header("authorization", format!("Bearer {key}"));
    }
    builder.body(body).expect("request")
}
