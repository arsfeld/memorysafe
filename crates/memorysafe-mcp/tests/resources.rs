mod support;

use rmcp::model::{CallToolRequestParams, ReadResourceRequestParams};
use serde_json::json;
use support::{args, connect, engine};

/// Every read in this file needs both the text and the content block's own
/// `mimeType` — the field a client actually dispatches rendering on, as
/// distinct from the listed `Resource`'s `mimeType` (already checked in
/// `the_server_lists_its_audit_and_stats_resources`).
fn text_and_mime(contents: &rmcp::model::ResourceContents) -> (String, Option<String>) {
    match contents {
        rmcp::model::ResourceContents::TextResourceContents {
            text, mime_type, ..
        } => (text.clone(), mime_type.clone()),
        other => panic!("expected text contents, got {other:?}"),
    }
}

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

    let (text, mime_type) = text_and_mime(&read.contents[0]);
    assert_eq!(
        mime_type.as_deref(),
        Some("application/json"),
        "the content block's own mimeType, not just the listing's, must say JSON"
    );
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

/// `AuditFilter::default().limit` caps the audit resource at 100 rows, and
/// this workspace's convention (documented on `AuditFilter::limit`) is that
/// truncation is detectable *only* via `returned.len() < limit` — a client
/// needs the limit to apply that rule at all. Without this field the
/// resource silently discarded the one number the convention depends on.
#[tokio::test]
async fn the_audit_resource_reports_its_effective_limit() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    client
        .call_tool(
            CallToolRequestParams::new("memory_remember")
                .with_arguments(args(json!({ "body": "one audited memory" }))),
        )
        .await
        .unwrap();

    let read = client
        .read_resource(ReadResourceRequestParams::new(
            "memorysafe://acme/user-42/coding-agent/audit",
        ))
        .await
        .unwrap();
    let (text, _) = text_and_mime(&read.contents[0]);
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();

    assert_eq!(
        value["limit"],
        json!(memorysafe_core::AuditFilter::default().limit),
        "the resource's envelope: {value}"
    );
    // Keep the existing keys alongside the new one, not in place of them.
    assert!(value["records"].is_array());
    assert!(value["scope"].is_object());
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn the_stats_resource_reports_capacity_and_corpus_shape() {
    let (eng, _dir) = engine();
    let client = connect(eng.clone()).await;

    // Deliberately different lengths, not "distinct memory {i}" (which are
    // all the same size): a used_bytes/used_items or item_count/total_bytes
    // transposition must fail loudly, not coincidentally match a small item
    // count.
    let bodies = [
        "a short memory about topic zero".to_string(),
        "a considerably longer memory body describing topic one, with extra \
         clauses and context so its byte count clearly differs from the first"
            .to_string(),
        "an even longer memory body about topic two, deliberately padded with \
         additional distinct words and phrases so this item's byte size is the \
         largest of the three and the median sits between the other two without \
         coinciding with any count"
            .to_string(),
    ];
    for body in &bodies {
        client
            .call_tool(
                CallToolRequestParams::new("memory_remember")
                    .with_arguments(args(json!({ "body": body }))),
            )
            .await
            .unwrap();
    }

    // The expected values come from the engine directly, not from hand
    // computing the byte-charge formula — this test's job is to check the
    // resource relays the engine's own numbers under the right keys, not to
    // re-derive `MemoryItem::byte_size`'s arithmetic.
    let scope = memorysafe_core::Scope::new("acme", "user-42", "coding-agent").unwrap();
    let expected_capacity = eng.capacity_state(&scope).await.unwrap();
    let expected_stats = eng.scope_stats(&scope).await.unwrap();
    // The fixture must actually discriminate a transposition, or the
    // assertions below would pass by coincidence.
    assert_ne!(expected_capacity.used_items, expected_capacity.used_bytes);
    assert_ne!(expected_stats.item_count, expected_stats.total_bytes);
    assert_ne!(expected_stats.total_bytes, expected_stats.median_item_bytes);

    let read = client
        .read_resource(ReadResourceRequestParams::new(
            "memorysafe://acme/user-42/coding-agent/stats",
        ))
        .await
        .unwrap();
    let (text, mime_type) = text_and_mime(&read.contents[0]);
    assert_eq!(
        mime_type.as_deref(),
        Some("application/json"),
        "the content block's own mimeType, not just the listing's, must say JSON"
    );
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();

    let mut keys: Vec<&str> = value
        .as_object()
        .expect("stats resource is a JSON object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "budget",
            "item_count",
            "mean_neighbour_similarity",
            "median_item_bytes",
            "scope",
            "total_bytes",
            "used_bytes",
            "used_items",
        ],
        "the exact key set, so deleting or renaming one is caught"
    );

    assert_eq!(value["scope"], serde_json::to_value(&scope).unwrap());
    assert_eq!(value["used_items"], json!(expected_capacity.used_items));
    assert_eq!(value["used_bytes"], json!(expected_capacity.used_bytes));
    assert_eq!(value["item_count"], json!(expected_stats.item_count));
    assert_eq!(value["total_bytes"], json!(expected_stats.total_bytes));
    assert_eq!(
        value["median_item_bytes"],
        json!(expected_stats.median_item_bytes)
    );
    assert!(value["budget"].is_object());
    // The backend never computes a real mean-neighbour-similarity figure
    // (only the engine's in-flight assessment path does, and this read does
    // not go through it) — publishing its `0.0` "no data" sentinel as though
    // it were a measurement would misread as "this corpus sits at minimum
    // similarity". `null` says "not available from this read" instead. See
    // `lib.rs`'s `read_resource` for the full explanation.
    assert_eq!(value["mean_neighbour_similarity"], serde_json::Value::Null);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn an_empty_scope_reads_as_a_successful_empty_document_not_an_error() {
    // A governance decision is not an error, and neither is the absence of
    // one: an empty audit trail and an empty scope's stats are both
    // successful reads. This is the deliberate converse of
    // `an_unknown_resource_uri_is_an_error_not_an_empty_document` — the pair
    // a refactor most easily collapses the wrong way ("no rows, so 404").
    let (eng, _dir) = engine();
    let client = connect(eng).await;

    let audit = client
        .read_resource(ReadResourceRequestParams::new(
            "memorysafe://acme/user-42/coding-agent/audit",
        ))
        .await
        .expect("an empty audit trail is a successful read");
    let (audit_text, _) = text_and_mime(&audit.contents[0]);
    let audit_value: serde_json::Value = serde_json::from_str(&audit_text).unwrap();
    assert_eq!(audit_value["records"], json!([]));

    let stats = client
        .read_resource(ReadResourceRequestParams::new(
            "memorysafe://acme/user-42/coding-agent/stats",
        ))
        .await
        .expect("an empty scope's stats are a successful read");
    let (stats_text, _) = text_and_mime(&stats.contents[0]);
    let stats_value: serde_json::Value = serde_json::from_str(&stats_text).unwrap();
    assert_eq!(stats_value["item_count"], json!(0));
    assert_eq!(stats_value["total_bytes"], json!(0));
    assert_eq!(stats_value["median_item_bytes"], json!(0));
    assert_eq!(stats_value["used_items"], json!(0));
    assert_eq!(stats_value["used_bytes"], json!(0));

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
