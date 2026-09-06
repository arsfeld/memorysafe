//! `recall`'s `limit` computation —
//! `req.budget.max_items.unwrap_or(DEFAULT_MAX_ITEMS).saturating_mul(OVERFETCH)
//! .clamp(MIN_CANDIDATES, MAX_CANDIDATES)` — decides how many candidates the
//! backend hands to `compose`. None of `tests/read.rs`'s six fixtures ask
//! for more than a handful of items, so nothing there can tell the real
//! overfetch from one shrunk to almost nothing. Confirmed by mutation
//! testing: mutating `OVERFETCH` from `8` down to `1` survives every test in
//! `tests/read.rs`, because `MIN_CANDIDATES`'s floor absorbs the difference
//! for every `max_items` those fixtures use.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{RecallBudget, RecallMode, RecallRequest, Scope, SensitivityLevel};
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

async fn seed_distinct(e: &Engine, n: usize) {
    for i in 0..n {
        e.remember(RememberRequest::new(
            scope(),
            &format!("overfetch probe memory {i} about distinct subject {i}"),
        ))
        .await
        .unwrap();
    }
}

/// With no item cap and a generous token budget, `compose`'s item-count
/// check is vacuous and its token check never binds, so it selects
/// essentially every candidate the backend hands it (`BaselinePolicy`'s MMR
/// fill keeps consuming candidates until none remain, per
/// `compose::working_set`). Seeding more items than `MIN_CANDIDATES` and
/// asserting more than `MIN_CANDIDATES` come back therefore pins that
/// `DEFAULT_MAX_ITEMS` and `OVERFETCH` genuinely widen the backend fetch
/// past the clamp's floor rather than the floor alone deciding it — an
/// `OVERFETCH` of `1` (or a `DEFAULT_MAX_ITEMS` shrunk to a handful) would
/// leave the floor as the only thing keeping the fetch above zero, capping
/// this at 10 items instead of past it.
#[tokio::test]
async fn recall_with_no_item_cap_overfetches_past_the_clamp_floor() {
    let e = engine();
    seed_distinct(&e, 15).await;

    let req = RecallRequest {
        scope: scope(),
        query: Some("overfetch probe memory".into()),
        tags_any: vec![],
        kinds: vec![],
        occurred_after: None,
        occurred_before: None,
        mode: RecallMode::WorkingSet,
        budget: RecallBudget {
            max_tokens: Some(1_000_000),
            max_items: None,
        },
        sensitivity_ceiling: SensitivityLevel::Restricted,
    };
    let ws = e.recall(req).await.unwrap();

    assert!(
        ws.items.len() > 10,
        "expected more than the clamp floor's worth of items with no item cap and a huge \
         token budget, got {}",
        ws.items.len()
    );
}

/// The test above cannot isolate `OVERFETCH` from `DEFAULT_MAX_ITEMS`: with
/// `max_items: None`, `DEFAULT_MAX_ITEMS * OVERFETCH == 160` either way an
/// `OVERFETCH` of `1` still gives `DEFAULT_MAX_ITEMS * 1 == 20`, comfortably
/// above the 15-item corpus — confirmed by mutation testing, where shrinking
/// `OVERFETCH` to `1` survives that test. Supplying an explicit `max_items`
/// bypasses `DEFAULT_MAX_ITEMS` entirely and isolates `OVERFETCH`'s own
/// multiplication: with `max_items: Some(3)`, the real limit is
/// `3 * OVERFETCH == 24` (clamp does not bind, `24` and `10` both exceed
/// nothing relevant), comfortably above the 15-item corpus, so all 15 reach
/// `compose`, 3 are selected, and `omitted_total == 12` exactly. With
/// `OVERFETCH` shrunk to `1`, the limit becomes `3 * 1 == 3`, clamped up to
/// the floor `MIN_CANDIDATES == 10` — strictly less than 15 — so the backend
/// truncates to 10 before `compose` ever runs, 3 are selected, and
/// `omitted_total == 7` instead.
#[tokio::test]
async fn recall_with_a_small_explicit_item_cap_still_overfetches_by_the_configured_factor() {
    let e = engine();
    seed_distinct(&e, 15).await;

    let req = RecallRequest {
        scope: scope(),
        query: Some("overfetch probe memory".into()),
        tags_any: vec![],
        kinds: vec![],
        occurred_after: None,
        occurred_before: None,
        mode: RecallMode::WorkingSet,
        budget: RecallBudget {
            max_tokens: Some(1_000_000),
            max_items: Some(3),
        },
        sensitivity_ceiling: SensitivityLevel::Restricted,
    };
    let ws = e.recall(req).await.unwrap();

    assert_eq!(
        ws.items.len(),
        3,
        "the item budget of 3 must still be respected"
    );
    assert_eq!(
        ws.omitted_total, 12,
        "expected all 15 seeded candidates to reach the policy (3 selected, 12 omitted); \
         a lower value means the backend truncated the candidate set before compose ever saw it"
    );
}

/// `MIN_CANDIDATES` (the clamp's floor) is pinned precisely via
/// `omitted_total`, which `compose::working_set` sets to exactly "candidates
/// it was handed minus items it selected" (`ranked.len() - chosen.len()`,
/// and `ranked` is always the full `candidates` slice it was given — see
/// that module's own construction).
///
/// Nine candidates all match the query; with `max_items: Some(1)` the naive
/// limit before clamping is `1 * OVERFETCH == 8`, strictly less than 9, so
/// only the floor keeps the backend from truncating the fusion result down
/// to 8 and silently dropping one of the nine before `compose` ever sees it.
/// With the real floor (`10 >= 9`) all nine survive `retrieve.rs`'s final
/// `out.truncate(query.limit)`, `compose` sees all nine, selects exactly one
/// (its budget), and `omitted_total == 8`. A missing or lower floor would
/// instead report `omitted_total == 7` — the backend would have already
/// dropped one candidate before the policy ever ran. See mutation evidence
/// in the task report: removing the floor from the `clamp` call changes
/// this exact number.
#[tokio::test]
async fn recall_with_a_tiny_item_budget_still_offers_the_policy_at_least_min_candidates() {
    let e = engine();
    seed_distinct(&e, 9).await;

    let req = RecallRequest {
        scope: scope(),
        query: Some("overfetch probe memory".into()),
        tags_any: vec![],
        kinds: vec![],
        occurred_after: None,
        occurred_before: None,
        mode: RecallMode::WorkingSet,
        budget: RecallBudget {
            max_tokens: Some(4000),
            max_items: Some(1),
        },
        sensitivity_ceiling: SensitivityLevel::Restricted,
    };
    let ws = e.recall(req).await.unwrap();

    assert_eq!(
        ws.items.len(),
        1,
        "the item budget of 1 must still be respected"
    );
    assert_eq!(
        ws.omitted_total, 8,
        "expected all 9 seeded candidates to reach the policy (1 selected, 8 omitted); \
         a lower value means the backend truncated the candidate set before compose ever saw it"
    );
}
