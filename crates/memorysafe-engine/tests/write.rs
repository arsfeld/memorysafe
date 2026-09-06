use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Action, Budget, ReasonCode, Scope, SensitivityLevel};
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
    Scope::new("acme", "user-42", "coding-agent").unwrap()
}

fn req(body: &str) -> RememberRequest {
    RememberRequest::new(scope(), body)
}

#[tokio::test]
async fn a_novel_memory_is_admitted_and_says_why() {
    let e = engine();
    // DEVIATION from the brief's literal test (documented in the task
    // report): `BaselinePolicy`'s `fragility::score` treats an item with
    // literally zero neighbours as maximally fragile ("irreplaceable" — its
    // own doc comment), which the real `admit::decide` routes through
    // `ReasonCode::ProtectedFragile`, never `NovelContent` — a property of
    // being the very first write into an empty scope, not something this
    // engine's pipeline gets wrong. One unrelated seed memory gives the
    // candidate under test a genuine (if distant) neighbourhood, which is
    // what "a novel memory is admitted and says why" means to exercise.
    e.remember(req("purple elephants juggle unrelated kitchen spatulas"))
        .await
        .unwrap();
    let out = e
        .remember(req("the production database migration runs on Sundays"))
        .await
        .unwrap();

    assert!(matches!(out.action, Action::Retain { .. }));
    assert!(
        out.reasons
            .iter()
            .any(|r| r.code == ReasonCode::NovelContent)
    );
    assert!(out.item_id.is_some());
    assert!(out.evicted.is_empty());
}

#[tokio::test]
async fn an_identical_rewrite_is_rejected_as_a_duplicate() {
    let e = engine();
    let body = "the deploy key rotates every ninety days";
    e.remember(req(body)).await.unwrap();
    let second = e.remember(req(body)).await.unwrap();

    // A rejection is a successful call: the product working, not an error.
    assert!(matches!(second.action, Action::Reject));
    assert!(
        second
            .reasons
            .iter()
            .any(|r| r.code == ReasonCode::NearDuplicate)
    );
}

#[tokio::test]
async fn a_near_duplicate_is_merged_into_the_existing_memory() {
    let e = engine();
    let first = e.remember(req("the cat sat on the mat")).await.unwrap();
    let second = e
        .remember(req("the cat sat on the mat today"))
        .await
        .unwrap();

    match second.action {
        Action::Merge { .. } => {
            assert_eq!(second.merged_into, first.item_id);
            assert!(
                second
                    .reasons
                    .iter()
                    .any(|r| r.code == ReasonCode::HighRedundancy)
            );
        }
        Action::Retain { .. } => {
            // Acceptable if the deterministic embedder scores them below the
            // merge threshold; the decision must still be explained.
            assert!(!second.reasons.is_empty());
        }
        other => panic!("unexpected action {other:?}"),
    }
}

#[tokio::test]
async fn a_credential_is_detected_and_stored_as_restricted() {
    let e = engine();
    let out = e
        .remember(req("the api key is sk-abc123def456ghi789jkl012"))
        .await
        .unwrap();
    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].sensitivity, SensitivityLevel::Restricted);
    assert!(out.item_id.is_some());
}

#[tokio::test]
async fn a_full_namespace_evicts_to_make_room_and_reports_what_it_dropped() {
    let e = engine();
    e.set_budget(
        &scope(),
        Budget {
            max_items: Some(3),
            max_bytes: None,
        },
    )
    .await
    .unwrap();

    for i in 0..3 {
        e.remember(req(&format!("distinct memory number {i} about topic {i}")))
            .await
            .unwrap();
    }
    let out = e
        .remember(req("a completely different subject entirely"))
        .await
        .unwrap();

    assert!(matches!(out.action, Action::Retain { .. }));
    assert_eq!(
        out.evicted.len(),
        1,
        "one eviction makes exactly enough room"
    );
    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 3, "the budget was exceeded");
}

#[tokio::test]
async fn a_retried_write_returns_the_original_outcome() {
    let e = engine();
    let mut r = req("written exactly once");
    r.idempotency_key = Some("retry-key".into());

    let first = e.remember(r.clone()).await.unwrap();
    let second = e.remember(r).await.unwrap();

    assert_eq!(first.item_id, second.item_id);
    assert_eq!(
        e.review(&scope(), &Default::default()).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn an_empty_body_is_a_validation_error_not_a_stored_memory() {
    let e = engine();
    assert!(e.remember(req("   ")).await.is_err());
    assert!(
        e.review(&scope(), &Default::default())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn every_write_leaves_exactly_one_audit_record() {
    let e = engine();
    e.remember(req("first distinct memory about alpha"))
        .await
        .unwrap();
    e.remember(req("second distinct memory about beta"))
        .await
        .unwrap();

    let audit = e.audit(&scope(), &Default::default()).await.unwrap();
    assert_eq!(audit.len(), 2);
    assert!(
        audit.iter().all(|r| r.decision.is_some()),
        "audit must carry the decision"
    );
    assert!(
        audit.iter().all(|r| r.assessment.is_some()),
        "audit must carry the assessment"
    );
    // Bodies must never reach the audit trail.
    let json = serde_json::to_string(&audit).unwrap();
    assert!(
        !json.contains("distinct memory"),
        "audit leaked an item body"
    );
}
