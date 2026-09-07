mod support;

use memorysafe_auth::{ApiKeyStore, generate};
use memorysafe_core::TenantId;
use memorysafe_mcp::{ScopeSource, http_service};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ReadResourceRequestParams};
use rmcp::transport::{
    StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
};
use serde_json::json;
use std::sync::Arc;
use support::{args, engine};
use tokio_util::sync::CancellationToken;

struct Served {
    address: std::net::SocketAddr,
    ct: CancellationToken,
    secret: String,
    // Held for the lifetime of the test, not read: the SQLite backend's
    // on-disk database lives under this directory, and the spawned server
    // task below keeps using it until `ct` is cancelled. Naming it `_dir`
    // (never `_`) keeps it bound for `Served`'s whole lifetime rather than
    // dropped — and therefore deleted out from under the running server —
    // at the end of this function. See `support::engine`'s own doc for why
    // this guard must be held by name.
    _dir: tempfile::TempDir,
}

async fn serve() -> Served {
    let tenant = TenantId::new("acme").unwrap();
    let g = generate(tenant, "integration").unwrap();
    let secret = g.secret.clone();
    let keys = Arc::new(ApiKeyStore::new(vec![g.record]));
    let (eng, _dir) = engine();

    let ct = CancellationToken::new();
    let service = http_service(eng, ScopeSource::Http { keys });
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn({
        let ct = ct.clone();
        async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { ct.cancelled_owned().await })
                .await;
        }
    });
    Served {
        address,
        ct,
        secret,
        _dir,
    }
}

/// `credential` is the bare secret, not a full `Authorization` header value:
/// rmcp's reqwest client sends `StreamableHttpClientTransportConfig::auth_header`
/// through `RequestBuilder::bearer_auth`, which itself prepends `"Bearer "`
/// (confirmed against `rmcp-3.2.0/src/transport/common/reqwest/streamable_http_client.rs`).
/// Passing an already-prefixed value here produces `Authorization: Bearer
/// Bearer msk_...` on the wire, which fails `memorysafe_auth`'s
/// `parse_presented` and every call looks like a bad credential — caught by
/// running this suite once with the field misused this way and reading the
/// server's own "credential is not a MemorySafe API key" error back.
fn transport(
    address: std::net::SocketAddr,
    credential: Option<String>,
) -> StreamableHttpClientTransport<reqwest::Client> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(format!("http://{address}/mcp"));
    config.auth_header = credential;
    config.allow_stateless = true;
    StreamableHttpClientTransport::from_config(config)
}

/// Every await in this file is wrapped so a wedged client cannot hang the
/// suite — see the plan's own warning that a network-bound test can hang
/// rather than fail.
async fn with_timeout<F: std::future::Future>(fut: F) -> F::Output {
    tokio::time::timeout(std::time::Duration::from_secs(10), fut)
        .await
        .expect("operation timed out")
}

