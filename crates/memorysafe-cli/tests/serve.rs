use assert_cmd::cargo::cargo_bin;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::json;

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("msafe.toml"), "data_dir = \"tenants\"\n").unwrap();
    dir
}

/// The spec's own acceptance test: a real MCP client driving the real binary
/// over stdio, exactly as an agent host would launch it.
#[tokio::test]
async fn a_real_mcp_client_drives_the_stdio_server() {
    let dir = workspace();
    let transport = TokioChildProcess::new(
        tokio::process::Command::new(cargo_bin("msafe")).configure(|cmd| {
            cmd.current_dir(dir.path())
                .env("MSAFE_TENANT", "acme")
                .env("MSAFE_SUBJECT", "user-42")
                .env("MSAFE_NAMESPACE", "agent")
                .args(["serve", "--transport", "stdio"]);
        }),
    )
    .expect("spawn msafe serve");

    let client = ().serve(transport).await.expect("the server speaks MCP on stdout");

    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 5, "{tools:?}");

    let written = client
        .call_tool(
            CallToolRequestParams::new("memory_remember").with_arguments(
                match json!({ "body": "a memory written through the spawned stdio server" }) {
                    serde_json::Value::Object(map) => map,
                    _ => unreachable!(),
                },
            ),
        )
        .await
        .expect("remember over stdio");
    assert_eq!(
        written.structured_content.as_ref().unwrap()["action"],
        json!("retain")
    );

    client.cancel().await.unwrap();
}

// Found by a consolidated Tasks 11-14 review, reproduced against the built
// binary. `serve --transport stdio` derives its default namespace from the
// working directory (`cmd::serve::namespace_from_cwd`) and never reads a
// `--namespace`/`MSAFE_NAMESPACE` at all — but dispatch used to resolve the
// full three-field scope before reaching any command, so this transport
// rejected on a missing namespace before it ever reached the derivation
// code that exists to run. `a_real_mcp_client_drives_the_stdio_server`
// above could not have caught this: it sets `MSAFE_NAMESPACE`
// unconditionally, which is exactly why a feature that never worked
// shipped looking tested.
#[tokio::test]
async fn stdio_serve_needs_no_namespace_and_still_reaches_the_cwd_derived_default() {
    let dir = workspace();
    let expected_namespace =
        memorysafe_cli::cmd::serve::slug(dir.path().file_name().unwrap().to_str().unwrap());

    let transport = TokioChildProcess::new(
        tokio::process::Command::new(cargo_bin("msafe")).configure(|cmd| {
            cmd.current_dir(dir.path())
                .env("MSAFE_TENANT", "acme")
                .env("MSAFE_SUBJECT", "user-42")
                .env_remove("MSAFE_NAMESPACE")
                .args(["serve", "--transport", "stdio"]);
        }),
    )
    .expect("spawn msafe serve with no MSAFE_NAMESPACE set");

    let client = ()
        .serve(transport)
        .await
        .expect("the server speaks MCP on stdout with no namespace configured");

    let written = client
        .call_tool(
            CallToolRequestParams::new("memory_remember").with_arguments(
                match json!({ "body": "a memory written with no namespace configured" }) {
                    serde_json::Value::Object(map) => map,
                    _ => unreachable!(),
                },
            ),
        )
        .await
        .expect("remember over stdio with no namespace configured");
    assert_eq!(
        written.structured_content.as_ref().unwrap()["action"],
        json!("retain")
    );

    client.cancel().await.unwrap();

    // The item must have actually landed in the cwd-derived namespace, not
    // nowhere and not some other default — proving the derivation code, not
    // just the absence of a startup error, was reached.
    let review = std::process::Command::new(cargo_bin("msafe"))
        .current_dir(dir.path())
        .env("MSAFE_TENANT", "acme")
        .env("MSAFE_SUBJECT", "user-42")
        .env("MSAFE_NAMESPACE", &expected_namespace)
        .args(["--json", "review"])
        .output()
        .unwrap();
    assert!(
        review.status.success(),
        "{}",
        String::from_utf8_lossy(&review.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&review.stdout).unwrap();
    let items = value["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        1,
        "the item did not land in the cwd-derived namespace '{expected_namespace}': {value}"
    );
}

#[tokio::test]
async fn the_http_router_serves_the_api_and_mounts_the_mcp_transport() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    // No `set_current_dir`: that is process-global state and these tests run in
    // parallel. `http_router_for_tests` is given the root explicitly instead.
    let dir = workspace();
    let router = memorysafe_cli::http_router_for_tests(dir.path());

    let health = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    // A GET on the MCP path is not a 404: the transport is mounted and answers
    // for itself, whatever it decides to say about a bare GET.
    let mcp = router
        .oneshot(
            Request::builder()
                .uri("/mcp")
                .header("host", "127.0.0.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        mcp.status(),
        StatusCode::NOT_FOUND,
        "the MCP transport is not mounted"
    );
}
