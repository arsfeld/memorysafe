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

#[tokio::test]
async fn review_echoes_the_clamped_limit_not_the_requested_one() {
    // The backend clamps `Page::limit` to `MAX_PAGE_LIMIT` before running the
    // query (`Page::effective_limit`), and this workspace's own paging
    // convention (`AuditFilter::limit`'s doc) makes `returned.len() < limit`
    // the SOLE exhaustion signal — there is deliberately no `truncated`
    // flag. If the echoed `limit` were the raw, unclamped request, a client
    // paging with a large limit would see fewer items than that limit and
    // wrongly conclude the scope was exhausted, silently hiding the rest of
    // what is stored — precisely what `GET /v1/memories` exists to make
    // visible.
    let h = harness();
    remember(&h, "one memory in a very large requested page", json!([])).await;

    let reply = send(
        &h.app,
        get(
            "/v1/memories?subject=user-42&namespace=agent&limit=5000",
            Some(&h.key),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.body["limit"],
        json!(memorysafe_backend::MAX_PAGE_LIMIT),
        "the echoed limit must be the backend's clamped ceiling, not the raw request: {}",
        reply.text
    );
}

#[tokio::test]
async fn deleting_or_forgetting_an_absent_id_is_a_successful_empty_result_not_a_404() {
    // The central constraint on this whole surface: a governance no-op is
    // 200 with nothing forgotten, never a 404. `DELETE /v1/memories/{id}`
    // and `POST /v1/forget` share the same `Engine::forget` call underneath
    // and must agree.
    let h = harness();
    let absent = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    let deleted = send(
        &h.app,
        delete(
            &format!("/v1/memories/{absent}?subject=user-42&namespace=agent"),
            Some(&h.key),
        ),
    )
    .await;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.text);
    assert_eq!(deleted.body["forgotten"], json!([]));

    let mut payload = scope();
    payload["ids"] = json!([absent]);
    let forgotten = send(&h.app, post("/v1/forget", Some(&h.key), payload)).await;
    assert_eq!(forgotten.status, StatusCode::OK, "{}", forgotten.text);
    assert_eq!(forgotten.body["forgotten"], json!([]));
}

#[tokio::test]
async fn reusing_an_idempotency_key_with_a_different_payload_is_a_conflict() {
    // The engine seam and the §9 status mapping are both pinned elsewhere
    // (`memorysafe-engine`'s `tests/write.rs`, `memorysafe-api`'s
    // `error.rs`); this is the composition of the two over this route —
    // the first HTTP-level 409 on this API.
    let h = harness();

    let mut first = scope();
    first["body"] = json!("the first payload under this idempotency key");
    first["idempotency_key"] = json!("shared-key");
    let ok = send(&h.app, post("/v1/memories", Some(&h.key), first)).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);

    let mut second = scope();
    second["body"] = json!("a completely different payload under the same key");
    second["idempotency_key"] = json!("shared-key");
    let conflict = send(&h.app, post("/v1/memories", Some(&h.key), second)).await;
    assert_eq!(conflict.status, StatusCode::CONFLICT, "{}", conflict.text);
    assert_eq!(conflict.body["error"], "conflict");
}

