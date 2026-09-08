use crate::error::ApiError;
use axum::http::HeaderMap;
use memorysafe_auth::{ApiKeyScope, ScopeResolver};
use memorysafe_core::Scope;
use serde::Deserialize;

/// The namespace, and only the namespace. Tenant and subject come from the
/// credential — see `memorysafe_auth::resolver`'s module documentation for
/// the contract this mirrors.
///
/// Appears as a query parameter on reads and as a flattened body field on
/// writes. Optional in both positions: a request that names no namespace
/// falls back exactly as an MCP call does, so the two adapters agree.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScopeParams {
    pub namespace: Option<String>,
}

impl ScopeParams {
    pub fn resolve(&self, resolver: &ApiKeyScope, headers: &HeaderMap) -> Result<Scope, ApiError> {
        Ok(resolver.resolve(headers, self.namespace.as_deref())?.scope)
    }
}
