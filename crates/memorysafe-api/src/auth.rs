use crate::AppState;
use crate::error::ApiError;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use memorysafe_auth::{AuthError, Authenticated};
use memorysafe_core::{Actor, TenantId};

/// A handler that takes this parameter cannot have skipped authentication —
/// there is no other way to construct one.
pub struct Auth(pub Authenticated);

impl Auth {
    pub fn tenant(&self) -> &TenantId {
        self.0.tenant()
    }

    pub fn actor(&self) -> Actor {
        self.0.actor()
    }
}

/// Strips a case-insensitive `Bearer` scheme from an `Authorization` header
/// value. RFC 7235 auth schemes are case-insensitive, so `bearer <token>` is
/// a valid credential; a literal `strip_prefix("Bearer ")` would refuse a
/// spec-legal client for no reason this crate has any stake in. Still
/// requires exactly the one space `strip_prefix("Bearer ")` required, so
/// `"Bearerx"` (no space) and `"Bearerish"` (a prefix collision with no
/// scheme boundary) are both refused. Mirrors
/// `memorysafe-mcp`'s `scope::strip_bearer`.
fn strip_bearer(value: &str) -> Option<&str> {
    let (scheme, rest) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(rest)
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let presented = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(strip_bearer)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or(AuthError::Missing)?;
        Ok(Auth(state.keys.authenticate(presented)?))
    }
}
