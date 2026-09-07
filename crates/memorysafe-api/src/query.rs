//! `Query<T>`, but answering every deserialisation failure the way this API
//! answers every other failure.
//!
//! axum's own [`axum::extract::Query`] renders its `QueryRejection` as plain
//! text, not the `Problem { error, message, retryable }` JSON envelope
//! [`crate::error::ApiError`] produces for the rest of this surface. A caller
//! that parses `Problem` off of every 4xx this API returns would choke on
//! the one 400 that came from a bare `Query<T>` extraction failure — an HTTP
//! surface that answers one class of 400 in plain text and every other class
//! in JSON is a real defect, not a cosmetic one, so query-string extraction
//! goes through `ValidatedQuery` instead everywhere on this API.

use crate::error::ApiError;
use axum::extract::{FromRequestParts, Query};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

/// Deserialises the request's query string into `T`, rejecting with
/// [`ApiError::Validation`] (carrying axum's own diagnostic message) rather
/// than axum's built-in plain-text `QueryRejection`.
///
/// Generic over the router's state, like `axum::extract::Query` itself —
/// extracting a query string needs nothing from `AppState`.
#[derive(Debug)]
pub struct ValidatedQuery<T>(pub T);

impl<T, S> FromRequestParts<S> for ValidatedQuery<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        Query::<T>::try_from_uri(&parts.uri)
            .map(|Query(value)| ValidatedQuery(value))
            .map_err(|rejection| ApiError::Validation(rejection.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::FromRequestParts;
    use axum::http::{Request, StatusCode};
    use axum::response::IntoResponse;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct ScopeQuery {
        #[allow(dead_code)]
        subject: String,
        #[allow(dead_code)]
        namespace: String,
    }

    /// Pins the amendment to Task 7: with `namespace` missing, axum's own
    /// `Query<T>` would fail before any handler body runs and render plain
    /// text, not the JSON `Problem` body every other 400 on this API
    /// returns. `ValidatedQuery` must produce that same JSON shape instead —
    /// this is exactly the failure Task 8's
    /// `a_missing_scope_parameter_is_400_not_a_default_scope` depends on.
    #[tokio::test]
    async fn a_missing_required_field_is_a_json_validation_problem() {
        let (mut parts, ()) = Request::builder()
            .uri("/v1/memories?subject=user-42")
            .body(())
            .unwrap()
            .into_parts();

        let err = ValidatedQuery::<ScopeQuery>::from_request_parts(&mut parts, &())
            .await
            .expect_err("a missing 'namespace' field must be rejected");

        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
        assert_eq!(body["error"], "validation");
        assert!(body["message"].is_string());
    }
}
