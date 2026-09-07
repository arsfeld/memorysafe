mod support;

use rmcp::model::CallToolRequestParams;
use serde_json::json;
use support::{args, connect, engine};

fn structured(result: &rmcp::model::CallToolResult) -> &serde_json::Value {
    result
        .structured_content
        .as_ref()
        .expect("every MemorySafe tool returns structured content")
}

#[tokio::test]
async fn the_server_advertises_the_write_tools_with_schemas() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();

    assert!(names.contains(&"memory_remember"), "{names:?}");
    assert!(names.contains(&"memory_recall"), "{names:?}");
    for tool in &tools {
        assert!(
            tool.description.as_ref().is_some_and(|d| !d.is_empty()),
            "tool {} has no description for the model to read",
            tool.name
        );
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn remembering_returns_the_governance_decision_not_just_an_id() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_remember").with_arguments(args(json!({
                "body": "the production database migration runs on Sundays"
            }))),
        )
        .await
        .unwrap();

    let value = structured(&result);
    assert_eq!(value["action"], "retain");
    assert!(value["item_id"].is_string());
    assert!(value["audit_id"].is_string());
    assert!(
        value["reasons"].as_array().is_some_and(|r| !r.is_empty()),
        "a decision with no reason is not governance: {value}"
    );
    assert!(value["reasons"][0]["code"].is_string());
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_rejected_duplicate_is_a_successful_tool_call() {
    // An agent learning its memory was redundant is the product working. If
    // this surfaces as a tool error, every client will retry it forever.
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    let body = "the deploy key rotates every ninety days";
    for _ in 0..2 {
        let result = client
            .call_tool(
                CallToolRequestParams::new("memory_remember")
                    .with_arguments(args(json!({ "body": body }))),
            )
            .await
            .expect("the call itself succeeds");
        assert_ne!(
            result.is_error,
            Some(true),
            "a governance decision became an error"
        );
    }

    let second = client
        .call_tool(
            CallToolRequestParams::new("memory_remember")
                .with_arguments(args(json!({ "body": body }))),
        )
        .await
        .unwrap();
    let value = structured(&second);
    assert!(
        value["action"] == "reject" || value["action"] == "merge",
        "an identical rewrite was admitted again: {value}"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn an_empty_body_is_a_tool_error_because_nothing_was_decided() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_remember")
                .with_arguments(args(json!({ "body": "   " }))),
        )
        .await;
    assert!(
        result.is_err(),
        "a validation failure must not look like a decision"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn recall_returns_a_governed_working_set_with_its_audit_id() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    for body in [
        "the production migration runs on Sundays",
        "the on-call rotation starts Monday morning",
        "the staging cluster is rebuilt every night",
    ] {
        client
            .call_tool(
                CallToolRequestParams::new("memory_remember")
                    .with_arguments(args(json!({ "body": body }))),
            )
            .await
            .unwrap();
    }

    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_recall").with_arguments(args(json!({
                "query": "when does the migration run",
                "max_items": 2
            }))),
        )
        .await
        .unwrap();

    let value = structured(&result);
    assert!(
        value["audit_id"].is_string(),
        "every recall is audited: {value}"
    );
    let items = value["items"].as_array().expect("items array");
    assert!(
        !items.is_empty(),
        "a matching query returned nothing: {value}"
    );
    assert!(items.len() <= 2, "the budget was ignored: {value}");
    for item in items {
        assert!(item["id"].is_string());
        assert!(item["body"].is_string());
        assert!(
            item["reason_code"].is_string(),
            "each selection says why it is there"
        );
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn recall_over_an_empty_namespace_is_an_empty_success() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_recall").with_arguments(args(json!({
                "query": "anything at all",
                "namespace": "somewhere-else"
            }))),
        )
        .await
        .unwrap();

    let value = structured(&result);
    assert_eq!(value["items"].as_array().unwrap().len(), 0);
    assert!(
        value["audit_id"].is_string(),
        "an empty recall is still audited"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn the_default_sensitivity_ceiling_fails_closed() {
    // The plan's own text specified `restricted` (which excludes nothing) as
    // `memory_recall`'s default `sensitivity_ceiling`. The controller ruled
    // it should be `internal` instead, to match
    // `memorysafe_backend::query::HardFilters::default`'s own documented
    // `// Fail closed.` comment for this exact field -- a deliberate
    // deviation from the plan's literal text, made because
    // `memorysafe-engine`'s `read.rs` builds `HardFilters` as a struct
    // literal from whatever this adapter supplies, so whatever this crate
    // defaults to is what actually runs in SQL, not `HardFilters`'s own
    // default.
    //
    // Nothing else in this workspace pinned that default before this test:
    // reverting `tools_write.rs`'s `.unwrap_or(SensitivityLevel::Internal)`
    // back to `Restricted` failed zero other tests. This test exists so a
    // silent revert of a security-relevant governance default does not pass
    // review a second time.
    let (eng, _dir) = engine();
    let client = connect(eng).await;

    // `sensitivity_hint` can only RAISE the level `BaselinePolicy` detects,
    // never lower it (`SensitivityLevel::raised_by`), and
    // `memorysafe-engine`'s `write.rs` applies that unconditionally,
    // regardless of what the policy assessed ("Applied by the ENGINE, not
    // trusted from the policy" — see that file's comment on this exact
    // line). Passing the maximum level as the hint therefore guarantees the
    // item's STORED sensitivity is `restricted`, deterministically,
    // regardless of what the detectors would have assigned to this
    // ordinary-looking body on their own — this test does not need to
    // guess or inspect detector behaviour to know what got stored.
    let write = client
        .call_tool(
            CallToolRequestParams::new("memory_remember").with_arguments(args(json!({
                "body": "the quarterly planning notes are attached",
                "sensitivity_hint": "restricted"
            }))),
        )
        .await
        .unwrap();
    let write_value = structured(&write);
    assert_eq!(
        write_value["action"], "retain",
        "the item was not retained, so this test cannot prove anything about recall: {write_value}"
    );

    // Step 2, the assertion that pins the default: no `sensitivity_ceiling`
    // named, so the default (`internal`) applies, and a `restricted` item
    // must not come back.
    let default_ceiling = client
        .call_tool(
            CallToolRequestParams::new("memory_recall").with_arguments(args(json!({
                "query": "quarterly planning notes"
            }))),
        )
        .await
        .unwrap();
    let default_value = structured(&default_ceiling);
    assert_eq!(
        default_value["items"].as_array().unwrap().len(),
        0,
        "a restricted item was returned under the default sensitivity ceiling: {default_value}"
    );

    // Step 3 proves step 2 failed for the right reason. If the write had
    // silently failed to persist, or the query missed the item for some
    // unrelated reason, step 2 would also show zero items — vacuously
    // "passing" a broken default exactly as readily as a correct one.
    // Naming a wide ceiling explicitly must still find the same item.
    let wide_ceiling = client
        .call_tool(
            CallToolRequestParams::new("memory_recall").with_arguments(args(json!({
                "query": "quarterly planning notes",
                "sensitivity_ceiling": "restricted"
            }))),
        )
        .await
        .unwrap();
    let wide_value = structured(&wide_ceiling);
    assert!(
        !wide_value["items"].as_array().unwrap().is_empty(),
        "the item was not found even with an explicit wide ceiling -- step 2's empty \
         result was not actually caused by the sensitivity ceiling: {wide_value}"
    );

    client.cancel().await.unwrap();
}

#[tokio::test]
async fn occurred_at_is_reachable_through_the_recall_time_filters() {
    // `RememberParams` had no `occurred_at` field: every item written
    // through MCP got `occurred_at: None`, and `memorysafe-backend`'s
    // `query.rs` documents that `NULL >= x` / `NULL <= x` are `NULL`, so a
    // recall bound never matches a `None` -- the time-filter machinery
    // `RecallRequest` exists to expose was present but unreachable from any
    // MCP caller. Proves it end to end, both directions: a window that
    // brackets the written `occurred_at` must find it, and a window that
    // does not must not -- the second half is what rules out "the filter is
    // silently ignored and this always finds everything by text alone."
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    let anchor: i64 = 1_700_000_000;

    client
        .call_tool(
            CallToolRequestParams::new("memory_remember").with_arguments(args(json!({
                "body": "the anchor incident happened during the maintenance window",
                "occurred_at": anchor
            }))),
        )
        .await
        .unwrap();

    let bracketed = client
        .call_tool(
            CallToolRequestParams::new("memory_recall").with_arguments(args(json!({
                "query": "the anchor incident maintenance window",
                "occurred_after": anchor - 60,
                "occurred_before": anchor + 60
            }))),
        )
        .await
        .unwrap();
    let bracketed_value = structured(&bracketed);
    assert!(
        !bracketed_value["items"]
            .as_array()
            .expect("items array")
            .is_empty(),
        "a window bracketing occurred_at did not find the item: {bracketed_value}"
    );

    let missed = client
        .call_tool(
            CallToolRequestParams::new("memory_recall").with_arguments(args(json!({
                "query": "the anchor incident maintenance window",
                "occurred_after": anchor + 3600,
                "occurred_before": anchor + 7200
            }))),
        )
        .await
        .unwrap();
    let missed_value = structured(&missed);
    assert_eq!(
        missed_value["items"].as_array().unwrap().len(),
        0,
        "a window that does not bracket occurred_at still matched: {missed_value}"
    );

    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_stdio_client_cannot_switch_subject() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_remember").with_arguments(args(json!({
                "body": "a memory for someone else",
                "subject": "another-user"
            }))),
        )
        .await;
    assert!(
        result.is_err(),
        "stdio let a client name a different subject"
    );
    client.cancel().await.unwrap();
}
