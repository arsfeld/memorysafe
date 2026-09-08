mod support;

use axum::http::StatusCode;
use serde_json::json;
use support::{delete, get, harness, post, send};

async fn remember(h: &support::Harness, body: &str) -> serde_json::Value {
    let reply = send(
        &h.app,
        post(
            "/v1/memories",
            Some(&h.key),
            json!({ "namespace": "agent", "body": body }),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    reply.body
}

#[tokio::test]
async fn the_audit_route_returns_decisions_and_never_bodies() {
    let h = harness();
    remember(&h, "a body that must not appear in the trail").await;

    let reply = send(&h.app, get("/v1/audit?namespace=agent", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::OK);
    let records = reply.body["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["event"], "admitted");
    assert!(records[0]["decision"].is_object());
    assert!(
        !reply.text.contains("a body that must not appear"),
        "the audit route leaked an item body"
    );
}

#[tokio::test]
async fn the_audit_route_says_when_it_truncated() {
    let h = harness();
    for i in 0..5 {
        remember(&h, &format!("distinct memory {i} about topic {i}")).await;
    }

    let capped = send(
        &h.app,
        get("/v1/audit?namespace=agent&limit=2", Some(&h.key)),
    )
    .await;
    assert_eq!(capped.body["records"].as_array().unwrap().len(), 2);
    assert_eq!(
        capped.body["truncated"],
        json!(true),
        "a capped page must admit it"
    );

    let whole = send(
        &h.app,
        get("/v1/audit?namespace=agent&limit=100", Some(&h.key)),
    )
    .await;
    assert_eq!(whole.body["truncated"], json!(false));
}

#[tokio::test]
async fn the_audit_route_filters_by_event() {
    let h = harness();
    let id = remember(&h, "a memory that will be deleted").await["item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    send(
        &h.app,
        delete(&format!("/v1/memories/{id}?namespace=agent"), Some(&h.key)),
    )
    .await;

    let reply = send(
        &h.app,
        get("/v1/audit?namespace=agent&event=forgotten", Some(&h.key)),
    )
    .await;
    let records = reply.body["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["event"], "forgotten");

    let several = send(
        &h.app,
        get(
            "/v1/audit?namespace=agent&event=admitted,forgotten",
            Some(&h.key),
        ),
    )
    .await;
    assert_eq!(
        several.body["records"].as_array().unwrap().len(),
        2,
        "a comma-separated event list must widen the filter, not narrow it to nothing"
    );

    let nonsense = send(
        &h.app,
        get("/v1/audit?namespace=agent&event=exploded", Some(&h.key)),
    )
    .await;
    assert_eq!(nonsense.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn maintenance_reports_what_it_scanned_and_can_be_resumed() {
    let h = harness();
    for i in 0..3 {
        remember(&h, &format!("healthy memory {i} about topic {i}")).await;
    }

    let reply = send(
        &h.app,
        post(
            "/v1/maintain",
            Some(&h.key),
            json!({ "namespace": "agent" }),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    // Fix round 1, Important 4: `.as_u64().is_some()` is satisfied by `0`,
    // and `reply.body.get("next_cursor").is_some()` is satisfied by `null`
    // — a handler returning a zeroed `MaintainReport` without ever calling
    // `Engine::maintain` passed both. Concrete values instead: three items
    // were remembered above, so a real scan reports `scanned: 3`, and a
    // three-item scope fits in one `MAINTAIN_BATCH` pass, so `next_cursor`
    // is genuinely `null`, not merely present.
    assert_eq!(reply.body["scanned"], json!(3), "{}", reply.text);
    assert_eq!(reply.body["forgotten"], json!(0));
    assert_eq!(
        reply.body["next_cursor"],
        serde_json::Value::Null,
        "{}",
        reply.text
    );

    // Cursor threading: nothing above ever sends a `cursor`, so
    // `MaintainBody::cursor` reaching `Engine::maintain` was itself
    // unexercised — a handler that always passed `None` regardless of the
    // request body would have passed every assertion above too. An offset
    // past every item this scope holds must scan nothing; rescanning from
    // the start would instead report `scanned: 3` a second time.
    let resumed = send(
        &h.app,
        post(
            "/v1/maintain",
            Some(&h.key),
            json!({ "namespace": "agent", "cursor": 3 }),
        ),
    )
    .await;
    assert_eq!(resumed.status, StatusCode::OK, "{}", resumed.text);
    assert_eq!(
        resumed.body["scanned"],
        json!(0),
        "an explicit cursor past every item must skip them, not rescan from the start: {}",
        resumed.text
    );
}

#[tokio::test]
async fn export_returns_ndjson_and_records_who_asked() {
    let h = harness();
    remember(&h, "the thing to export").await;

    let reply = send(&h.app, get("/v1/export?namespace=agent", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.text.contains("the thing to export"));
    assert!(
        reply.text.lines().count() >= 2,
        "a header line and an item line"
    );

    let admin = send(&h.app, get("/v1/audit?namespace=_admin", Some(&h.key))).await;
    assert_eq!(
        admin.status,
        StatusCode::FORBIDDEN,
        "the admin trail is not readable through a caller-supplied scope"
    );

    // It is readable through the engine, which is how the CLI shows it.
    let rows = h
        .engine
        .audit(
            &memorysafe_core::Scope::admin(&memorysafe_core::TenantId::new("acme").unwrap()),
            &memorysafe_core::AuditFilter {
                events: vec![memorysafe_core::AuditEvent::Exported],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].actor.kind, memorysafe_core::ActorKind::ApiKey);
}

#[tokio::test]
async fn export_can_render_markdown_for_a_person_to_read() {
    let h = harness();
    remember(&h, "the on-call rotation starts Monday").await;

    let reply = send(
        &h.app,
        get("/v1/export?namespace=agent&format=markdown", Some(&h.key)),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.text.contains("# MemorySafe export"));
    assert!(reply.text.contains("the on-call rotation starts Monday"));

    // C2 (final review): `format=markdown` used to call `Engine::export_markdown`
    // directly, which writes no audit row — a key holder could read the
    // tenant's entire corpus, body and all, with no trace of having done so.
    // `docs/known-gaps.md` accepted deferring `Engine::export`'s own missing
    // audit record only on the explicit condition that Plan 3 expose neither
    // export surface without one; this asserts the condition actually holds
    // for markdown, the same way `export_returns_ndjson_and_records_who_asked`
    // (above) asserts it for ndjson.
    let rows = h
        .engine
        .audit(
            &memorysafe_core::Scope::admin(&memorysafe_core::TenantId::new("acme").unwrap()),
            &memorysafe_core::AuditFilter {
                events: vec![memorysafe_core::AuditEvent::Exported],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "a markdown export must write exactly one Exported audit row"
    );
    assert_eq!(rows[0].actor.kind, memorysafe_core::ActorKind::ApiKey);
}

/// Export is scoped to the credential's subject.
#[tokio::test]
async fn export_takes_its_subject_from_the_credential() {
    let h = harness();
    remember(&h, "the credential subject owns this export").await;

    let reply = send(&h.app, get("/v1/export?namespace=agent", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert!(
        reply
            .text
            .contains("the credential subject owns this export")
    );
}

/// Fix round 1, Important 5: `ExportQuery::include_audit` was threaded into
/// `ScopeSelector` with no test ever setting it — a handler that silently
/// hardcoded `include_audit: true` would have passed every other test in
/// this file while handing every caller the tenant's entire audit trail,
/// `_admin` rows included, on every export.
#[tokio::test]
async fn export_omits_audit_records_unless_include_audit_is_requested() {
    let h = harness();
    remember(
        &h,
        "a memory whose own audit trail must not leak by default",
    )
    .await;

    let default_export = send(&h.app, get("/v1/export?namespace=agent", Some(&h.key))).await;
    assert_eq!(
        default_export.status,
        StatusCode::OK,
        "{}",
        default_export.text
    );
    assert!(
        !default_export.text.contains("\"record\":\"audit\""),
        "a default export (include_audit unset) carried audit records: {}",
        default_export.text
    );

    let with_audit = send(
        &h.app,
        get(
            "/v1/export?namespace=agent&include_audit=true",
            Some(&h.key),
        ),
    )
    .await;
    assert_eq!(with_audit.status, StatusCode::OK, "{}", with_audit.text);
    assert!(
        with_audit.text.contains("\"record\":\"audit\""),
        "include_audit=true must actually include audit records, or the two exports \
         are indistinguishable: {}",
        with_audit.text
    );
}

#[tokio::test]
async fn an_export_round_trips_through_import() {
    let source = harness();
    for body in ["first exported memory", "second exported memory"] {
        remember(&source, body).await;
    }
    let ndjson = send(
        &source.app,
        get("/v1/export?namespace=agent", Some(&source.key)),
    )
    .await
    .text;

    let target = harness();
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/import")
        .header("authorization", format!("Bearer {}", target.key))
        .header("content-type", "application/x-ndjson")
        .body(axum::body::Body::from(ndjson))
        .unwrap();
    let reply = send(&target.app, request).await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.body["items_imported"], json!(2));

    let listed = send(
        &target.app,
        get("/v1/memories?namespace=agent", Some(&target.key)),
    )
    .await;
    assert_eq!(listed.body["items"].as_array().unwrap().len(), 2);
}

/// Fix round 1, Important 1: `import`'s bare `String` extractor (before this
/// fix) rendered an over-the-limit body as axum's own plain-text 413
/// (`StringRejection::FailedToBufferBody(FailedToBufferBody::LengthLimitError)`)
/// rather than the `Problem` JSON envelope every other failure on this API
/// returns — and this is not a theoretical edge case: `import`'s own doc
/// deliberately leaves the 2 MB default body limit undisturbed ("An archive
/// larger than that is a CLI job, not an HTTP request"), so this is the one
/// failure mode the route is explicitly designed to produce. Through the
/// real router this time, not just `ValidatedText`'s own unit test, so this
/// also proves the route is actually wired to the wrapper.
#[tokio::test]
async fn an_oversized_import_is_a_json_problem_not_a_plain_text_413() {
    let h = harness();
    // One byte over axum's hardcoded 2 MB default extractor limit.
    let oversized = vec![b'x'; 2_097_152 + 1];
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/import")
        .header("authorization", format!("Bearer {}", h.key))
        .header("content-type", "application/x-ndjson")
        .body(axum::body::Body::from(oversized))
        .unwrap();
    let reply = send(&h.app, request).await;

    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.text);
    assert_eq!(reply.body["error"], "validation", "{}", reply.text);
    assert!(reply.body["message"].is_string());
}

#[tokio::test]
async fn purging_the_credential_subject_removes_its_memories() {
    let h = harness();
    remember(&h, "a memory belonging to the credential subject").await;
    remember(&h, "another memory belonging to the credential subject").await;

    let reply = send(&h.app, delete("/v1/subjects/user-42", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.body["items_removed"], json!(2));

    let gone = send(&h.app, get("/v1/memories?namespace=agent", Some(&h.key))).await;
    assert_eq!(gone.body["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn the_reserved_subject_cannot_be_purged() {
    let h = harness();
    let reply = send(&h.app, delete("/v1/subjects/_admin", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
}

/// Fix round 1, Important 2: the brief's own hand-rolled check compared only
/// against `ADMIN_COMPONENT`, missing `PURGED_COMPONENT` (`_purged`) —
/// `memorysafe_auth::check_reserved` covers both, and its own doc says it
/// exists as a free function precisely for adapter paths with no
/// `Authenticated::scope` to route through, which `export` and
/// `purge_subject` both are.
#[tokio::test]
async fn the_purged_reserved_word_cannot_be_named_as_an_export_namespace() {
    let h = harness();
    let by_namespace = send(&h.app, get("/v1/export?namespace=_purged", Some(&h.key))).await;
    assert_eq!(
        by_namespace.status,
        StatusCode::FORBIDDEN,
        "{}",
        by_namespace.text
    );
}

/// Fix round 1, Important 2: the `_purged` half of the same gap, on the
/// purge route itself.
#[tokio::test]
async fn the_purged_reserved_word_cannot_be_purged_either() {
    let h = harness();
    let reply = send(&h.app, delete("/v1/subjects/_purged", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text);
}

/// The `SubjectPurged` row must name the caller who ordered the erasure, not
/// an anonymous system actor — `Engine::purge_subject` takes an `Actor` for
/// exactly this reason (Plan 3's per-tenant-governance amendment). Read back
/// through the engine directly, at the purged subject's own former scope
/// (`Engine::purge_subject` files `SubjectPurged` under the subject's
/// lexicographically first namespace, not the tenant's admin scope — see
/// `mutate.rs`'s `purge_scope`), since the caller-facing `GET /v1/audit`
/// route can only ever query a scope the caller may still name, and a purged
/// subject's own scope is exactly nameable (unlike `_admin`).
#[tokio::test]
async fn purge_is_audited_with_the_actor_who_asked_for_it() {
    let h = harness();
    remember(&h, "a memory belonging to the credential subject").await;

    let reply = send(&h.app, delete("/v1/subjects/user-42", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);

    let rows = h
        .engine
        .audit(
            &memorysafe_core::Scope::new("acme", "user-42", "agent").unwrap(),
            &memorysafe_core::AuditFilter {
                events: vec![memorysafe_core::AuditEvent::SubjectPurged],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].actor.kind,
        memorysafe_core::ActorKind::ApiKey,
        "purge_subject must be attributed to the caller, not Actor::system()"
    );
}

/// Export and purge are the highest-consequence operations this route file
/// adds: a caller must never reach outside their own tenant through either
/// one, no matter what subject or namespace name it shares with another
/// tenant. Both keys are registered against one shared engine so the two
/// tenants' data genuinely coexist in the same backend — the ordinary
/// `harness()` helper always mints an "acme" key against its own fresh
/// engine, which would prove nothing about cross-tenant isolation since
/// there is only ever one tenant present to begin with.
#[tokio::test]
async fn export_and_purge_never_cross_a_tenant_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let engine = std::sync::Arc::new(memorysafe_engine::Engine::new(
        memorysafe_engine::EngineConfig::new(
            std::sync::Arc::new(memorysafe_backend_sqlite::SqliteBackend::open(dir.keep())),
            std::sync::Arc::new(memorysafe_embed::DeterministicEmbedder::new(256)),
            std::sync::Arc::new(memorysafe_policy::BaselinePolicy::default()),
        ),
    ));
    let acme = memorysafe_auth::generate(
        memorysafe_core::TenantId::new("acme").unwrap(),
        memorysafe_core::SubjectId::new("shared-subject-name").unwrap(),
        "acme-key",
    )
    .unwrap();
    let globex = memorysafe_auth::generate(
        memorysafe_core::TenantId::new("globex").unwrap(),
        memorysafe_core::SubjectId::new("shared-subject-name").unwrap(),
        "globex-key",
    )
    .unwrap();
    let keys = std::sync::Arc::new(memorysafe_auth::ApiKeyStore::new(vec![
        acme.record,
        globex.record,
    ]));
    let app = memorysafe_api::router(memorysafe_api::AppState {
        engine: engine.clone(),
        resolver: std::sync::Arc::new(memorysafe_auth::ApiKeyScope::new(keys)),
    });

    let write = send(
        &app,
        post(
            "/v1/memories",
            Some(&acme.secret),
            json!({
                "namespace": "agent",
                "body": "acme's private memory, not globex's to read or erase"
            }),
        ),
    )
    .await;
    assert_eq!(write.status, StatusCode::OK, "{}", write.text);

    // globex's export must never include acme's item, even though nothing
    // in the request names a tenant at all — the tenant comes from the
    // credential alone.
    let exported = send(&app, get("/v1/export", Some(&globex.secret))).await;
    assert_eq!(exported.status, StatusCode::OK);
    assert!(
        !exported.text.contains("acme's private memory"),
        "globex's export leaked acme's item: {}",
        exported.text
    );

    // globex purging the identically-named subject must not touch acme's
    // items — the subject name collision must not become a tenant collision.
    let purge = send(
        &app,
        delete("/v1/subjects/shared-subject-name", Some(&globex.secret)),
    )
    .await;
    assert_eq!(purge.status, StatusCode::OK, "{}", purge.text);
    assert_eq!(
        purge.body["items_removed"],
        json!(0),
        "globex's purge removed items belonging to acme"
    );

    let still_there = send(
        &app,
        get("/v1/memories?namespace=agent", Some(&acme.secret)),
    )
    .await;
    assert_eq!(
        still_there.body["items"].as_array().unwrap().len(),
        1,
        "acme's memory was purged by globex's request"
    );
}
