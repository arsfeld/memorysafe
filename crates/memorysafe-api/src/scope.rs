use crate::auth::Auth;
use crate::error::ApiError;
use memorysafe_core::Scope;
use serde::Deserialize;

/// Subject and namespace, always both, never defaulted. Appears as query
/// parameters on reads and as flattened body fields on writes.
#[derive(Debug, Clone, Deserialize)]
pub struct ScopeParams {
    pub subject: String,
    pub namespace: String,
}

impl ScopeParams {
    /// The tenant comes from the credential; only subject and namespace come
    /// from the request. A cross-tenant scope is unrepresentable here.
    pub fn resolve(&self, auth: &Auth) -> Result<Scope, ApiError> {
        resolve(auth, &self.subject, &self.namespace)
    }
}

/// The same rule for query strings, which cannot use `#[serde(flatten)]`:
/// `serde_urlencoded` buffers flattened values as strings and every numeric
/// field beside them then fails to deserialize. Query handlers spell the two
/// fields out and call this.
pub fn resolve(auth: &Auth, subject: &str, namespace: &str) -> Result<Scope, ApiError> {
    Ok(auth.0.scope(subject, namespace)?)
}
