//! `Engine::recall` builds `HardFilters` by copying `tags_any`, `kinds`,
//! `occurred_after` and `occurred_before` straight off the `RecallRequest`.
//! `tests/read.rs` never sets any of the four (every fixture there leaves
//! them at their empty/`None` defaults), so nothing in that suite can tell a
//! `read.rs` that forwards these fields from one that silently drops them.
//! Confirmed by mutation testing: hardcoding any one of
//! `tags_any`/`kinds`/`occurred_after`/`occurred_before` to its empty value
//! inside `recall` survives every test in `tests/read.rs`. The global
//! constraints call these out by name alongside the sensitivity ceiling
//! (which mandated tests 2 and 4 do cover) as filters that "execute inside
//! the backend query" — this file is the same guarantee for the three the
//! mandated suite leaves unchecked.

use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{RecallBudget, RecallMode, RecallRequest, Scope, SensitivityLevel};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
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

fn base_req(query: &str) -> RecallRequest {
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
            max_items: Some(10),
        },
        sensitivity_ceiling: SensitivityLevel::Restricted,
    }
}

#[tokio::test]
async fn recall_honors_the_tags_any_filter() {
    let e = engine();
    let mut tagged = RememberRequest::new(scope(), "a memory about cats and work deadlines");
    tagged.tags = vec!["work".into()];
    e.remember(tagged).await.unwrap();
    let untagged = RememberRequest::new(scope(), "a memory about cats and weekend plans");
    e.remember(untagged).await.unwrap();

    let mut req = base_req("cats");
    req.tags_any = vec!["work".into()];
    let ws = e.recall(req).await.unwrap();

    assert!(!ws.items.is_empty(), "the tagged item should still match");
    assert!(
        ws.items
            .iter()
            .all(|s| s.item.tags.iter().any(|t| t == "work")),
        "an item without the requested tag reached a tags_any-filtered recall"
    );
}

#[tokio::test]
async fn recall_honors_the_kinds_filter() {
    let e = engine();
    let mut fact = RememberRequest::new(scope(), "a fact about cats sleeping all day");
    fact.kind = "fact".into();
    e.remember(fact).await.unwrap();
    let mut note = RememberRequest::new(scope(), "a note about cats playing all day");
    note.kind = "note".into();
    e.remember(note).await.unwrap();

    let mut req = base_req("cats");
    req.kinds = vec!["note".into()];
    let ws = e.recall(req).await.unwrap();

    assert!(
        !ws.items.is_empty(),
        "the note-kind item should still match"
    );
    assert!(
        ws.items.iter().all(|s| s.item.kind == "note"),
        "an item of a kind outside the filter reached a kinds-filtered recall"
    );
}

#[tokio::test]
async fn recall_honors_occurred_time_bounds() {
    let e = engine();
    let base = OffsetDateTime::UNIX_EPOCH;
    let mut early = RememberRequest::new(scope(), "a cats memory from early in the year");
    early.occurred_at = Some(base);
    e.remember(early).await.unwrap();
    let mut late = RememberRequest::new(scope(), "a cats memory from late in the year");
    late.occurred_at = Some(base + Duration::days(300));
    e.remember(late).await.unwrap();

    let mut req = base_req("cats");
    req.occurred_after = Some(base + Duration::days(100));
    let ws = e.recall(req).await.unwrap();

    assert!(!ws.items.is_empty(), "the later item should still match");
    assert!(
        ws.items.iter().all(|s| s
            .item
            .occurred_at
            .is_some_and(|t| t >= base + Duration::days(100))),
        "an item before occurred_after reached a time-bounded recall"
    );
}
