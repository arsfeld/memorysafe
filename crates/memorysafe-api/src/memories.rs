//! The memory routes: recall, remember, review, get, delete, forget, protect.
//!
//! Thin by design — every governance outcome reported here came out of an
//! `Engine` method. No scoring, ranking, filtering or deciding happens in
//! this file.

use crate::AppState;
use crate::auth::Auth;
use crate::error::ApiError;
use crate::json::ValidatedJson;
use crate::query::ValidatedQuery;
use crate::scope::ScopeParams;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use memorysafe_backend::Page;
use memorysafe_core::{
    ItemId, MemoryItem, RecallBudget, RecallMode, RecallRequest, SensitivityLevel, Source,
    SourceKind, WorkingSet,
};
use memorysafe_engine::{ForgetOutcome, ForgetSelector, RememberRequest, WriteOutcome};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use time::{Duration, OffsetDateTime};

fn item_id(raw: &str) -> Result<ItemId, ApiError> {
    ItemId::parse(raw).map_err(|e| ApiError::Validation(format!("bad item id '{raw}': {e}")))
}

fn timestamp(seconds: Option<i64>, field: &str) -> Result<Option<OffsetDateTime>, ApiError> {
    seconds
        .map(|s| {
            OffsetDateTime::from_unix_timestamp(s)
                .map_err(|_| ApiError::Validation(format!("{field} is not a valid Unix timestamp")))
        })
        .transpose()
}

#[derive(Debug, Deserialize)]
pub struct RememberBody {
    #[serde(flatten)]
    pub scope: ScopeParams,
    pub body: String,
    pub kind: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub attrs: BTreeMap<String, Value>,
    pub source_kind: Option<SourceKind>,
    pub source_id: Option<String>,
    pub occurred_at: Option<i64>,
    pub sensitivity_hint: Option<SensitivityLevel>,
    pub ttl_seconds: Option<i64>,
    pub idempotency_key: Option<String>,
}

pub async fn remember(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    ValidatedJson(body): ValidatedJson<RememberBody>,
) -> Result<Json<WriteOutcome>, ApiError> {
    let scope = body.scope.resolve(&state.resolver, &headers)?;
    let mut req = RememberRequest::new(scope, &body.body);
    req.actor = auth.actor();
    if let Some(kind) = body.kind {
        req.kind = kind;
    }
    req.tags = body.tags;
    req.attrs = body.attrs;
    req.source = Source {
        kind: body.source_kind.unwrap_or(SourceKind::Agent),
        id: body.source_id,
    };
    req.occurred_at = timestamp(body.occurred_at, "occurred_at")?;
    req.sensitivity_hint = body.sensitivity_hint;
    req.ttl = body.ttl_seconds.map(Duration::seconds);
    req.idempotency_key = body.idempotency_key;

    Ok(Json(state.engine.remember(req).await?))
}

#[derive(Debug, Deserialize)]
pub struct RecallBody {
    #[serde(flatten)]
    pub scope: ScopeParams,
    pub query: Option<String>,
    #[serde(default)]
    pub tags_any: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    pub occurred_after: Option<i64>,
    pub occurred_before: Option<i64>,
    #[serde(default)]
    pub mode: RecallMode,
    #[serde(default)]
    pub budget: RecallBudget,
    pub sensitivity_ceiling: Option<SensitivityLevel>,
}

pub async fn recall(
    State(state): State<AppState>,
    headers: HeaderMap,
    ValidatedJson(body): ValidatedJson<RecallBody>,
) -> Result<Json<WorkingSet>, ApiError> {
    let req = RecallRequest {
        scope: body.scope.resolve(&state.resolver, &headers)?,
        query: body.query,
        tags_any: body.tags_any,
        kinds: body.kinds,
        occurred_after: timestamp(body.occurred_after, "occurred_after")?,
        occurred_before: timestamp(body.occurred_before, "occurred_before")?,
        mode: body.mode,
        budget: body.budget,
        // No per-key clearance exists in v1, so a caller must name the
        // ceiling explicitly to see anything above it. Fails closed to
        // `Internal`, matching `memorysafe_backend::query::HardFilters`'s own
        // documented default for this exact field (`// Fail closed.`) and
        // `memorysafe-mcp`'s `tools_write.rs`'s identical default for the
        // same reason: the cost of guessing too narrow (a visible, reported
        // annoyance a caller fixes by passing the argument) is not symmetric
        // with the cost of guessing too wide (a silent over-disclosure
        // nobody notices).
        sensitivity_ceiling: body
            .sensitivity_ceiling
            .unwrap_or(SensitivityLevel::Internal),
    };
    Ok(Json(state.engine.recall(req).await?))
}

