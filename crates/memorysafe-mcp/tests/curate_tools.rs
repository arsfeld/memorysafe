mod support;

use rmcp::model::CallToolRequestParams;
use serde_json::json;
use support::{args, connect, engine};

fn structured(result: &rmcp::model::CallToolResult) -> &serde_json::Value {
    result
        .structured_content
        .as_ref()
        .expect("structured content")
}

async fn remember(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    body: &str,
    tag: &str,
) -> String {
    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_remember")
                .with_arguments(args(json!({ "body": body, "tags": [tag] }))),
        )
        .await
        .unwrap();
    structured(&result)["item_id"]
        .as_str()
        .expect("an admitted id")
        .to_owned()
}

#[tokio::test]
async fn the_server_advertises_all_five_tools() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    let names: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    for expected in [
        "memory_recall",
        "memory_remember",
        "memory_forget",
        "memory_review",
        "memory_protect",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "missing {expected} in {names:?}"
        );
    }
    assert_eq!(names.len(), 5, "the surface stays small: {names:?}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn review_lists_what_is_stored_and_why() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    remember(&client, "the release train leaves on Thursdays", "process").await;
    remember(
        &client,
        "the incident review template lives in the wiki",
        "process",
    )
    .await;

    let result = client
        .call_tool(CallToolRequestParams::new("memory_review").with_arguments(args(json!({}))))
        .await
        .unwrap();

    let value = structured(&result);
    let items = value["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    for item in items {
        assert!(item["body"].is_string());
        assert!(item["sensitivity"].is_string());
        assert!(item["protection"].is_string());
        assert!(
            item["reason_code"].is_string(),
            "review must say why an item is stored: {item}"
        );
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn review_pages() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    for i in 0..5 {
        remember(
            &client,
            &format!("distinct memory {i} about topic {i}"),
            "bulk",
        )
        .await;
    }

    let page = client
        .call_tool(
            CallToolRequestParams::new("memory_review")
                .with_arguments(args(json!({ "limit": 2, "offset": 2 }))),
        )
        .await
        .unwrap();
    assert_eq!(structured(&page)["items"].as_array().unwrap().len(), 2);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn forgetting_by_id_removes_exactly_that_memory() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    let doomed = remember(&client, "a memory to delete", "work").await;
    remember(&client, "a memory to keep for later", "work").await;

    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_forget")
                .with_arguments(args(json!({ "ids": [doomed.clone()] }))),
        )
        .await
        .unwrap();

    let value = structured(&result);
    assert_eq!(value["forgotten"], json!([doomed]));
    assert!(value["audit_id"].is_string());

    let left = client
        .call_tool(CallToolRequestParams::new("memory_review").with_arguments(args(json!({}))))
        .await
        .unwrap();
    assert_eq!(structured(&left)["items"].as_array().unwrap().len(), 1);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn forgetting_by_tag_removes_only_the_tagged_memories() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    remember(&client, "alpha note about deployments", "work").await;
    remember(&client, "beta note about the kitchen", "home").await;
    remember(&client, "gamma note about deployments", "work").await;

    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_forget")
                .with_arguments(args(json!({ "tag": "work" }))),
        )
        .await
        .unwrap();
    assert_eq!(
        structured(&result)["forgotten"].as_array().unwrap().len(),
        2
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn forget_requires_exactly_one_selector() {
    // Two selectors is ambiguous and none is a request to delete everything.
    // Both must be refused rather than guessed at.
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    assert!(
        client
            .call_tool(CallToolRequestParams::new("memory_forget").with_arguments(args(json!({}))))
            .await
            .is_err()
    );
    assert!(
        client
            .call_tool(
                CallToolRequestParams::new("memory_forget")
                    .with_arguments(args(json!({ "tag": "work", "kind": "fact" })))
            )
            .await
            .is_err()
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn forgetting_an_absent_id_is_a_successful_empty_result() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_forget")
                .with_arguments(args(json!({ "ids": ["01ARZ3NDEKTSV4RRFFQ69G5FAV"] }))),
        )
        .await
        .unwrap();
    assert_eq!(
        structured(&result)["forgotten"].as_array().unwrap().len(),
        0
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn pinning_is_visible_in_a_later_review() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    let id = remember(&client, "never forget this one", "important").await;

    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_protect")
                .with_arguments(args(json!({ "id": id.clone(), "level": "pinned" }))),
        )
        .await
        .unwrap();
    assert_eq!(structured(&result)["protection"], "pinned");

    let review = client
        .call_tool(CallToolRequestParams::new("memory_review").with_arguments(args(json!({}))))
        .await
        .unwrap();
    let items = structured(&review)["items"].as_array().unwrap();
    let pinned = items
        .iter()
        .find(|i| i["id"] == json!(id))
        .expect("the item survives");
    assert_eq!(pinned["protection"], "pinned");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_protected_window_needs_its_deadline() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    let id = remember(&client, "protect me for a while", "important").await;

    assert!(
        client
            .call_tool(
                CallToolRequestParams::new("memory_protect")
                    .with_arguments(args(json!({ "id": id.clone(), "level": "protected" })))
            )
            .await
            .is_err(),
        "a protected window with no deadline never expires — that is `pinned`, and the caller \
         must say which they meant"
    );

    let until = time::OffsetDateTime::now_utc().unix_timestamp() + 86_400;
    let ok = client
        .call_tool(
            CallToolRequestParams::new("memory_protect").with_arguments(args(
                json!({ "id": id, "level": "protected", "until": until }),
            )),
        )
        .await
        .unwrap();
    assert_eq!(structured(&ok)["protection"], "protected");
    assert_eq!(structured(&ok)["protected_until"], json!(until));
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn protecting_a_memory_that_does_not_exist_is_an_error() {
    let (eng, _dir) = engine();
    let client = connect(eng).await;
    assert!(
        client
            .call_tool(
                CallToolRequestParams::new("memory_protect").with_arguments(args(
                    json!({ "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "level": "pinned" })
                ))
            )
            .await
            .is_err()
    );
    client.cancel().await.unwrap();
}
