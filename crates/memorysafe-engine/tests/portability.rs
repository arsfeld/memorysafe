use memorysafe_backend::ScopeSelector;
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Protection, Scope, SensitivityLevel, TenantId};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::{BaselineConfig, BaselinePolicy};
use std::sync::Arc;
use time::{Duration, OffsetDateTime};

fn engine() -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ))
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

fn tenant() -> TenantId {
    TenantId::new("acme").unwrap()
}

fn selector(include_audit: bool) -> ScopeSelector {
    ScopeSelector {
        tenant: tenant(),
        subject: None,
        namespace: None,
        include_audit,
    }
}

async fn seed(e: &Engine) {
    for body in [
        "the production migration runs on Sundays",
        "the deploy key rotates every ninety days",
        "the on-call rotation starts Monday morning",
    ] {
        e.remember(RememberRequest::new(scope(), body))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn ndjson_round_trips_through_a_fresh_engine() {
    let source = engine();
    seed(&source).await;

    let ndjson = source.export_ndjson(&selector(true)).await.unwrap();
    assert!(ndjson.lines().count() >= 4, "header plus three items");

    let target = engine();
    let report = target.import_ndjson(&ndjson, &tenant()).await.unwrap();
    assert_eq!(report.items_imported, 3);

    let mut before = source.review(&scope(), &Default::default()).await.unwrap();
    let mut after = target.review(&scope(), &Default::default()).await.unwrap();
    before.sort_by(|a, b| a.id.cmp(&b.id));
    after.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(before, after, "the round trip did not reproduce the corpus");
}

#[tokio::test]
async fn every_ndjson_line_is_a_standalone_json_object() {
    let e = engine();
    seed(&e).await;
    let ndjson = e.export_ndjson(&selector(false)).await.unwrap();
    for line in ndjson.lines() {
        let value: serde_json::Value = serde_json::from_str(line).expect("each line parses");
        assert!(
            value.get("record").is_some(),
            "each line names its record type"
        );
    }
}

#[tokio::test]
async fn the_markdown_view_is_readable_and_contains_the_bodies() {
    let e = engine();
    seed(&e).await;
    let md = e.export_markdown(&selector(false)).await.unwrap();

    assert!(md.contains("# MemorySafe export"));
    assert!(md.contains("the production migration runs on Sundays"));
    assert!(
        md.contains("acme / user-42 / agent"),
        "scope must be identifiable"
    );
}

#[tokio::test]
async fn importing_the_same_stream_twice_changes_nothing_the_second_time() {
    let source = engine();
    seed(&source).await;
    let ndjson = source.export_ndjson(&selector(false)).await.unwrap();

    let target = engine();
    target.import_ndjson(&ndjson, &tenant()).await.unwrap();
    let second = target.import_ndjson(&ndjson, &tenant()).await.unwrap();

    assert_eq!(second.items_imported, 0);
    assert_eq!(second.items_skipped_existing, 3);
    assert_eq!(
        target
            .review(&scope(), &Default::default())
            .await
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn an_import_cannot_downgrade_sensitivity_or_forge_a_pin() {
    // An import stream is caller-supplied JSON and MemoryItem's fields are
    // public, so it can assert anything. Neither claim is believed.
    let e = engine();
    let ndjson = format!(
        "{}\n{}\n",
        r#"{"record":"header","format_version":1,"exported_at":0}"#,
        r#"{"record":"item","item":{"id":"01ARZ3NDEKTSV4RRFFQ69G5FAV",
           "scope":{"tenant":"acme","subject":"user-42","namespace":"agent"},
           "body":"the deploy api key is sk-abc123def456ghi789jkl012",
           "kind":"fact","source":{"kind":"agent","id":null},"occurred_at":null,
           "created_at":0,"tags":[],"attrs":{},"sensitivity":"public","ttl":null,
           "protection":{"kind":"pinned"},"pending_embedding":false}}"#
            .replace('\n', "")
            .replace("           ", "")
    );

    e.import_ndjson(&ndjson, &tenant()).await.unwrap();
    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].sensitivity,
        SensitivityLevel::Restricted,
        "import downgraded a credential to Public"
    );
    assert_eq!(
        stored[0].protection,
        Protection::Normal,
        "import forged an unevictable pin"
    );
}

