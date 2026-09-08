//! The MCP adapter's view of the scope contract.
//!
//! The contract itself lives in `memorysafe-auth`, so this adapter and the
//! HTTP one cannot disagree about it. What lives here is the two things that
//! are genuinely MCP's: pulling the request's headers out of an rmcp
//! `Extensions`, and mapping `AuthError` onto `ErrorData`.

use memorysafe_auth::{AuthError, Resolved, ScopeResolver};
use rmcp::ErrorData;
use rmcp::model::Extensions;

/// The MCP error surface has no status codes, so the distinction 401 and 403
/// draw is carried in the message rather than lost.
pub fn auth_error(e: AuthError) -> ErrorData {
    ErrorData::invalid_params(e.to_string(), None)
}

/// Resolve one tool or resource call.
///
/// Over stdio there is no HTTP request at all, so an absent `Parts` yields an
/// empty header map rather than an error — a `FixedScope` needs no headers,
/// and an `ApiKeyScope` will refuse the call anyway for want of a credential.
/// That keeps "no credential" as the reason a keyed transport rejects an
/// unauthenticated call, instead of a confusing "no HTTP request context".
pub fn resolve_call(
    resolver: &dyn ScopeResolver,
    extensions: &Extensions,
    namespace: Option<&str>,
) -> Result<Resolved, ErrorData> {
    let headers = extensions
        .get::<http::request::Parts>()
        .map(|parts| parts.headers.clone())
        .unwrap_or_default();
    resolver.resolve(&headers, namespace).map_err(auth_error)
}
