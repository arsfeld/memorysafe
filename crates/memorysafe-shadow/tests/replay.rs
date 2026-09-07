use memorysafe_backend::ScopeSelector;
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Scope, TenantId};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use memorysafe_shadow::{TracedAction, replay, run};
use std::sync::Arc;

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

async fn seeded_export(include_audit: bool) -> String {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.path().to_path_buf())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ));
    for body in [
        "the production migration runs on Sundays",
        "the on-call rotation starts Monday morning",
        "the staging cluster is rebuilt every night",
    ] {
        engine
            .remember(RememberRequest::new(scope(), body))
            .await
            .unwrap();
    }
    // A rejected write: recorded, but unreplayable by construction.
    engine
        .remember(RememberRequest::new(
            scope(),
            "the production migration runs on Sundays",
        ))
        .await
        .unwrap();

    engine
        .export_ndjson(&ScopeSelector {
            tenant: TenantId::new("acme").unwrap(),
            subject: None,
            namespace: None,
            include_audit,
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn an_archive_with_audit_becomes_a_scenario_of_the_admitted_writes() {
    let ndjson = seeded_export(true).await;
    let r = replay::from_export_ndjson("live", &ndjson).unwrap();

    assert_eq!(r.scenario.writes.len(), 3, "three admissions");
    assert!(
        !r.scenario.unreplayable.is_empty(),
        "the rejected write must be reported, not silently dropped"
    );
    assert!(r.scenario.coverage() < 1.0);
    assert!(r.scenario.coverage() > 0.5);

    let bodies: Vec<&str> = r.scenario.writes.iter().map(|w| w.body.as_str()).collect();
    assert!(bodies.contains(&"the production migration runs on Sundays"));
}

#[tokio::test]
async fn the_recorded_decisions_come_back_as_a_trace_that_diffs_against_a_replay() {
    let ndjson = seeded_export(true).await;
    let r = replay::from_export_ndjson("live", &ndjson).unwrap();
    let recorded = r
        .recorded
        .expect("an archive with audit has recorded decisions");

    assert_eq!(recorded.decisions.len(), r.scenario.writes.len());
    for decision in &recorded.decisions {
        assert!(matches!(decision.action, TracedAction::Retain { .. }));
    }

    let replayed = run(&r.scenario, Arc::new(BaselinePolicy::default()))
        .await
        .unwrap();
    let d = memorysafe_shadow::diff(&recorded, &replayed).unwrap();
    assert_eq!(d.total, 3);
    // The same policy over the same writes must still admit all three; the
    // reasons may differ because the replay corpus is smaller.
    assert_eq!(
        d.transitions.values().filter(|_| true).count(),
        d.transitions.len(),
        "sanity"
    );
    assert!(
        !d.transitions.keys().any(|k| k.ends_with("->reject")),
        "replaying the same policy turned an admission into a rejection: {:?}",
        d.transitions
    );
}

#[tokio::test]
async fn an_archive_without_audit_still_yields_a_scenario_in_creation_order() {
    let ndjson = seeded_export(false).await;
    let r = replay::from_export_ndjson("no-audit", &ndjson).unwrap();

    assert_eq!(r.scenario.writes.len(), 3);
    assert!(
        r.recorded.is_none(),
        "there is nothing recorded to compare against"
    );
    assert!(r.scenario.unreplayable.is_empty());
}

#[tokio::test]
async fn a_stream_that_is_not_an_export_is_a_clear_error() {
    assert!(replay::from_export_ndjson("junk", "not json at all\n").is_err());
    assert!(replay::from_export_ndjson("junk", "{\"record\":\"nonsense\"}\n").is_err());
    // An empty stream is not an error; it is an empty scenario.
    let empty = replay::from_export_ndjson("empty", "").unwrap();
    assert!(empty.scenario.writes.is_empty());
}

#[tokio::test]
async fn a_replayed_scenario_carries_the_scope_each_write_belonged_to() {
    let ndjson = seeded_export(true).await;
    let r = replay::from_export_ndjson("live", &ndjson).unwrap();
    for write in &r.scenario.writes {
        assert_eq!(write.scope, scope());
    }
}

/// An `Admitted` audit row whose item is not in this archive — a narrower
/// item export than audit export, or an item purged after the audit row was
/// written — must be counted in `unreplayable`, never silently absent from
/// both `writes` and `unreplayable`. Coverage is supposed to say how much of
/// a real log a shadow run exercised; a row that just vanishes would make
/// that number lie by omission, and `assert!(...is_ok())` alone would not
/// catch it.
#[tokio::test]
async fn an_admitted_item_missing_from_the_archive_is_reported_not_silently_dropped() {
    let ndjson = seeded_export(true).await;

    // Drop exactly one `Item` line, as if the corpus were exported for a
    // narrower scope than the audit trail (or the item were purged
    // afterwards). Its `Admitted` audit row stays in the stream.
    let mut already_dropped_one = false;
    let filtered: String = ndjson
        .lines()
        .filter(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            if !already_dropped_one && value["record"] == "item" {
                already_dropped_one = true;
                return false;
            }
            true
        })
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(
        already_dropped_one,
        "the seeded export must carry at least one item line"
    );

    let full = replay::from_export_ndjson("live", &ndjson).unwrap();
    let with_a_gap = replay::from_export_ndjson("live", &filtered).unwrap();

    assert_eq!(
        with_a_gap.scenario.writes.len(),
        full.scenario.writes.len() - 1,
        "the item missing from the archive must not silently appear as a write"
    );
    assert_eq!(
        with_a_gap.scenario.unreplayable.len(),
        full.scenario.unreplayable.len() + 1,
        "it must be counted as unreplayable instead of vanishing uncounted"
    );
    assert!(
        with_a_gap
            .scenario
            .unreplayable
            .iter()
            .any(|u| u.why.contains("not in this archive")),
        "the reason must say why: {:?}",
        with_a_gap.scenario.unreplayable
    );
}
