use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use memorysafe_auth::AuthError;
use memorysafe_core::CoreError;
use memorysafe_engine::EngineError;
use serde::Serialize;

/// The error body. One shape for every failure, so a client can parse once.
#[derive(Debug, Serialize)]
pub struct Problem {
    /// A stable machine-readable tag. Never a message.
    pub error: &'static str,
    pub message: String,
    /// True only when trying the same request again could succeed.
    pub retryable: bool,
}

#[derive(Debug)]
pub enum ApiError {
    Validation(String),
    Unauthenticated(String),
    Forbidden(String),
    NotFound(String),
    Conflict(String),
    Unavailable { message: String, retryable: bool },
    Internal(String),
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str, bool) {
        match self {
            ApiError::Validation(_) => (StatusCode::BAD_REQUEST, "validation", false),
            ApiError::Unauthenticated(_) => (StatusCode::UNAUTHORIZED, "unauthenticated", false),
            ApiError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden", false),
            ApiError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found", false),
            ApiError::Conflict(_) => (StatusCode::CONFLICT, "conflict", false),
            ApiError::Unavailable { retryable, .. } => {
                (StatusCode::SERVICE_UNAVAILABLE, "backend", *retryable)
            }
            ApiError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal", false),
        }
    }

    fn message(&self) -> &str {
        match self {
            ApiError::Validation(m)
            | ApiError::Unauthenticated(m)
            | ApiError::Forbidden(m)
            | ApiError::NotFound(m)
            | ApiError::Conflict(m)
            | ApiError::Internal(m) => m,
            ApiError::Unavailable { message, .. } => message,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, retryable) = self.parts();
        // 5xx is a failure of ours; 4xx is the caller's request. Only the first
        // is worth a log line at warn level.
        if status.is_server_error() {
            tracing::warn!(status = %status, error, message = self.message(), "request failed");
        }
        let body = Problem {
            error,
            message: self.message().to_owned(),
            retryable,
        };
        (status, Json(body)).into_response()
    }
}

impl From<EngineError> for ApiError {
    fn from(e: EngineError) -> Self {
        let retryable = e.is_retryable();
        match e {
            EngineError::Validation(m) => ApiError::Validation(m),
            EngineError::NotFound(m) => ApiError::NotFound(m),
            EngineError::Conflict(m) => ApiError::Conflict(m),
            EngineError::Backend(ref inner) => ApiError::Unavailable {
                message: inner.to_string(),
                retryable,
            },
            // The engine degrades rather than failing when the embedder is
            // unavailable, so reaching here means the degradation itself
            // broke. `retryable` comes from `is_retryable()` above, same as
            // the `Backend` arm — not hardcoded, so this stays truthful if
            // `is_retryable()`'s own rule ever changes.
            EngineError::Embedder(ref inner) => ApiError::Unavailable {
                message: inner.to_string(),
                retryable,
            },
            EngineError::PolicyRefused(m) => ApiError::Internal(m),
        }
    }
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        let message = e.to_string();
        // Exhaustive over every `AuthError` variant, deliberately with no
        // wildcard arm: a wildcard here would silently classify any variant
        // `memorysafe-auth` adds later as 403, which is exactly wrong for a
        // server-side fault like `Rng` (see below) and is the kind of
        // decision-table drift this plan has been bitten by before. Losing
        // exhaustiveness-checking is the cost of ever adding `_ => ...`; a
        // failure to compile here is the intended signal to come back and
        // classify the new variant on purpose.
        match e {
            // The caller has not established who they are — 401.
            AuthError::Missing | AuthError::Malformed | AuthError::Unknown => {
                ApiError::Unauthenticated(message)
            }
            // A malformed component is the caller's typo, not a refusal.
            AuthError::Scope(_) => ApiError::Validation(message),
            // The caller is known, and the answer is still no — 403.
            AuthError::Disabled | AuthError::WrongTenant { .. } | AuthError::Reserved { .. } => {
                ApiError::Forbidden(message)
            }
            // The server's own entropy source failed. Not the caller's
            // fault and not a refusal of their credential — 500. Unreachable
            // from any route this task defines, but `key.rs`'s own doc says
            // `GeneratedKey` is what a future HTTP admin (key-minting) route
            // holds, and that route would call the same `generate` this
            // variant comes from.
            AuthError::Rng => ApiError::Internal(message),
        }
    }
}

