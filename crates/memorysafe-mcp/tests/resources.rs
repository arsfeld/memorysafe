mod support;

use rmcp::model::{CallToolRequestParams, ReadResourceRequestParams};
use serde_json::json;
use support::{args, connect, engine};

#[tokio::test]
async fn the_server_lists_its_audit_and_stats_resources() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    let listed = client.list_resources(None).await.unwrap();
    let uris: Vec<&str> = listed.resources.iter().map(|r| r.uri.as_str()).collect();

    assert!(
        uris.contains(&"memorysafe://acme/user-42/coding-agent/audit"),
        "{uris:?}"
    );
    assert!(
        uris.contains(&"memorysafe://acme/user-42/coding-agent/stats"),
        "{uris:?}"
    );
    for resource in &listed.resources {
        assert_eq!(resource.mime_type.as_deref(), Some("application/json"));
    }

    let templates = client.list_resource_templates(None).await.unwrap();
    assert_eq!(
        templates.resource_templates.len(),
        2,
        "a client on HTTP has no default scope and needs the templates"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn the_audit_resource_shows_the_decisions_without_the_bodies() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    client
        .call_tool(
            CallToolRequestParams::new("memory_remember")
                .with_arguments(args(json!({ "body": "a memory whose body must not leak" }))),
        )
        .await
        .unwrap();

    let read = client
        .read_resource(ReadResourceRequestParams::new(
            "memorysafe://acme/user-42/coding-agent/audit",
        ))
        .await
        .unwrap();

    let text = match &read.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
        other => panic!("expected text contents, got {other:?}"),
    };
    let value: serde_json::Value = serde_json::from_str(&text).expect("the audit resource is JSON");
    let records = value["records"].as_array().expect("records array");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["event"], "admitted");
    assert!(
        records[0]["decision"].is_object(),
        "the decision is the point of the trail"
    );
    assert!(
        !text.contains("a memory whose body must not leak"),
        "the audit resource leaked an item body"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn the_stats_resource_reports_capacity_and_corpus_shape() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    for i in 0..3 {
        client
            .call_tool(
                CallToolRequestParams::new("memory_remember").with_arguments(args(
                    json!({ "body": format!("distinct memory {i} on topic {i}") }),
                )),
            )
            .await
            .unwrap();
    }

    let read = client
        .read_resource(ReadResourceRequestParams::new(
            "memorysafe://acme/user-42/coding-agent/stats",
        ))
        .await
        .unwrap();
    let text = match &read.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
        other => panic!("expected text contents, got {other:?}"),
    };
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();

    assert_eq!(value["item_count"], json!(3));
    assert!(value["used_bytes"].as_u64().is_some_and(|b| b > 0));
    assert!(value["total_bytes"].as_u64().is_some());
    assert!(value["budget"].is_object());
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_resource_in_another_subject_is_refused_over_stdio() {
    // Same rule as the tools: stdio is bound to one subject. A resource URI is
    // not a way around it.
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    let denied = client
        .read_resource(ReadResourceRequestParams::new(
            "memorysafe://acme/someone-else/coding-agent/audit",
        ))
        .await;
    assert!(denied.is_err(), "a resource URI crossed a subject boundary");

    let other_tenant = client
        .read_resource(ReadResourceRequestParams::new(
            "memorysafe://globex/user-42/coding-agent/audit",
        ))
        .await;
    assert!(
        other_tenant.is_err(),
        "a resource URI crossed a tenant boundary"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn an_unknown_resource_uri_is_an_error_not_an_empty_document() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    assert!(
        client
            .read_resource(ReadResourceRequestParams::new(
                "memorysafe://acme/user-42/agent/secrets"
            ))
            .await
            .is_err()
    );
    assert!(
        client
            .read_resource(ReadResourceRequestParams::new("file:///etc/passwd"))
            .await
            .is_err()
    );
    client.cancel().await.unwrap();
}