#[tokio::test]
async fn recall_hard_filters_are_threaded_into_the_backend_query() {
    // `tags_any`, `kinds`, `occurred_after` and `occurred_before` must reach
    // `RecallRequest`, not be dropped on the way — deleting any one of them
    // from the handler leaves both items admitted, since a two-item scope is
    // small enough that both survive relevance ranking on their own.
    let h = harness();

    let mut a = scope();
    a["body"] = json!("the quarterly infrastructure review happens every March");
    a["tags"] = json!(["alpha"]);
    a["kind"] = json!("note");
    a["occurred_at"] = json!(1_000_000);
    assert_eq!(
        send(&h.app, post("/v1/memories", Some(&h.key), a))
            .await
            .status,
        StatusCode::OK
    );

    let mut b = scope();
    b["body"] = json!("grocery list for the weekend farmers market");
    b["tags"] = json!(["beta"]);
    b["kind"] = json!("fact");
    b["occurred_at"] = json!(2_000_000);
    assert_eq!(
        send(&h.app, post("/v1/memories", Some(&h.key), b))
            .await
            .status,
        StatusCode::OK
    );

    async fn search_with(
        h: &support::Harness,
        field: &str,
        value: serde_json::Value,
    ) -> serde_json::Value {
        let mut payload = scope();
        payload["mode"] = json!("search");
        payload["query"] = json!("memory");
        payload[field] = value;
        send(&h.app, post("/v1/recall", Some(&h.key), payload))
            .await
            .body
    }

    let by_tag = search_with(&h, "tags_any", json!(["alpha"])).await;
    assert_eq!(
        by_tag["items"].as_array().unwrap().len(),
        1,
        "tags_any was not threaded into the query: {by_tag}"
    );
    assert_eq!(by_tag["items"][0]["item"]["tags"], json!(["alpha"]));

    let by_kind = search_with(&h, "kinds", json!(["fact"])).await;
    assert_eq!(
        by_kind["items"].as_array().unwrap().len(),
        1,
        "kinds was not threaded into the query: {by_kind}"
    );
    assert_eq!(by_kind["items"][0]["item"]["kind"], json!("fact"));

    let by_after = search_with(&h, "occurred_after", json!(1_500_000)).await;
    assert_eq!(
        by_after["items"].as_array().unwrap().len(),
        1,
        "occurred_after was not threaded into the query: {by_after}"
    );
    assert_eq!(by_after["items"][0]["item"]["kind"], json!("fact"));

    let by_before = search_with(&h, "occurred_before", json!(1_500_000)).await;
    assert_eq!(
        by_before["items"].as_array().unwrap().len(),
        1,
        "occurred_before was not threaded into the query: {by_before}"
    );
    assert_eq!(by_before["items"][0]["item"]["kind"], json!("note"));
}

#[tokio::test]
async fn recall_sensitivity_ceiling_defaults_closed_and_widens_when_named() {
    // Pins Important 3's fix directly: an unstated ceiling must fail closed
    // to `Internal`, and naming `restricted` explicitly must widen recall to
    // include a restricted item. Deleting `sensitivity_ceiling` from the
    // `RecallRequest` construction (defaulting to the engine's own
    // permit-everything behaviour) or reverting the default back to
    // `Restricted` both make the first half of this test fail.
    let h = harness();
    remember(
        &h,
        "a routine note about a scheduled backup job",
        json!(["ceiling-test"]),
    )
    .await;
    remember(
        &h,
        "the api key for staging is stored in vault",
        json!(["ceiling-test"]),
    )
    .await;

    let mut payload = scope();
    payload["mode"] = json!("search");
    payload["query"] = json!("memory");
    payload["tags_any"] = json!(["ceiling-test"]);

    let default_ceiling = send(&h.app, post("/v1/recall", Some(&h.key), payload.clone())).await;
    assert_eq!(
        default_ceiling.status,
        StatusCode::OK,
        "{}",
        default_ceiling.text
    );
    let items = default_ceiling.body["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        1,
        "the unstated ceiling must fail closed to Internal, excluding the \
         restricted item: {}",
        default_ceiling.text
    );
    assert!(
        items[0]["item"]["body"]
            .as_str()
            .unwrap()
            .contains("routine"),
        "the item that survived the default ceiling must be the non-restricted one: {}",
        default_ceiling.text
    );

    payload["sensitivity_ceiling"] = json!("restricted");
    let widened = send(&h.app, post("/v1/recall", Some(&h.key), payload)).await;
    assert_eq!(widened.status, StatusCode::OK, "{}", widened.text);
    assert_eq!(
        widened.body["items"].as_array().unwrap().len(),
        2,
        "naming the ceiling explicitly must widen recall to include the \
         restricted item: {}",
        widened.text
    );
}
