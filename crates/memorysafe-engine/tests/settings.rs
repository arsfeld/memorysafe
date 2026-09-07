use memorysafe_backend::ScopeSelector;
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Actor, ActorKind, AuditEvent, AuditFilter, Scope, SubjectId, TenantId};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest, RetentionProfile};
use memorysafe_policy::{BaselineConfig, BaselinePolicy};
use std::sync::Arc;

fn engine() -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ))
}

fn acme() -> TenantId {
    TenantId::new("acme").unwrap()
}

fn globex() -> TenantId {
    TenantId::new("globex").unwrap()
}

fn scope(tenant: &str) -> Scope {
    Scope::new(tenant, "user-42", "agent").unwrap()
}

fn operator() -> Actor {
    Actor {
        kind: ActorKind::Human,
        id: Some("ops@acme".into()),
    }
}

#[tokio::test]
async fn a_tenant_without_an_override_uses_the_engine_default() {
    let e = engine();
    assert_eq!(
        e.tenant_settings(&acme()).policy_config,
        BaselineConfig::default()
    );
    assert_eq!(e.retention_for(&acme()), RetentionProfile::Balanced);
}

#[tokio::test]
async fn setting_a_policy_config_changes_only_that_tenant() {
    let e = engine();
    let tuned = BaselineConfig {
        merge_threshold: 0.80,
        ..Default::default()
    };
    e.set_tenant_policy_config(&acme(), tuned.clone(), &operator())
        .await
        .unwrap();

    assert_eq!(
        e.tenant_settings(&acme()).policy_config.merge_threshold,
        0.80
    );
    assert_eq!(
        e.tenant_settings(&globex()).policy_config,
        BaselineConfig::default(),
        "one tenant's tuning leaked into another"
    );
}

#[tokio::test]
async fn a_policy_change_writes_exactly_one_audit_record_naming_both_versions() {
    let e = engine();
    e.set_tenant_policy_config(&acme(), BaselineConfig::default(), &operator())
        .await
        .unwrap();

    let admin = Scope::admin(&acme());
    let rows = e
        .audit(
            &admin,
            &AuditFilter {
                events: vec![AuditEvent::PolicyChanged],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].actor, operator());
    assert!(rows[0].scope.is_admin());
    let decision = rows[0]
        .decision
        .as_ref()
        .expect("the transition is the decision");
    assert!(
        !decision.reasons.is_empty(),
        "a policy change must say what changed"
    );
    let detail = &decision.reasons[0].detail;
    assert!(
        detail.contains("baseline"),
        "the reason names the policy: {detail}"
    );
}

#[tokio::test]
async fn an_incoherent_policy_config_is_refused_and_audits_nothing() {
    let e = engine();
    // Merging above the duplicate threshold is unreachable: every candidate
    // that would merge is already rejected as an exact duplicate.
    let broken = BaselineConfig {
        merge_threshold: 0.99,
        duplicate_threshold: 0.90,
        ..Default::default()
    };
    assert!(
        e.set_tenant_policy_config(&acme(), broken, &operator())
            .await
            .is_err()
    );

    let out_of_range = BaselineConfig {
        mmr_lambda: 1.5,
        ..Default::default()
    };
    assert!(
        e.set_tenant_policy_config(&acme(), out_of_range, &operator())
            .await
            .is_err()
    );

    let rows = e
        .audit(&Scope::admin(&acme()), &AuditFilter::default())
        .await
        .unwrap();
    assert!(
        rows.is_empty(),
        "a refused change must leave no trace of having happened"
    );
    assert_eq!(
        e.tenant_settings(&acme()).policy_config,
        BaselineConfig::default()
    );
}

#[tokio::test]
async fn retention_is_per_tenant_and_purge_honours_the_tenant_it_is_purging() {
    let e = engine();
    e.set_tenant_retention(&acme(), RetentionProfile::HipaaRetain, &operator())
        .await
        .unwrap();
    e.set_tenant_retention(&globex(), RetentionProfile::GdprStrict, &operator())
        .await
        .unwrap();

    e.remember(RememberRequest::new(scope("acme"), "a clinical note"))
        .await
        .unwrap();
    e.remember(RememberRequest::new(scope("globex"), "an ordinary note"))
        .await
        .unwrap();

    let user = SubjectId::new("user-42").unwrap();
    let kept = e.purge_subject(&acme(), &user, &operator()).await.unwrap();
    let dropped = e
        .purge_subject(&globex(), &user, &operator())
        .await
        .unwrap();

    assert_eq!(kept.items_removed, 1);
    assert!(
        kept.audit_rows_preserved >= 1,
        "hipaa_retain must keep the decision record"
    );
    assert_eq!(kept.audit_rows_removed, 0);

    assert_eq!(dropped.items_removed, 1);
    assert!(dropped.audit_rows_removed >= 1, "gdpr_strict must cascade");
    assert_eq!(dropped.audit_rows_preserved, 0);
}

