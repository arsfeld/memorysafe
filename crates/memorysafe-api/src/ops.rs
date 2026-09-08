//! The operational routes: audit, maintain, export, import, purge.
//!
//! Thin by design, like `memories.rs` — every governance outcome reported
//! here came out of an `Engine` method. The one piece of route-local logic
//! this file still has (deciding which `AuditEvent` names a caller's
//! comma-separated `event` filter names, and refusing an unknown one) is
//! request parsing, not policy: it decides nothing about which memories or
//! audit rows exist, only which ones the caller asked to see.

use crate::AppState;
use crate::auth::Auth;
use crate::error::ApiError;
use crate::json::ValidatedJson;
use crate::query::ValidatedQuery;
use crate::scope::ScopeParams;
use crate::text::ValidatedText;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use memorysafe_backend::{ImportReport, ScopeSelector};
use memorysafe_core::{AuditEvent, AuditFilter, AuditId, AuditRecord, ItemId};
use memorysafe_engine::{MaintainCursor, MaintainReport, PurgeOutcome};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

fn timestamp(seconds: Option<i64>, field: &str) -> Result<Option<OffsetDateTime>, ApiError> {
    seconds
        .map(|s| {
            OffsetDateTime::from_unix_timestamp(s)
                .map_err(|_| ApiError::Validation(format!("{field} is not a valid Unix timestamp")))
        })
        .transpose()
}

/// No `#[serde(flatten)]` and no `Vec`: axum's `Query` deserializes with
/// `serde_urlencoded`, which supports neither. Events arrive as one
/// comma-separated value — `?event=admitted,forgotten` — and an unknown name
/// is a 400 rather than a silently empty filter.
#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    #[serde(flatten)]
    pub scope: ScopeParams,
    pub event: Option<String>,
    pub item: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub after: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct AuditResponse {
    pub records: Vec<AuditRecord>,
    /// True when more rows matched than were returned. Continue with
    /// `after=<id of the last record>`.
    pub truncated: bool,
}

pub async fn audit(
    State(state): State<AppState>,
    headers: HeaderMap,
    ValidatedQuery(query): ValidatedQuery<AuditQuery>,
) -> Result<Json<AuditResponse>, ApiError> {
    let scope = query.scope.resolve(&state.resolver, &headers)?;

    let events = query
        .event
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|raw| {
            serde_json::from_value::<AuditEvent>(serde_json::Value::String(raw.to_owned()))
                .map_err(|_| ApiError::Validation(format!("unknown audit event '{raw}'")))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let default_filter = AuditFilter::default();
    let requested = query.limit.unwrap_or(default_filter.limit);
    let filter = AuditFilter {
        events,
        subject: None,
        namespace: None,
        after: query
            .after
            .as_deref()
            .map(AuditId::parse)
            .transpose()
            .map_err(|e| ApiError::Validation(format!("bad cursor: {e}")))?,
        item: query
            .item
            .as_deref()
            .map(ItemId::parse)
            .transpose()
            .map_err(|e| ApiError::Validation(format!("bad item id: {e}")))?,
        since: timestamp(query.since, "since")?,
        until: timestamp(query.until, "until")?,
        // Overwritten inside `Engine::audit_page` before the backend ever
        // sees it (`requested.saturating_add(1)`) — see that method's doc.
        // Left as `requested` here rather than some placeholder so this
        // struct is still a coherent `AuditFilter` in its own right.
        limit: requested,
    };

    // Ruling D: the "ask for one extra row, then report `truncated`"
    // protocol lives in `Engine::audit_page`, not here — see its doc for
    // why. This route supplies the caller's page and forwards exactly what
    // came back.
    let (records, truncated) = state.engine.audit_page(&scope, &filter, requested).await?;
    Ok(Json(AuditResponse { records, truncated }))
}

#[derive(Debug, Deserialize)]
pub struct MaintainBody {
    #[serde(flatten)]
    pub scope: ScopeParams,
    pub cursor: Option<usize>,
}

pub async fn maintain(
    State(state): State<AppState>,
    headers: HeaderMap,
    ValidatedJson(body): ValidatedJson<MaintainBody>,
) -> Result<Json<MaintainReport>, ApiError> {
    let scope = body.scope.resolve(&state.resolver, &headers)?;
    let cursor = body.cursor.map(|offset| MaintainCursor { offset });
    Ok(Json(state.engine.maintain(&scope, cursor).await?))
}

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    pub namespace: Option<String>,
    #[serde(default)]
    pub include_audit: bool,
    /// `ndjson` (default) round-trips; `markdown` is for a person to read.
    pub format: Option<String>,
}

pub async fn export(
    State(state): State<AppState>,
    auth: Auth,
    _headers: HeaderMap,
    ValidatedQuery(query): ValidatedQuery<ExportQuery>,
) -> Result<Response, ApiError> {
    let mut selector = ScopeSelector::for_subject(auth.tenant().clone(), auth.subject().clone());
    selector.include_audit = query.include_audit;

    if let Some(ns) = &query.namespace
        && !ns.is_empty()
    {
        memorysafe_auth::check_reserved(None, Some(ns.as_str()))?;
        let namespace = memorysafe_core::Namespace::new(ns)?;
        selector = selector.in_namespace(namespace);
    }

    match query.format.as_deref() {
        None | Some("ndjson") => {
            // Audited: an export is a governance event once a person or an API
            // caller initiates it.
            let body = state
                .engine
                .export_ndjson_as(&selector, &auth.actor())
                .await?;
            Ok(([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response())
        }
        Some("markdown") => {
            // Audited, same as the ndjson arm above: `docs/known-gaps.md`
            // accepted deferring `Engine::export`'s missing audit record on
            // the explicit condition that Plan 3 expose neither surface
            // without a ceiling and a record. `export_markdown_as` mirrors
            // `export_ndjson_as` so this arm meets that condition too.
            let body = state
                .engine
                .export_markdown_as(&selector, &auth.actor())
                .await?;
            Ok((
                [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
                body,
            )
                .into_response())
        }
        Some(other) => Err(ApiError::Validation(format!(
            "unknown export format '{other}'; expected ndjson or markdown"
        ))),
    }
}

pub async fn import(
    State(state): State<AppState>,
    auth: Auth,
    ValidatedText(ndjson): ValidatedText,
) -> Result<Json<ImportReport>, ApiError> {
    Ok(Json(
        state
            .engine
            .import_ndjson_as(&ndjson, auth.tenant(), &auth.actor())
            .await?,
    ))
}

pub async fn purge_subject(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<PurgeOutcome>, ApiError> {
    Ok(Json(
        state
            .engine
            .purge_subject(auth.tenant(), auth.subject(), &auth.actor())
            .await?,
    ))
}
