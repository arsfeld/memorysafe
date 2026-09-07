mod support;

use axum::http::StatusCode;
use memorysafe_auth::{ApiKeyStore, generate};
use memorysafe_core::TenantId;
use serde_json::json;
use support::{get, harness, post, send};

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
async fn a_bearer_scheme_with_no_space_before_the_credential_is_refused() {
    // The scheme boundary: `strip_bearer` requires exactly one space after
    // the scheme (`split_once(' ')`), matching `memorysafe-mcp`'s own test
    // (`http_accepts_the_bearer_scheme_case_insensitively`). Without this, a
    // regression to `strip_prefix("Bearer")` (no trailing space) combined
    // with the existing `.map(str::trim)` would accept `Bearer<key>` as a
    // valid credential and pass every other test in this file, including
    // `the_bearer_scheme_is_accepted_case_insensitively` above — that test's
    // own cases all carry the required space.
    let h = harness();
    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/whoami")
        .header("authorization", format!("Bearer{}", h.key))
        .body(axum::body::Body::empty())
        .unwrap();
    let reply = send(&h.app, request).await;
    assert_eq!(
        reply.status,
        StatusCode::UNAUTHORIZED,
        "a missing space after the scheme must still be refused: {}",
        reply.text
    );
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
    // Not just the status: axum's own default fallback also answers a bare
    // 404 with an empty body, so asserting status alone cannot tell "our
    // `not_found` handler ran" from "no fallback is wired up at all" —
    // deleting `.fallback(not_found)` from `router()` would still pass a
    // status-only assertion. `not_found`'s whole reason to exist (its own
    // doc comment: "a client parsing `Problem` on every failure would choke
    // on the one failure it did not cause") goes untested without this.
    assert_eq!(reply.body["error"], "not_found");
    assert!(reply.body["message"].is_string());
    assert_eq!(reply.body["retryable"], json!(false));
}

#[tokio::test]
async fn a_matched_path_with_the_wrong_method_is_405_with_a_problem_body() {
    // `Router::fallback` (which `an_unknown_route_is_404...` pins) only
    // covers an unmatched *path*. `/v1/health` exists and only answers GET,
    // so POSTing it is the other half of "axum can answer a 4xx before any
    // handler of ours runs": without `.method_not_allowed_fallback(...)`,
    // axum's own default is an empty-bodied 405 that bypasses `Problem`.
    let h = harness();
    let reply = send(&h.app, post("/v1/health", None, json!({}))).await;
    assert_eq!(
        reply.status,
        StatusCode::METHOD_NOT_ALLOWED,
        "{}",
        reply.text
    );
    assert_eq!(reply.body["error"], "method_not_allowed");
    assert!(reply.body["message"].is_string());
    assert_eq!(reply.body["retryable"], json!(false));
}
