//! The admin routes: per-tenant budgets, policy configuration, and audit
//! retention.
//!
//! **There is no cross-tenant administrator in v1.** An API key is scoped to
//! exactly one tenant, so `{tenant}` in every path here must equal the
//! authenticated tenant — `same_tenant` below is the one place that check is
//! made, and every handler in this file calls it before doing anything else.
//! A mismatch is 403, not a quiet reroute to the caller's own tenant: a
//! misdirected admin request should fail loudly, not configure the wrong
//! tenant.
//!
//! Thin by design, like `memories.rs` and `ops.rs`: nothing here scores,
//! ranks, or decides. Every governance outcome reported came out of an
//! `Engine` method, and every `PolicyChanged` row this surface causes is
//! attributed to the authenticated caller's own `Actor`, never
//! `Actor::system()`.

use crate::AppState;
use crate::auth::Auth;
use crate::error::ApiError;
use crate::json::ValidatedJson;
use crate::query::ValidatedQuery;
use crate::scope::ScopeParams;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use memorysafe_core::{Budget, CapacityState, TenantId};
use memorysafe_engine::RetentionProfile;
use memorysafe_policy::BaselineConfig;
use serde::{Deserialize, Serialize};

/// An API key is scoped to one tenant, so the path segment is a check, not a
/// selector. Making a misdirected admin request fail loudly is the entire
/// point of carrying it.
fn same_tenant(auth: &Auth, path: &str) -> Result<TenantId, ApiError> {
    let requested = TenantId::new(path)?;
    auth.0.authorize_tenant(&requested)?;
    Ok(requested)
}

#[derive(Debug, Serialize)]
pub struct BudgetResponse {
    pub budget: Budget,
    pub used_items: u64,
    pub used_bytes: u64,
}

impl From<CapacityState> for BudgetResponse {
    fn from(state: CapacityState) -> Self {
        Self {
            budget: state.budget,
            used_items: state.used_items,
            used_bytes: state.used_bytes,
        }
    }
}

pub async fn get_budget(
    State(state): State<AppState>,
    auth: Auth,
    Path(tenant): Path<String>,
    headers: HeaderMap,
    ValidatedQuery(scope): ValidatedQuery<ScopeParams>,
) -> Result<Json<BudgetResponse>, ApiError> {
    same_tenant(&auth, &tenant)?;
    let scope = scope.resolve(&state.resolver, &headers)?;
    Ok(Json(state.engine.capacity_state(&scope).await?.into()))
}

/// Budgets are per scope, not per tenant (a `Budget` bounds a namespace), so
/// this carries a namespace even though the route lives under a tenant path.
#[derive(Debug, Deserialize)]
pub struct SetBudgetBody {
    #[serde(flatten)]
    pub scope: ScopeParams,
    pub max_items: Option<u64>,
    pub max_bytes: Option<u64>,
}

pub async fn put_budget(
    State(state): State<AppState>,
    auth: Auth,
    Path(tenant): Path<String>,
    headers: HeaderMap,
    ValidatedJson(body): ValidatedJson<SetBudgetBody>,
) -> Result<Json<BudgetResponse>, ApiError> {
    same_tenant(&auth, &tenant)?;
    let scope = body.scope.resolve(&state.resolver, &headers)?;
    state
        .engine
        .set_budget(
            &scope,
            Budget {
                max_items: body.max_items,
                max_bytes: body.max_bytes,
            },
        )
        .await?;
    Ok(Json(state.engine.capacity_state(&scope).await?.into()))
}

/// **Reports what `Engine::tenant_settings` reports, no more.** With no
/// override on record, `tenant_settings(t).policy_config` falls back to a
/// hardcoded `BaselineConfig::default()`, not the config inside a custom
/// `default_policy` the engine may have been built with (see that method's
/// own doc in `memorysafe-engine`'s `lib.rs`) — an opaque `Arc<dyn
/// GovernancePolicy>` has no way to hand back the config it was built from.
/// So this route can under-report what is actually governing a tenant's
/// writes until `PUT` is called for it at least once. There is no fix
/// available at this layer; this doc exists so the route does not claim more
/// than it delivers.
pub async fn get_policy(
    State(state): State<AppState>,
    auth: Auth,
    Path(tenant): Path<String>,
) -> Result<Json<BaselineConfig>, ApiError> {
    let tenant = same_tenant(&auth, &tenant)?;
    Ok(Json(state.engine.tenant_settings(&tenant).policy_config))
}

#[derive(Debug, Serialize)]
pub struct PolicyChanged {
    #[serde(flatten)]
    pub config: BaselineConfig,
    pub audit_id: String,
}

pub async fn put_policy(
    State(state): State<AppState>,
    auth: Auth,
    Path(tenant): Path<String>,
    ValidatedJson(config): ValidatedJson<BaselineConfig>,
) -> Result<Json<PolicyChanged>, ApiError> {
    let tenant = same_tenant(&auth, &tenant)?;
    let audit_id = state
        .engine
        .set_tenant_policy_config(&tenant, config.clone(), &auth.actor())
        .await?;
    Ok(Json(PolicyChanged {
        config,
        audit_id: audit_id.to_string(),
    }))
}

#[derive(Debug, Serialize)]
pub struct RetentionView {
    pub profile: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_id: Option<String>,
}

pub async fn get_retention(
    State(state): State<AppState>,
    auth: Auth,
    Path(tenant): Path<String>,
) -> Result<Json<RetentionView>, ApiError> {
    let tenant = same_tenant(&auth, &tenant)?;
    Ok(Json(RetentionView {
        profile: state.engine.retention_for(&tenant).name(),
        audit_id: None,
    }))
}

#[derive(Debug, Deserialize)]
pub struct SetRetentionBody {
    pub profile: String,
}

pub async fn put_retention(
    State(state): State<AppState>,
    auth: Auth,
    Path(tenant): Path<String>,
    ValidatedJson(body): ValidatedJson<SetRetentionBody>,
) -> Result<Json<RetentionView>, ApiError> {
    let tenant = same_tenant(&auth, &tenant)?;
    let profile = RetentionProfile::from_name(&body.profile).ok_or_else(|| {
        ApiError::Validation(format!(
            "unknown retention profile '{}'; expected balanced, gdpr_strict, hipaa_retain, or forensic",
            body.profile
        ))
    })?;
    let audit_id = state
        .engine
        .set_tenant_retention(&tenant, profile, &auth.actor())
        .await?;
    Ok(Json(RetentionView {
        profile: profile.name(),
        audit_id: Some(audit_id.to_string()),
    }))
}
