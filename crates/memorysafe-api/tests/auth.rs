mod support;

use axum::http::StatusCode;
use memorysafe_auth::{ApiKeyStore, generate};
use memorysafe_core::TenantId;
use serde_json::json;
use support::{get, harness, send};

#[tokio::test]
async fn health_needs_no_credential() {
    let h = harness();
    let reply = send(&h.app, get("/v1/health", None)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["status"], "ok");
}

#[tokio::test]
async fn whoami_names_the_tenant_the_key_belongs_to() {
    let h = harness();
    let reply = send(&h.app, get("/v1/whoami", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.body["tenant"], "acme");
    assert!(reply.body["key_id"].is_string());
    assert!(
        !reply.text.contains(&h.key),
        "whoami echoed the credential back"
    );
}

#[tokio::test]
async fn a_request_with_no_credential_is_401_and_says_so_in_a_problem_body() {
    let h = harness();
    let reply = send(&h.app, get("/v1/whoami", None)).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.body["error"], "unauthenticated");
    assert!(reply.body["message"].is_string());
    assert_eq!(reply.body["retryable"], json!(false));
}

#[tokio::test]
async fn a_credential_that_is_not_a_bearer_token_is_401() {
    let h = harness();
    for header in [
        "",
        "Basic abc",
        "Bearer",
        "Bearer   ",
        "bearer lowercase-scheme",
    ] {
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/v1/whoami")
            .header("authorization", header)
            .body(axum::body::Body::empty())
            .unwrap();
        let reply = send(&h.app, request).await;
        assert_eq!(
            reply.status,
            StatusCode::UNAUTHORIZED,
            "header {header:?} was accepted"
        );
    }
}

#[tokio::test]
async fn the_bearer_scheme_is_accepted_case_insensitively() {
    // RFC 7235 auth schemes are case-insensitive, so `bearer <token>` is a
    // valid credential. `memorysafe-mcp`'s `scope::strip_bearer` already
    // gets this right; this pins the same behaviour here, on the success
    // path — none of this file's other cases present a *valid* key under a
    // non-canonical scheme, so without this test a regression to a
    // case-sensitive `strip_prefix("Bearer ")` would pass every other test
    // in this file.
    let h = harness();
    for scheme in ["Bearer", "bearer", "BEARER", "BeArEr"] {
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/v1/whoami")
            .header("authorization", format!("{scheme} {}", h.key))
            .body(axum::body::Body::empty())
            .unwrap();
        let reply = send(&h.app, request).await;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "scheme {scheme:?} was refused: {}",
            reply.text
        );
        assert_eq!(reply.body["tenant"], "acme");
    }
}

#[tokio::test]
async fn an_unknown_key_is_401_not_403() {
    // 403 would tell the caller the key is real. 401 says "identify yourself",
    // which is the truth and leaks nothing.
    let h = harness();
    let stranger = generate(TenantId::new("acme").unwrap(), "not in this store").unwrap();
    let reply = send(&h.app, get("/v1/whoami", Some(&stranger.secret))).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_disabled_key_is_403_because_the_caller_is_known() {
    let dir = tempfile::tempdir().unwrap();
    let engine = std::sync::Arc::new(memorysafe_engine::Engine::new(
        memorysafe_engine::EngineConfig::new(
            std::sync::Arc::new(memorysafe_backend_sqlite::SqliteBackend::open(dir.keep())),
            std::sync::Arc::new(memorysafe_embed::DeterministicEmbedder::new(256)),
            std::sync::Arc::new(memorysafe_policy::BaselinePolicy::default()),
        ),
    ));
    let g = generate(TenantId::new("acme").unwrap(), "revoked").unwrap();
    let mut record = g.record;
    record.disabled = true;
    let app = memorysafe_api::router(memorysafe_api::AppState {
        engine,
        keys: std::sync::Arc::new(ApiKeyStore::new(vec![record])),
    });

    let reply = send(&app, get("/v1/whoami", Some(&g.secret))).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.body["error"], "forbidden");
}

#[tokio::test]
async fn an_unknown_route_is_404_with_a_problem_body() {
    let h = harness();
    let reply = send(&h.app, get("/v1/nope", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}