#[tokio::test]
async fn an_export_is_audited_with_the_actor_who_asked_for_it() {
    let e = engine();
    e.remember(RememberRequest::new(scope("acme"), "the thing to export"))
        .await
        .unwrap();

    let sel = ScopeSelector {
        tenant: acme(),
        subject: None,
        namespace: None,
        include_audit: false,
    };
    let ndjson = e.export_ndjson_as(&sel, &operator()).await.unwrap();
    assert!(ndjson.contains("the thing to export"));

    let rows = e
        .audit(
            &Scope::admin(&acme()),
            &AuditFilter {
                events: vec![AuditEvent::Exported],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "an export is a governance event, recorded once"
    );
    assert_eq!(rows[0].actor, operator());

    let json = serde_json::to_string(&rows).unwrap();
    assert!(
        !json.contains("the thing to export"),
        "the export audit row leaked a body"
    );
}

#[tokio::test]
async fn an_import_is_audited_with_the_actor_who_asked_for_it() {
    let source = engine();
    source
        .remember(RememberRequest::new(scope("acme"), "the thing to move"))
        .await
        .unwrap();
    let sel = ScopeSelector {
        tenant: acme(),
        subject: None,
        namespace: None,
        include_audit: false,
    };
    let ndjson = source.export_ndjson(&sel).await.unwrap();

    let target = engine();
    let report = target
        .import_ndjson_as(&ndjson, &acme(), &operator())
        .await
        .unwrap();
    assert_eq!(report.items_imported, 1);

    let rows = target
        .audit(
            &Scope::admin(&acme()),
            &AuditFilter {
                events: vec![AuditEvent::Imported],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].actor, operator());
}

#[tokio::test]
async fn a_tenant_policy_actually_governs_that_tenants_writes() {
    // The registry is worthless if the write path still reads the default. A
    // duplicate threshold of 0.0 rejects everything, so the second write to
    // acme must be refused while globex's is admitted.
    //
    // `near_duplicate_floor: 0.0` is also set, deliberately, and not merely
    // for symmetry: `validate` (settings.rs) refuses any config whose
    // `near_duplicate_floor` sits above `merge_threshold` ("hides the
    // neighbours a merge decision would cite as evidence"), and the
    // `BaselineConfig` default (`0.30`) is above this test's
    // `merge_threshold: 0.0` — so leaving it at the default makes this an
    // incoherent config by that rule and `set_tenant_policy_config` below
    // would return `Err`, not `Ok`. `near_duplicate_floor` plays no role in
    // what this test actually checks (it gates evidence reporting, not the
    // reject/merge decision, which `classify` derives from
    // `duplicate_threshold`/`merge_threshold` alone), so setting it to `0.0`
    // changes nothing this test observes.
    let e = engine();
    let everything_is_a_duplicate = BaselineConfig {
        duplicate_threshold: 0.0,
        merge_threshold: 0.0,
        near_duplicate_floor: 0.0,
        ..Default::default()
    };
    e.set_tenant_policy_config(&acme(), everything_is_a_duplicate, &operator())
        .await
        .unwrap();

    e.remember(RememberRequest::new(scope("acme"), "first acme memory"))
        .await
        .unwrap();
    let second = e
        .remember(RememberRequest::new(
            scope("acme"),
            "an entirely unrelated topic",
        ))
        .await
        .unwrap();
    assert!(
        matches!(
            second.action,
            memorysafe_core::Action::Reject | memorysafe_core::Action::Merge { .. }
        ),
        "acme's own policy was not consulted: {:?}",
        second.action
    );

    e.remember(RememberRequest::new(scope("globex"), "first globex memory"))
        .await
        .unwrap();
    let globex_second = e
        .remember(RememberRequest::new(
            scope("globex"),
            "an entirely unrelated topic",
        ))
        .await
        .unwrap();
    assert!(
        matches!(globex_second.action, memorysafe_core::Action::Retain { .. }),
        "globex got acme's policy"
    );
}
