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
            // unavailable, so reaching here means the degradation itself broke.
            EngineError::Embedder(ref inner) => ApiError::Unavailable {
                message: inner.to_string(),
                retryable: true,
            },
            EngineError::PolicyRefused(m) => ApiError::Internal(m),
        }
    }
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        let message = e.to_string();
        if e.is_unauthenticated() {
            ApiError::Unauthenticated(message)
        } else {
            match e {
                // A malformed component is the caller's typo, not a refusal.
                AuthError::Scope(_) => ApiError::Validation(message),
                _ => ApiError::Forbidden(message),
            }
        }
    }
}

impl From<CoreError> for ApiError {
    fn from(e: CoreError) -> Self {
        ApiError::Validation(e.to_string())
    }
}
