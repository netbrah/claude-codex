//! Gemini-specific error parsing and classification.
//!
//! Parses structured error responses from the Gemini API so the provider
//! can distinguish retryable server errors (429, 500, 503) from permanent
//! failures (403, 404) and surface actionable error messages.

use serde::Deserialize;

/// Top-level Gemini error response envelope.
#[derive(Debug, Deserialize)]
pub(crate) struct GeminiErrorResponse {
    pub error: GeminiError,
}

/// Structured error body returned by the Gemini API.
#[derive(Debug, Deserialize)]
pub(crate) struct GeminiError {
    pub code: u16,
    pub message: String,
    pub status: String,
}

impl GeminiError {
    /// Returns `true` for HTTP status codes that are transient and should
    /// be retried (rate-limit, internal server error, service unavailable).
    pub fn is_retryable(&self) -> bool {
        matches!(self.code, 429 | 500 | 503)
    }
}

/// Try to parse a Gemini error from a response body string.
///
/// Returns `None` if the body is not valid JSON or does not match the
/// expected `{"error": {...}}` envelope.
pub(crate) fn parse_gemini_error(body: &str) -> Option<GeminiErrorResponse> {
    serde_json::from_str(body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_429_error() {
        let body = r#"{"error":{"code":429,"message":"Resource has been exhausted","status":"RESOURCE_EXHAUSTED"}}"#;
        let err = parse_gemini_error(body).unwrap();
        assert_eq!(err.error.code, 429);
        assert_eq!(err.error.status, "RESOURCE_EXHAUSTED");
        assert!(err.error.is_retryable());
    }

    #[test]
    fn parse_503_error() {
        let body =
            r#"{"error":{"code":503,"message":"The model is overloaded","status":"UNAVAILABLE"}}"#;
        let err = parse_gemini_error(body).unwrap();
        assert_eq!(err.error.code, 503);
        assert!(err.error.is_retryable());
    }

    #[test]
    fn parse_500_error() {
        let body = r#"{"error":{"code":500,"message":"Internal error","status":"INTERNAL"}}"#;
        let err = parse_gemini_error(body).unwrap();
        assert!(err.error.is_retryable());
    }

    #[test]
    fn parse_403_error() {
        let body =
            r#"{"error":{"code":403,"message":"Permission denied","status":"PERMISSION_DENIED"}}"#;
        let err = parse_gemini_error(body).unwrap();
        assert_eq!(err.error.code, 403);
        assert!(!err.error.is_retryable());
    }

    #[test]
    fn parse_404_error() {
        let body = r#"{"error":{"code":404,"message":"Model not found","status":"NOT_FOUND"}}"#;
        let err = parse_gemini_error(body).unwrap();
        assert!(!err.error.is_retryable());
    }

    #[test]
    fn parse_400_error() {
        let body =
            r#"{"error":{"code":400,"message":"Invalid argument","status":"INVALID_ARGUMENT"}}"#;
        let err = parse_gemini_error(body).unwrap();
        assert!(!err.error.is_retryable());
    }

    #[test]
    fn parse_invalid_body_returns_none() {
        assert!(parse_gemini_error("not json").is_none());
        assert!(parse_gemini_error(r#"{"other":"field"}"#).is_none());
        assert!(parse_gemini_error("").is_none());
        assert!(parse_gemini_error("{}").is_none());
    }

    #[test]
    fn parse_preserves_message_text() {
        let body = r#"{"error":{"code":429,"message":"Quota exceeded for model gemini-2.5-flash. Please retry after 60s","status":"RESOURCE_EXHAUSTED"}}"#;
        let err = parse_gemini_error(body).unwrap();
        assert!(err.error.message.contains("Quota exceeded"));
        assert!(err.error.message.contains("retry after 60s"));
    }
}