#[tokio::test]
async fn a_forged_far_future_protection_window_is_clamped_to_the_configured_bound() {
    // `an_import_cannot_downgrade_sensitivity_or_forge_a_pin` only checks the
    // `Pinned` VARIANT. Fix-round review found that a crafted
    // `"protection":{"kind":"protected","until":<huge>}` sails through
    // untouched, and is indistinguishable from a forged pin to every
    // consumer in the codebase: `Protection::is_evictable` returns false
    // until that instant, `gather` never offers it for eviction,
    // `validate::decision` refuses to evict it, and it permanently consumes
    // the namespace's budget. `Engine::import` clamps `until` to
    // `now + protection_window_days` (30 days by default) — the same bound
    // a genuine admission decision is held to.
    let e = engine();
    let far_future: i64 = 253_402_300_799; // 9999-12-31T23:59:59Z
    let ndjson = format!(
        "{}\n{}\n",
        r#"{"record":"header","format_version":1,"exported_at":0}"#,
        serde_json::json!({
            "record": "item",
            "item": {
                "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "scope": {"tenant": "acme", "subject": "user-42", "namespace": "agent"},
                "body": "an ordinary memory",
                "kind": "fact",
                "source": {"kind": "agent", "id": serde_json::Value::Null},
                "occurred_at": serde_json::Value::Null,
                "created_at": 0,
                "tags": [],
                "attrs": {},
                "sensitivity": "internal",
                "ttl": serde_json::Value::Null,
                "protection": {"kind": "protected", "until": far_future},
                "pending_embedding": false
            }
        })
    );

    e.import_ndjson(&ndjson, &tenant()).await.unwrap();
    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    match stored[0].protection {
        Protection::Protected { until } => {
            let lower_bound = OffsetDateTime::now_utc() + Duration::days(29);
            let upper_bound = OffsetDateTime::now_utc() + Duration::days(31);
            assert!(
                until > lower_bound && until < upper_bound,
                "a forged far-future protection window was not clamped to the \
                 configured bound: {until}"
            );
        }
        other => {
            panic!("a claimed Protected window must stay Protected (just clamped), got {other:?}")
        }
    }
}

#[tokio::test]
async fn import_uses_the_baseline_floor_not_a_deployments_tightened_config() {
    // `Engine::import`'s own doc names this trade-off explicitly: sensitivity
    // detection on import always runs against `BaselineConfig::default()`,
    // never this engine's own configured policy, because building a real
    // `AssessContext` per imported item would cost a `Backend::neighbours`
    // and `Backend::scope_stats` round trip apiece. This characterizes the
    // consequence: a deployment that tightened detection (here, a much
    // shorter `credential_token_min_len`) gets the looser DEFAULT
    // classification on the one path that takes untrusted input, not its
    // own configured floor. This test pins current, documented behaviour —
    // if it starts failing, `Engine::import` began consulting the configured
    // policy, and this test (and the doc it mirrors) should be updated to
    // say so, not silenced.
    let dir = tempfile::tempdir().expect("tempdir");
    let tightened = BaselineConfig {
        credential_token_min_len: 6,
        ..BaselineConfig::default()
    };
    let target = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::new(tightened)),
    ));

    // 10 alnum chars, mixed case and digits: `looks_like_secret_token` fires
    // at this engine's configured floor (min_len 6) but not at the crate
    // default (min_len 20).
    let token = "aB3xY9zK1m";
    let ndjson = format!(
        "{}\n{}\n",
        r#"{"record":"header","format_version":1,"exported_at":0}"#,
        serde_json::json!({
            "record": "item",
            "item": {
                "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "scope": {"tenant": "acme", "subject": "user-42", "namespace": "agent"},
                "body": format!("the value is {token} today"),
                "kind": "fact",
                "source": {"kind": "agent", "id": serde_json::Value::Null},
                "occurred_at": serde_json::Value::Null,
                "created_at": 0,
                "tags": [],
                "attrs": {},
                "sensitivity": "public",
                "ttl": serde_json::Value::Null,
                "protection": {"kind": "normal"},
                "pending_embedding": false
            }
        })
    );

    target.import_ndjson(&ndjson, &tenant()).await.unwrap();
    let stored = target.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].sensitivity,
        SensitivityLevel::Internal,
        "documented gap changed: import is now consulting the deployment's \
         configured policy rather than the baseline default"
    );
}

#[tokio::test]
async fn a_body_cannot_forge_a_scope_header_in_the_markdown_export() {
    // A body containing a newline followed by "## acme / victim / ns" would
    // otherwise render as a genuine-looking scope section for a tenant this
    // item was never in, misrepresenting provenance in the one artifact
    // whose whole purpose is legibility. Bodies (and kind, and tags) are
    // attacker-controlled on the import path this same task opens.
    let e = engine();
    let forged = "innocuous line\n## acme / victim / ns\n\nforged content";
    let ndjson = format!(
        "{}\n{}\n",
        r#"{"record":"header","format_version":1,"exported_at":0}"#,
        serde_json::json!({
            "record": "item",
            "item": {
                "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "scope": {"tenant": "acme", "subject": "user-42", "namespace": "agent"},
                "body": forged,
                "kind": "fact",
                "source": {"kind": "agent", "id": serde_json::Value::Null},
                "occurred_at": serde_json::Value::Null,
                "created_at": 0,
                "tags": [],
                "attrs": {},
                "sensitivity": "internal",
                "ttl": serde_json::Value::Null,
                "protection": {"kind": "normal"},
                "pending_embedding": false
            }
        })
    );

    e.import_ndjson(&ndjson, &tenant()).await.unwrap();
    let md = e.export_markdown(&selector(false)).await.unwrap();
    assert!(
        !md.contains("\n## acme / victim / ns"),
        "an item body forged a scope header: {md}"
    );
    // Neutralised, not merely absent: a mutant that strips the leading `#`
    // characters entirely (instead of escaping them) also defeats the
    // heading and would pass the assertion above, but it silently destroys
    // the body's actual content instead of preserving it. The escaped form
    // must still contain the literal `#` characters the body had — just
    // with a backslash defusing their structural meaning.
    assert!(
        md.contains("\\## acme / victim / ns"),
        "the '#' characters must be escaped, not silently dropped: {md}"
    );
}

