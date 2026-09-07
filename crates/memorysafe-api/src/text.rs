//! `String`, but answering every body-buffering failure the way this API
//! answers every other failure.
//!
//! Fix round 1, Important 1. `POST /v1/import` takes a bare `String`
//! extractor (ndjson, not JSON — [`crate::json::ValidatedJson`] does not
//! apply). axum's own [`String`] `FromRequest` impl renders every one of its
//! `StringRejection` kinds as plain text, not the `Problem { error, message,
//! retryable }` JSON envelope [`crate::error::ApiError`] produces for the
//! rest of this surface — and one of its two kinds carries a status code §9
//! has no row for at all: a body over the extractor's length limit
//! (`StringRejection::FailedToBufferBody(FailedToBufferBody::LengthLimitError)`)
//! is **413**, plain text. That limit is the brief's own deliberate 2 MB
//! default (`import`'s own doc: "An archive larger than that is a CLI job,
//! not an HTTP request"), so this is not an edge case nobody hits — it is
//! the one failure mode the route was explicitly designed to produce. The
//! other kind, invalid UTF-8
//! (`StringRejection::InvalidUtf8`), is already 400 but still plain text.
//! §9's error contract has exactly one status for "the caller's request was
//! malformed": 400/`Validation` — the same rule [`crate::json::ValidatedJson`]'s
//! own doc states for `JsonRejection`'s 415/422 — so this collapses both
//! `StringRejection` kinds onto it uniformly, the same way `ValidatedJson`
//! collapses `JsonRejection`'s four.

use crate::error::ApiError;
use axum::extract::{FromRequest, Request};

/// Reads the request body as UTF-8 text, rejecting with
/// [`ApiError::Validation`] (carrying axum's own diagnostic message) rather
/// than axum's built-in `StringRejection`, which renders as plain text under
/// a status code that varies by failure kind (400/413).
#[derive(Debug)]
pub struct ValidatedText(pub String);

impl<S> FromRequest<S> for ValidatedText
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        String::from_request(req, state)
            .await
            .map(ValidatedText)
            .map_err(|rejection| ApiError::Validation(rejection.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode};
    use axum::response::IntoResponse;

    async fn problem_body(response: axum::response::Response) -> serde_json::Value {
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    /// A body over axum's default 2 MB extractor limit would otherwise be
    /// `StringRejection::FailedToBufferBody(FailedToBufferBody::LengthLimitError)`
    /// — 413, plain text. This is `POST /v1/import`'s own deliberately
    /// undisturbed default (see that route's doc), so this is the failure
    /// mode most likely to actually happen on this route, not a
    /// theoretical one.
    #[tokio::test]
    async fn an_oversized_body_is_a_validation_problem_not_a_plain_text_413() {
        // One byte over axum's hardcoded 2 MB (`2_097_152`) default limit —
        // applied by `RequestExt::with_limited_body` regardless of whether
        // this request passes through a full `Router`, since nothing here
        // sets a `DefaultBodyLimit` extension to override it.
        let oversized = vec![b'x'; 2_097_152 + 1];
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/v1/import")
            .header("content-type", "application/x-ndjson")
            .body(Body::from(oversized))
            .unwrap();

        let err = ValidatedText::from_request(req, &())
            .await
            .expect_err("a body over the default limit must be rejected");

        let body = problem_body(err.into_response()).await;
        assert_eq!(body["error"], "validation");
        assert!(body["message"].is_string());
    }

    /// Invalid UTF-8 would otherwise be `StringRejection::InvalidUtf8` —
    /// already 400, but still plain text rather than the `Problem` envelope.
    #[tokio::test]
    async fn invalid_utf8_is_a_validation_problem_not_plain_text() {
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/v1/import")
            .header("content-type", "application/x-ndjson")
            .body(Body::from(vec![0xff, 0xfe, 0xfd]))
            .unwrap();

        let err = ValidatedText::from_request(req, &())
            .await
            .expect_err("invalid UTF-8 must be rejected");

        let body = problem_body(err.into_response()).await;
        assert_eq!(body["error"], "validation");
    }
}