#[tokio::test]
async fn an_authenticated_client_writes_and_reads_over_streamable_http() {
    let served = serve().await;
    let client = with_timeout(().serve(transport(served.address, Some(served.secret.clone()))))
        .await
        .expect("client connects");

    let tools = with_timeout(client.list_all_tools()).await.unwrap();
    assert_eq!(tools.len(), 5);

    let written = with_timeout(client.call_tool(
        CallToolRequestParams::new("memory_remember").with_arguments(args(json!({
            "body": "the http transport carries a request header through to scope resolution",
            "subject": "user-42",
            "namespace": "agent"
        }))),
    ))
    .await
    .expect("remember over http");
    assert_eq!(
        written.structured_content.as_ref().unwrap()["action"],
        json!("retain")
    );

    let recalled = with_timeout(client.call_tool(
        CallToolRequestParams::new("memory_recall").with_arguments(args(json!({
            "query": "how does the http transport carry a header to scope resolution",
            "subject": "user-42",
            "namespace": "agent"
        }))),
    ))
    .await
    .expect("recall over http");
    assert!(
        !recalled.structured_content.as_ref().unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    with_timeout(client.cancel()).await.unwrap();
    served.ct.cancel();
}

#[tokio::test]
async fn a_client_with_no_credential_cannot_call_a_tool() {
    let served = serve().await;
    let client = with_timeout(().serve(transport(served.address, None)))
        .await
        .expect("client connects");

    // Listing is public; acting is not. The tool call must fail because scope
    // resolution has no tenant to work with.
    let result = with_timeout(client.call_tool(
        CallToolRequestParams::new("memory_remember").with_arguments(args(json!({
            "body": "this must not be stored",
            "subject": "user-42",
            "namespace": "agent"
        }))),
    ))
    .await;
    assert!(result.is_err(), "an unauthenticated write was accepted");

    with_timeout(client.cancel()).await.unwrap();
    served.ct.cancel();
}

#[tokio::test]
async fn a_key_for_one_tenant_cannot_be_used_as_another() {
    let served = serve().await;
    let stranger = generate(TenantId::new("globex").unwrap(), "stranger").unwrap();
    let client = with_timeout(().serve(transport(served.address, Some(stranger.secret.clone()))))
        .await
        .expect("client connects");

    let result = with_timeout(client.call_tool(
        CallToolRequestParams::new("memory_remember").with_arguments(args(json!({
            "body": "a memory for a tenant this server has never heard of",
            "subject": "user-42",
            "namespace": "agent"
        }))),
    ))
    .await;
    assert!(result.is_err(), "a key from another store authenticated");

    with_timeout(client.cancel()).await.unwrap();
    served.ct.cancel();
}

/// Task 5 left this gap on the record: `ScopeSource::Http` had never been
/// driven through a real transport, so "can a caller read another tenant's
/// audit trail by naming it in a resource URI" had never actually been
/// tried against the path that matters — over HTTP, where the tenant comes
/// from the authenticated key, not from server config. `read_resource`
/// resolves subject/namespace from the URI but takes the tenant from
/// `ScopeSource::resolve` (the key), then refuses when the URI's tenant
/// segment disagrees. This is the refusal half; the next test is the
/// success half, so this cannot pass merely because every read errors.
#[tokio::test]
async fn a_client_cannot_read_another_tenants_resource_by_naming_it_in_the_uri_over_http() {
    let served = serve().await;
    let client = with_timeout(().serve(transport(served.address, Some(served.secret.clone()))))
        .await
        .expect("client connects");

    // The key authenticates tenant "acme"; the URI names tenant "globex".
    // A `200` here would mean rows resolved under one tenant's connection
    // came back labelled with another tenant's URI.
    let denied = with_timeout(client.read_resource(ReadResourceRequestParams::new(
        "memorysafe://globex/user-42/agent/audit",
    )))
    .await;
    let err = denied.expect_err("a resource URI crossed a tenant boundary over http");
    // Pin the reason, not just the outcome: `read_resource`'s tenant-mismatch
    // guard returns exactly this message. Without checking it, an unrelated
    // failure — a broken route, a session that never established, another
    // auth regression — could masquerade as this test still passing.
    assert!(
        format!("{err:?}").contains("no such resource"),
        "expected a tenant-mismatch refusal (\"no such resource\"), got: {err:?}"
    );

    with_timeout(client.cancel()).await.unwrap();
    served.ct.cancel();
}

/// The success half of the pair above: an authenticated caller reading a
/// resource under its *own* tenant must still work. Without this, the
/// refusal test could pass hollowly if every resource read over HTTP
/// errored regardless of tenant.
#[tokio::test]
async fn a_client_can_read_its_own_tenants_resource_over_http() {
    let served = serve().await;
    let client = with_timeout(().serve(transport(served.address, Some(served.secret.clone()))))
        .await
        .expect("client connects");

    with_timeout(client.call_tool(
        CallToolRequestParams::new("memory_remember").with_arguments(args(json!({
            "body": "a memory read back through the resource, not the tool",
            "subject": "user-42",
            "namespace": "agent"
        }))),
    ))
    .await
    .expect("remember over http");

    let read = with_timeout(client.read_resource(ReadResourceRequestParams::new(
        "memorysafe://acme/user-42/agent/audit",
    )))
    .await
    .expect("a same-tenant resource read must succeed");
    let rmcp::model::ResourceContents::TextResourceContents { text, .. } = &read.contents[0] else {
        panic!("expected text contents");
    };
    let value: serde_json::Value = serde_json::from_str(text).expect("audit resource is JSON");
    assert_eq!(
        value["records"].as_array().unwrap().len(),
        1,
        "the remembered item's audit row must be visible to its own tenant: {value}"
    );

    with_timeout(client.cancel()).await.unwrap();
    served.ct.cancel();
}