#[derive(Debug, Deserialize)]
pub struct ReviewQuery {
    #[serde(flatten)]
    pub scope: ScopeParams,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ReviewResponse {
    pub items: Vec<MemoryItem>,
    pub offset: usize,
    pub limit: usize,
}

pub async fn review(
    State(state): State<AppState>,
    headers: HeaderMap,
    ValidatedQuery(query): ValidatedQuery<ReviewQuery>,
) -> Result<Json<ReviewResponse>, ApiError> {
    let scope = query.scope.resolve(&state.resolver, &headers)?;
    let default_page = Page::default();
    let page = Page {
        offset: query.offset.unwrap_or(default_page.offset),
        limit: query.limit.unwrap_or(default_page.limit),
    };
    let items = state.engine.review(&scope, &page).await?;
    Ok(Json(ReviewResponse {
        items,
        offset: page.offset,
        // Not `page.limit`: the backend clamps to `Page::effective_limit()`
        // (`MAX_PAGE_LIMIT`) before running the query, and this echo is the
        // only exhaustion signal a caller has — no `truncated` flag exists,
        // by this workspace's own paging convention (`AuditFilter::limit`'s
        // doc states the same rule). Echoing the raw, unclamped request would
        // make a caller asking for more than the ceiling see fewer items
        // than the (wrong) limit it was told, and wrongly conclude the scope
        // was exhausted — silently hiding part of what is stored, exactly
        // the failure `memorysafe-mcp`'s `tools_curate.rs::memory_review`
        // (the same shape, over MCP) already fixed and documents in full.
        limit: page.effective_limit(),
    }))
}

pub async fn get_one(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    ValidatedQuery(scope): ValidatedQuery<ScopeParams>,
) -> Result<Json<MemoryItem>, ApiError> {
    let scope = scope.resolve(&state.resolver, &headers)?;
    let id = item_id(&id)?;
    state
        .engine
        .get(&scope, &id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("no memory {id} in this scope")))
}

pub async fn delete_one(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    ValidatedQuery(scope): ValidatedQuery<ScopeParams>,
) -> Result<Json<ForgetOutcome>, ApiError> {
    let scope = scope.resolve(&state.resolver, &headers)?;
    let id = item_id(&id)?;
    Ok(Json(
        state
            .engine
            .forget(&scope, ForgetSelector::Ids(vec![id]))
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct ForgetBody {
    #[serde(flatten)]
    pub scope: ScopeParams,
    pub ids: Option<Vec<String>>,
    pub tag: Option<String>,
    pub kind: Option<String>,
}

pub async fn forget(
    State(state): State<AppState>,
    headers: HeaderMap,
    ValidatedJson(body): ValidatedJson<ForgetBody>,
) -> Result<Json<ForgetOutcome>, ApiError> {
    let scope = body.scope.resolve(&state.resolver, &headers)?;

    let given = [body.ids.is_some(), body.tag.is_some(), body.kind.is_some()]
        .into_iter()
        .filter(|present| *present)
        .count();
    if given != 1 {
        return Err(ApiError::Validation(
            "exactly one of 'ids', 'tag', or 'kind' is required".into(),
        ));
    }

    let selector = if let Some(ids) = body.ids {
        ForgetSelector::Ids(ids.iter().map(|r| item_id(r)).collect::<Result<_, _>>()?)
    } else if let Some(tag) = body.tag {
        ForgetSelector::Tag(tag)
    } else {
        ForgetSelector::Kind(body.kind.expect("checked above"))
    };
    Ok(Json(state.engine.forget(&scope, selector).await?))
}

#[derive(Debug, Deserialize)]
pub struct ProtectBody {
    #[serde(flatten)]
    pub scope: ScopeParams,
    pub level: String,
    pub until: Option<i64>,
}

pub async fn protect(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    ValidatedJson(body): ValidatedJson<ProtectBody>,
) -> Result<Json<WriteOutcome>, ApiError> {
    let scope = body.scope.resolve(&state.resolver, &headers)?;
    let id = item_id(&id)?;
    // The five-branch decision table lives in `memorysafe_core::parse_protection`,
    // not here — see `ApiError`'s `From<ProtectionParseError>` impl for why.
    let protection = memorysafe_core::parse_protection(&body.level, body.until)?;
    Ok(Json(state.engine.protect(&scope, &id, protection).await?))
}