impl From<CoreError> for ApiError {
    fn from(e: CoreError) -> Self {
        ApiError::Validation(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins Important 3 of the fix-round-1 review: the wildcard this match
    /// used to end in absorbed `AuthError::Rng` into `Forbidden` (403),
    /// telling a caller their credential was refused when the real fault was
    /// the server's own entropy source. `AuthError::Rng`'s own `Display`
    /// ("system randomness unavailable") names a server fault, not a
    /// caller-side refusal.
    #[test]
    fn a_broken_entropy_source_is_a_server_error_not_a_forbidden() {
        let api_err: ApiError = AuthError::Rng.into();
        assert_eq!(api_err.parts().0, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(api_err.parts().1, "internal");
    }

    /// The reserved-word refusal (`_admin`/`_purged`) is a 403, not a 400 —
    /// worth pinning directly, next to the exhaustive match that decides it,
    /// since it is easy to assume "the caller named an illegal component" is
    /// a validation error rather than a refusal.
    #[test]
    fn a_reserved_scope_component_is_forbidden_not_validation() {
        let api_err: ApiError = AuthError::Reserved {
            component: "_admin",
        }
        .into();
        assert_eq!(api_err.parts().0, StatusCode::FORBIDDEN);
        assert_eq!(api_err.parts().1, "forbidden");
    }

    /// New Minor 3, fix round 2: `AuthError::is_unauthenticated()`
    /// (`memorysafe-auth`) and the exhaustive `match e` above (this file) are
    /// two independent classifications of the same eight variants — one per
    /// crate — and nothing forces them to agree. The `match` above already
    /// fails to compile if `memorysafe-auth` adds a ninth, unclassified
    /// variant (Important 3, fix round 1); that guarantee says nothing about
    /// two variants BOTH sides already classify quietly drifting apart — the
    /// same copied-decision-table shape this plan has already been bitten by
    /// more than once (a reserved-word check that diverged between
    /// transports; a protection table that diverged three ways; §9's own
    /// mapping, fixed in this task's fix round 1).
    ///
    /// Chose this over routing the 401 arm through `is_unauthenticated()`
    /// directly: doing that would mean going back to an `if
    /// e.is_unauthenticated() { .. } else { match e { .. } }` shape, and the
    /// `else` arm's `match` would need a wildcard again (the compiler cannot
    /// know which variants `is_unauthenticated()` already excluded) — trading
    /// away Important 3's compile-time exhaustiveness to buy back agreement
    /// with `is_unauthenticated()`. This test keeps both: the match stays
    /// exhaustive and wildcard-free, and agreement is checked here, over one
    /// instance of every variant, instead of assumed.
    #[test]
    fn every_variant_agrees_with_is_unauthenticated() {
        let variants: Vec<AuthError> = vec![
            AuthError::Missing,
            AuthError::Malformed,
            AuthError::Unknown,
            AuthError::Disabled,
            AuthError::WrongTenant {
                authorized: "acme".into(),
                requested: "globex".into(),
            },
            AuthError::Reserved {
                component: "_admin",
            },
            AuthError::Scope(CoreError::Empty { field: "subject" }),
            AuthError::Rng,
        ];
        for variant in variants {
            let is_unauthenticated = variant.is_unauthenticated();
            let status = ApiError::from(variant).parts().0;
            assert_eq!(
                status == StatusCode::UNAUTHORIZED,
                is_unauthenticated,
                "status {status} disagrees with is_unauthenticated() == {is_unauthenticated}"
            );
        }
    }
}
