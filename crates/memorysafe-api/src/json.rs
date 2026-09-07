//! `Json<T>`, but answering every deserialisation failure the way this API
//! answers every other failure.
//!
//! axum's own [`axum::Json`] renders every one of its `JsonRejection` kinds as
//! plain text, not the `Problem { error, message, retryable }` JSON envelope
//! [`crate::error::ApiError`] produces for the rest of this surface — and,
//! worse than [`crate::query::ValidatedQuery`]'s sibling defect, two of its
//! four kinds carry status codes §9 has no row for at all:
//! `MissingJsonContentType` is 415, and `JsonDataError` (syntactically valid
//! JSON that doesn't match `T` — a missing required field, for instance) is
//! 422. §9's error contract has exactly one status for "the caller's request
//! was malformed": 400/`Validation`. A missing `namespace` in a JSON body
//! must answer the same way a missing `namespace` in a query string does
//! (`ValidatedQuery`'s own doc states the identical rule for that sibling
//! extractor) — so every JSON body extraction on this API goes through
//! `ValidatedJson` instead of bare `axum::Json`.

use crate::error::ApiError;
use axum::Json;
use axum::extract::{FromRequest, Request};
use serde::de::DeserializeOwned;

/// Deserialises the request body as JSON into `T`, rejecting with
/// [`ApiError::Validation`] (carrying axum's own diagnostic message) rather
/// than axum's built-in `JsonRejection`, which renders as plain text under a
/// status code that varies by failure kind (400/415/422) — §9 has one status
/// for a malformed request, and this collapses all four `JsonRejection`
/// kinds onto it uniformly.
#[derive(Debug)]
pub struct ValidatedJson<T>(pub T);

impl<T, S> FromRequest<S> for ValidatedJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| ValidatedJson(value))
            .map_err(|rejection| ApiError::Validation(rejection.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode};
    use axum::response::IntoResponse;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct ScopeBody {
        #[allow(dead_code)]
        subject: String,
        #[allow(dead_code)]
        namespace: String,
    }

    async fn problem_body(response: axum::response::Response) -> serde_json::Value {
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    /// A syntactically valid JSON body missing a required field would
    /// otherwise be axum's `JsonDataError` — 422, plain text.
    #[tokio::test]
    async fn a_missing_required_field_is_a_json_validation_problem_not_a_422() {
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/v1/memories")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"subject":"user-42"}"#))
            .unwrap();

        let err = ValidatedJson::<ScopeBody>::from_request(req, &())
            .await
            .expect_err("a missing 'namespace' field must be rejected");

        let body = problem_body(err.into_response()).await;
        assert_eq!(body["error"], "validation");
        assert!(body["message"].is_string());
    }

    /// A missing `Content-Type` header would otherwise be axum's
    /// `MissingJsonContentType` — 415, plain text.
    #[tokio::test]
    async fn a_missing_content_type_is_a_json_validation_problem_not_a_415() {
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/v1/memories")
            .body(Body::from(r#"{"subject":"user-42","namespace":"agent"}"#))
            .unwrap();

        let err = ValidatedJson::<ScopeBody>::from_request(req, &())
            .await
            .expect_err("a missing content-type must be rejected");

        let body = problem_body(err.into_response()).await;
        assert_eq!(body["error"], "validation");
    }

    /// Syntactically invalid JSON would otherwise be axum's
    /// `JsonSyntaxError` — already 400, but still plain text rather than the
    /// `Problem` envelope, so it belongs in this suite for completeness.
    #[tokio::test]
    async fn malformed_json_syntax_is_a_json_validation_problem() {
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/v1/memories")
            .header("content-type", "application/json")
            .body(Body::from("{not valid json"))
            .unwrap();

        let err = ValidatedJson::<ScopeBody>::from_request(req, &())
            .await
            .expect_err("malformed JSON syntax must be rejected");

        let body = problem_body(err.into_response()).await;
        assert_eq!(body["error"], "validation");
    }
}
