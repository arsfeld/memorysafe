use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    AuditEvent, RecallBudget, RecallMode, RecallRequest, Scope, SensitivityLevel,
};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

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

fn recall(query: &str, ceiling: SensitivityLevel, max_items: usize) -> RecallRequest {
    RecallRequest {
        scope: scope(),
        query: Some(query.into()),
        tags_any: vec![],
        kinds: vec![],
        occurred_after: None,
        occurred_before: None,
        mode: RecallMode::WorkingSet,
        budget: RecallBudget {
            max_tokens: Some(4000),
            max_items: Some(max_items),
        },
        sensitivity_ceiling: ceiling,
    }
}

async fn seed(e: &Engine, bodies: &[&str]) {
    for b in bodies {
        e.remember(RememberRequest::new(scope(), b)).await.unwrap();
    }
}

#[tokio::test]
async fn recall_returns_relevant_memories_each_with_a_reason() {
    let e = engine();
    seed(
        &e,
        &[
            "the cat sat on the mat",
            "quarterly revenue exceeded projections",
            "the deployment pipeline runs nightly",
        ],
    )
    .await;

    let ws = e
        .recall(recall(
            "the cat sat on the mat",
            SensitivityLevel::Restricted,
            5,
        ))
        .await
        .unwrap();

    assert!(!ws.items.is_empty());
    assert!(
        ws.items.iter().all(|s| !s.reason.detail.is_empty()),
        "every item needs a reason"
    );
    assert!(ws.audit_id.is_some(), "a recall must be audited");
}

#[tokio::test]
async fn a_restricted_memory_never_reaches_a_caller_cleared_only_to_personal() {
    let e = engine();
    // Detected as Restricted by the credential patterns.
    e.remember(RememberRequest::new(
        scope(),
        "the deploy api key is sk-abc123def456ghi789jkl012 for cats",
    ))
    .await
    .unwrap();
    seed(&e, &["an ordinary note about cats"]).await;

    let ws = e
        .recall(recall("cats", SensitivityLevel::Personal, 10))
        .await
        .unwrap();

    assert!(
        ws.items
            .iter()
            .all(|s| s.item.sensitivity <= SensitivityLevel::Personal),
        "the sensitivity ceiling leaked"
    );
    let json = serde_json::to_string(&ws).unwrap();
    assert!(
        !json.contains("sk-abc123"),
        "a restricted body reached the response"
    );
}

#[tokio::test]
async fn the_item_budget_is_respected_and_the_rest_is_reported() {
    let e = engine();
    let bodies: Vec<String> = (0..8)
        .map(|i| format!("distinct memory {i} concerning subject {i}"))
        .collect();
    let refs: Vec<&str> = bodies.iter().map(|s| s.as_str()).collect();
    seed(&e, &refs).await;

    let ws = e
        .recall(recall("distinct memory", SensitivityLevel::Restricted, 3))
        .await
        .unwrap();
    assert!(ws.items.len() <= 3);
    assert!(!ws.omitted.is_empty(), "what was cut must be reported");
}

#[tokio::test]
async fn search_mode_bypasses_composition_but_not_governance() {
    let e = engine();
    e.remember(RememberRequest::new(
        scope(),
        "the deploy api key is sk-abc123def456ghi789jkl012 for cats",
    ))
    .await
    .unwrap();
    seed(&e, &["an ordinary note about cats"]).await;

    let mut req = recall("cats", SensitivityLevel::Personal, 10);
    req.mode = RecallMode::Search;
    let ws = e.recall(req).await.unwrap();

    assert!(
        ws.items
            .iter()
            .all(|s| s.item.sensitivity <= SensitivityLevel::Personal),
        "search mode must still enforce the ceiling"
    );
    assert!(ws.audit_id.is_some(), "search mode must still be audited");
}

#[tokio::test]
async fn a_recall_over_an_empty_scope_is_empty_not_an_error() {
    let e = engine();
    let ws = e
        .recall(recall("anything", SensitivityLevel::Restricted, 5))
        .await
        .unwrap();
    assert!(ws.items.is_empty());
    assert!(ws.omitted.is_empty());
}

#[tokio::test]
async fn the_recall_audit_record_names_exactly_the_returned_items() {
    let e = engine();
    seed(&e, &["alpha memory about cats", "beta memory about cats"]).await;
    let ws = e
        .recall(recall("cats", SensitivityLevel::Restricted, 1))
        .await
        .unwrap();

    let audit = e
        .audit(
            &scope(),
            &memorysafe_core::AuditFilter {
                events: vec![AuditEvent::Recalled],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(audit.len(), 1);
    // Compare ids, not emptiness. `!items.is_empty()` is satisfied by a
    // backend that writes one arbitrary unrelated id, which would make the
    // message's claim false while the assertion passed.
    let audited: std::collections::BTreeSet<_> =
        audit[0].items.iter().map(|r| r.id().clone()).collect();
    let returned: std::collections::BTreeSet<_> =
        ws.items.iter().map(|s| s.item.id.clone()).collect();
    assert!(
        !returned.is_empty(),
        "the recall returned nothing, so there is nothing for the audit to name"
    );
    assert_eq!(
        audited, returned,
        "the recall audit must name exactly the returned items"
    );
    // **What was cut is deliberately not in the audit record**, and the
    // assertion above positively forbids it: `refs` is built from
    // `composed.items` alone. An omitted item was considered and rejected, so
    // naming it in the audit trail would record ids the caller never received
    // — a compliance artifact listing memories that were not disclosed. The
    // cut is reported to the caller instead, on `WorkingSet::omitted` (a
    // sample) and `WorkingSet::omitted_total` (the count). The test's name
    // says only what it checks.
    let json = serde_json::to_string(&audit).unwrap();
    assert!(
        !json.contains("alpha memory"),
        "the recall audit leaked a body"
    );
}
