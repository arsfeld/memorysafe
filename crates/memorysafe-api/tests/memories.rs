mod support;

use axum::http::StatusCode;
use serde_json::json;
use support::{delete, get, harness, post, send};

fn scope() -> serde_json::Value {
    json!({ "subject": "user-42", "namespace": "agent" })
}

async fn remember(h: &support::Harness, body: &str, tags: serde_json::Value) -> serde_json::Value {
    let mut payload = scope();
    payload["body"] = json!(body);
    payload["tags"] = tags;
    let reply = send(&h.app, post("/v1/memories", Some(&h.key), payload)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    reply.body
}

#[tokio::test]
async fn a_write_returns_two_hundred_with_the_decision() {
    let h = harness();
    let out = remember(&h, "the production migration runs on Sundays", json!([])).await;

    assert_eq!(out["action"]["kind"], "retain", "{out}");
    assert!(out["item_id"].is_string());
    assert!(out["audit_id"].is_string());
    assert!(!out["reasons"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_rejected_write_is_also_two_hundred() {
    // The single most important behaviour on this surface. If a redundant write
    // is a 4xx, every client library will treat governance as an outage.
    let h = harness();
    let body = "the deploy key rotates every ninety days";
    remember(&h, body, json!([])).await;
    let second = remember(&h, body, json!([])).await;

    assert!(
        second["action"]["kind"] == "reject" || second["action"]["kind"] == "merge",
        "an identical rewrite was admitted again: {second}"
    );
}

#[tokio::test]
async fn an_empty_body_is_four_hundred() {
    let h = harness();
    let mut payload = scope();
    payload["body"] = json!("   ");
    let reply = send(&h.app, post("/v1/memories", Some(&h.key), payload)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.body["error"], "validation");
}

#[tokio::test]
async fn a_retried_write_with_the_same_key_returns_the_same_outcome() {
    let h = harness();
    let mut payload = scope();
    payload["body"] = json!("written exactly once");
    payload["idempotency_key"] = json!("retry-me");

    let first = send(&h.app, post("/v1/memories", Some(&h.key), payload.clone())).await;
    let second = send(&h.app, post("/v1/memories", Some(&h.key), payload)).await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(second.status, StatusCode::OK);
    assert_eq!(first.body["item_id"], second.body["item_id"]);

    let listed = send(
        &h.app,
        get("/v1/memories?subject=user-42&namespace=agent", Some(&h.key)),
    )
    .await;
    assert_eq!(listed.body["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn recall_returns_a_working_set_with_reasons_and_an_audit_id() {
    let h = harness();
    for body in [
        "the production migration runs on Sundays",
        "the on-call rotation starts Monday morning",
    ] {
        remember(&h, body, json!([])).await;
    }

    let mut payload = scope();
    payload["query"] = json!("when does the migration run");
    payload["budget"] = json!({ "max_tokens": 500, "max_items": 1 });
    let reply = send(&h.app, post("/v1/recall", Some(&h.key), payload)).await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert!(reply.body["audit_id"].is_string(), "{}", reply.text);
    let items = reply.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "the budget was ignored: {}", reply.text);
    assert!(items[0]["item"]["body"].is_string());
    assert!(items[0]["reason"]["code"].is_string());
}

#[tokio::test]
async fn recall_in_search_mode_is_still_scoped_and_audited() {
    let h = harness();
    remember(&h, "a memory in the agent namespace", json!([])).await;

    let mut payload = scope();
    payload["mode"] = json!("search");
    payload["query"] = json!("memory");
    let reply = send(&h.app, post("/v1/recall", Some(&h.key), payload)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(
        reply.body["audit_id"].is_string(),
        "search mode skipped the audit"
    );

    let mut elsewhere = json!({ "subject": "user-42", "namespace": "other" });
    elsewhere["mode"] = json!("search");
    elsewhere["query"] = json!("memory");
    let empty = send(&h.app, post("/v1/recall", Some(&h.key), elsewhere)).await;
    assert_eq!(
        empty.body["items"].as_array().unwrap().len(),
        0,
        "search crossed a namespace"
    );
}

#[tokio::test]
async fn a_single_memory_can_be_fetched_and_a_missing_one_is_404() {
    let h = harness();
    let id = remember(&h, "fetch me by id", json!([])).await["item_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let found = send(
        &h.app,
        get(
            &format!("/v1/memories/{id}?subject=user-42&namespace=agent"),
            Some(&h.key),
        ),
    )
    .await;
    assert_eq!(found.status, StatusCode::OK);
    assert_eq!(found.body["body"], "fetch me by id");

    let missing = send(
        &h.app,
        get(
            "/v1/memories/01ARZ3NDEKTSV4RRFFQ69G5FAV?subject=user-42&namespace=agent",
            Some(&h.key),
        ),
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    let malformed = send(
        &h.app,
        get(
            "/v1/memories/not-a-ulid?subject=user-42&namespace=agent",
            Some(&h.key),
        ),
    )
    .await;
    assert_eq!(
        malformed.status,
        StatusCode::BAD_REQUEST,
        "a bad id is the caller's mistake"
    );
}

#[tokio::test]
async fn deleting_by_id_removes_exactly_that_memory() {
    let h = harness();
    let id = remember(&h, "a memory to delete", json!([])).await["item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    remember(&h, "a memory to keep around", json!([])).await;

    let reply = send(
        &h.app,
        delete(
            &format!("/v1/memories/{id}?subject=user-42&namespace=agent"),
            Some(&h.key),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["forgotten"], json!([id]));

    let left = send(
        &h.app,
        get("/v1/memories?subject=user-42&namespace=agent", Some(&h.key)),
    )
    .await;
    assert_eq!(left.body["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn forgetting_by_query_takes_exactly_one_selector() {
    let h = harness();
    remember(&h, "alpha note about deployments", json!(["work"])).await;
    remember(&h, "beta note about the kitchen", json!(["home"])).await;

    let mut payload = scope();
    payload["tag"] = json!("work");
    let ok = send(&h.app, post("/v1/forget", Some(&h.key), payload)).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.body["forgotten"].as_array().unwrap().len(), 1);

    let none = send(&h.app, post("/v1/forget", Some(&h.key), scope())).await;
    assert_eq!(
        none.status,
        StatusCode::BAD_REQUEST,
        "an empty selector is not 'delete everything'"
    );

    let mut both = scope();
    both["tag"] = json!("work");
    both["kind"] = json!("fact");
    assert_eq!(
        send(&h.app, post("/v1/forget", Some(&h.key), both))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn protecting_a_memory_pins_it_and_is_visible_in_review() {
    let h = harness();
    let id = remember(&h, "never forget this one", json!([])).await["item_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let mut payload = scope();
    payload["level"] = json!("pinned");
    let reply = send(
        &h.app,
        post(&format!("/v1/memories/{id}/protect"), Some(&h.key), payload),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.body["action"]["protection"]["kind"], "pinned");

    let listed = send(
        &h.app,
        get("/v1/memories?subject=user-42&namespace=agent", Some(&h.key)),
    )
    .await;
    let item = listed.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == json!(id))
        .expect("the item survives");
    assert_eq!(item["protection"]["kind"], "pinned");
}

#[tokio::test]
async fn the_reserved_subject_is_403_on_every_route_that_takes_a_scope() {
    let h = harness();
    let query = send(
        &h.app,
        get("/v1/memories?subject=_admin&namespace=_admin", Some(&h.key)),
    )
    .await;
    assert_eq!(query.status, StatusCode::FORBIDDEN);

    let body = send(
        &h.app,
        post(
            "/v1/memories",
            Some(&h.key),
            json!({
                "subject": "_admin", "namespace": "_admin", "body": "forged"
            }),
        ),
    )
    .await;
    assert_eq!(body.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_missing_scope_parameter_is_400_not_a_default_scope() {
    // Defaulting a namespace here would silently write into someone else's
    // budget. The caller must say where.
    let h = harness();
    let reply = send(&h.app, get("/v1/memories?subject=user-42", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.body["error"], "validation");
}

#[tokio::test]
async fn review_pages_and_reports_the_page_it_returned() {
    let h = harness();
    for i in 0..5 {
        remember(&h, &format!("distinct memory {i} on topic {i}"), json!([])).await;
    }

    let reply = send(
        &h.app,
        get(
            "/v1/memories?subject=user-42&namespace=agent&limit=2&offset=2",
            Some(&h.key),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["items"].as_array().unwrap().len(), 2);
    assert_eq!(reply.body["offset"], json!(2));
    assert_eq!(reply.body["limit"], json!(2));
}
