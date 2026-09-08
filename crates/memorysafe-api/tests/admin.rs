mod support;

use axum::http::StatusCode;
use serde_json::json;
use support::{get, harness, post, put, send};

#[tokio::test]
async fn a_tenant_can_read_and_set_a_namespace_budget() {
    let h = harness();
    let before = send(
        &h.app,
        get(
            "/v1/admin/tenants/acme/budgets?namespace=agent",
            Some(&h.key),
        ),
    )
    .await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.text);
    assert_eq!(before.body["budget"]["max_items"], json!(null));

    let set = send(
        &h.app,
        put(
            "/v1/admin/tenants/acme/budgets",
            Some(&h.key),
            json!({ "namespace": "agent", "max_items": 3 }),
        ),
    )
    .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.text);
    assert_eq!(set.body["budget"]["max_items"], json!(3));

    let after = send(
        &h.app,
        get(
            "/v1/admin/tenants/acme/budgets?namespace=agent",
            Some(&h.key),
        ),
    )
    .await;
    assert_eq!(after.body["budget"]["max_items"], json!(3));
}

#[tokio::test]
async fn a_budget_actually_bounds_the_namespace() {
    // A route that stores a number nobody reads is worse than no route.
    let h = harness();
    send(
        &h.app,
        put(
            "/v1/admin/tenants/acme/budgets",
            Some(&h.key),
            json!({ "namespace": "agent", "max_items": 2 }),
        ),
    )
    .await;

    for i in 0..4 {
        send(
            &h.app,
            post(
                "/v1/memories",
                Some(&h.key),
                json!({
                    "namespace": "agent", "body": format!("distinct memory {i} on topic {i}")
                }),
            ),
        )
        .await;
    }

    let listed = send(&h.app, get("/v1/memories?namespace=agent", Some(&h.key))).await;
    assert!(
        listed.body["items"].as_array().unwrap().len() <= 2,
        "the budget was exceeded: {}",
        listed.text
    );
}

#[tokio::test]
async fn the_policy_config_round_trips_and_the_change_is_audited() {
    let h = harness();
    let before = send(&h.app, get("/v1/admin/tenants/acme/policy", Some(&h.key))).await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.text);
    assert_eq!(before.body["merge_threshold"], json!(0.93));

    let mut tuned: serde_json::Value = before.body.clone();
    tuned["merge_threshold"] = json!(0.8);
    let set = send(
        &h.app,
        put("/v1/admin/tenants/acme/policy", Some(&h.key), tuned),
    )
    .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.text);
    assert!(
        set.body["audit_id"].is_string(),
        "a policy change is a governance event"
    );

    let after = send(&h.app, get("/v1/admin/tenants/acme/policy", Some(&h.key))).await;
    assert_eq!(after.body["merge_threshold"], json!(0.8));
}

#[tokio::test]
async fn an_incoherent_policy_config_is_four_hundred() {
    let h = harness();
    let before = send(&h.app, get("/v1/admin/tenants/acme/policy", Some(&h.key))).await;
    let mut broken: serde_json::Value = before.body.clone();
    broken["merge_threshold"] = json!(0.99);
    broken["duplicate_threshold"] = json!(0.90);

    let reply = send(
        &h.app,
        put("/v1/admin/tenants/acme/policy", Some(&h.key), broken),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.text);
    assert_eq!(reply.body["error"], "validation");

    let unchanged = send(&h.app, get("/v1/admin/tenants/acme/policy", Some(&h.key))).await;
    assert_eq!(unchanged.body["merge_threshold"], json!(0.93));
}

/// Pins the overflow bound added to `memorysafe-engine`'s `settings::validate`
/// (`crates/memorysafe-engine/src/settings.rs`) at the HTTP seam: a
/// `protection_window_days` large enough to risk walking `admit::decide`'s
/// `ctx.now + Duration::days(..)` past `OffsetDateTime`'s representable range
/// must be refused here too, not merely at the unit level, since this is the
/// route a real admin PUT reaches.
#[tokio::test]
async fn a_protection_window_that_risks_a_date_overflow_is_four_hundred() {
    let h = harness();
    let before = send(&h.app, get("/v1/admin/tenants/acme/policy", Some(&h.key))).await;
    let mut broken: serde_json::Value = before.body.clone();
    broken["protection_window_days"] = json!(10_000_000);

    let reply = send(
        &h.app,
        put("/v1/admin/tenants/acme/policy", Some(&h.key), broken),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.text);
    assert_eq!(reply.body["error"], "validation");
    assert!(
        reply.text.contains("protection_window_days"),
        "the failure should name the offending field: {}",
        reply.text
    );
}

#[tokio::test]
async fn the_retention_profile_round_trips_by_name() {
    let h = harness();
    let before = send(
        &h.app,
        get("/v1/admin/tenants/acme/retention", Some(&h.key)),
    )
    .await;
    assert_eq!(before.body["profile"], json!("balanced"));

    for profile in ["gdpr_strict", "hipaa_retain", "forensic", "balanced"] {
        let set = send(
            &h.app,
            put(
                "/v1/admin/tenants/acme/retention",
                Some(&h.key),
                json!({ "profile": profile }),
            ),
        )
        .await;
        assert_eq!(set.status, StatusCode::OK, "{profile}: {}", set.text);
        assert_eq!(set.body["profile"], json!(profile));
        assert!(set.body["audit_id"].is_string());

        let after = send(
            &h.app,
            get("/v1/admin/tenants/acme/retention", Some(&h.key)),
        )
        .await;
        assert_eq!(after.body["profile"], json!(profile));
    }

    let nonsense = send(
        &h.app,
        put(
            "/v1/admin/tenants/acme/retention",
            Some(&h.key),
            json!({ "profile": "whatever" }),
        ),
    )
    .await;
    assert_eq!(nonsense.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_key_cannot_administer_another_tenant() {
    let h = harness();
    for uri in [
        "/v1/admin/tenants/globex/policy",
        "/v1/admin/tenants/globex/retention",
        "/v1/admin/tenants/globex/budgets?namespace=agent",
    ] {
        let reply = send(&h.app, get(uri, Some(&h.key))).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{uri} was readable");
    }

    let write = send(
        &h.app,
        put(
            "/v1/admin/tenants/globex/retention",
            Some(&h.key),
            json!({ "profile": "forensic" }),
        ),
    )
    .await;
    assert_eq!(write.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_routes_need_a_credential_like_everything_else() {
    let h = harness();
    assert_eq!(
        send(&h.app, get("/v1/admin/tenants/acme/policy", None))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}