#[tokio::test]
async fn an_import_cannot_use_fresh_detection_to_downgrade_an_already_high_classification() {
    // `an_import_cannot_downgrade_sensitivity_or_forge_a_pin` only exercises
    // the direction where fresh detection is HIGHER than the payload's claim
    // (a credential body claiming "public"). The engine's own doc for
    // `Engine::import` promises the opposite direction too: it "keeps
    // whichever level is higher", i.e. a payload that already carries a
    // higher, legitimately-earned classification must not be pulled DOWN to
    // whatever an ordinary-looking body detects as on its own. This item
    // earns `Restricted` purely from an explicit `sensitivity_hint` at write
    // time — its body carries no marker `assess` would ever flag — so a
    // re-import that trusted only fresh detection (dropping the payload's own
    // `item.sensitivity`) would silently downgrade it to `Internal`.
    let source = engine();
    let mut req = RememberRequest::new(scope(), "the weekly newsletter goes out on Fridays");
    req.sensitivity_hint = Some(SensitivityLevel::Restricted);
    source.remember(req).await.unwrap();

    let ndjson = source.export_ndjson(&selector(false)).await.unwrap();

    let target = engine();
    target.import_ndjson(&ndjson, &tenant()).await.unwrap();
    let stored = target.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].sensitivity,
        SensitivityLevel::Restricted,
        "import downgraded an already-high classification to freshly detected Internal"
    );
}

#[tokio::test]
async fn malformed_ndjson_is_rejected_with_a_useful_error() {
    let e = engine();
    let err = e
        .import_ndjson("{not json at all", &tenant())
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("line 1"),
        "the error must name the bad line: {err}"
    );
}

#[tokio::test]
async fn the_named_bad_line_is_the_ndjson_lines_own_position_not_serde_jsons_internal_one() {
    // `malformed_ndjson_is_rejected_with_a_useful_error` cannot by itself
    // distinguish a correct 1-indexed line number from an off-by-one: its
    // stream is a single physical line, and `serde_json`'s own error message
    // independently contains the substring "line 1" (its own internal
    // position within that one fragment) regardless of what index
    // `import_ndjson` prefixes onto it. This stream puts the malformed JSON
    // on the *second* physical line, so `serde_json`'s internal "line 1"
    // (relative to that one bad fragment) cannot masquerade as the outer
    // "line 2" this test actually requires.
    let e = engine();
    let ndjson = format!(
        "{}\n{}\n",
        r#"{"record":"header","format_version":1,"exported_at":0}"#, "{not json at all"
    );
    let err = e.import_ndjson(&ndjson, &tenant()).await.unwrap_err();
    assert!(
        err.to_string().contains("line 2"),
        "the error must name the ndjson stream's own second line, not serde_json's internal one: {err}"
    );
}

#[tokio::test]
async fn a_blank_line_between_records_is_tolerated_not_rejected() {
    // `Backend::import`'s own doc calls the stream "order-tolerant... this is
    // deliberate — requiring exactly one Header would reject the
    // concatenation of two exports, which migration tooling actually does".
    // A blank line is the everyday residue of exactly that concatenation (or
    // of a hand-edited file), and `import_ndjson` skips one rather than
    // handing it to `serde_json` as an empty, unparseable "record".
    let source = engine();
    seed(&source).await;
    let ndjson = source.export_ndjson(&selector(false)).await.unwrap();

    let mut with_blank_line = String::new();
    let mut lines = ndjson.lines();
    with_blank_line.push_str(lines.next().expect("header line"));
    with_blank_line.push('\n');
    with_blank_line.push('\n'); // the blank line under test
    for line in lines {
        with_blank_line.push_str(line);
        with_blank_line.push('\n');
    }

    let target = engine();
    let report = target
        .import_ndjson(&with_blank_line, &tenant())
        .await
        .unwrap();
    assert_eq!(report.items_imported, 3);
}

#[tokio::test]
async fn exporting_an_empty_scope_yields_a_header_and_nothing_else() {
    let e = engine();
    let ndjson = e.export_ndjson(&selector(false)).await.unwrap();
    assert_eq!(ndjson.lines().count(), 1);
}
