use super::*;
use base64::Engine;
use pretty_assertions::assert_eq;

#[test]
fn map_api_error_maps_server_overloaded() {
    let err = map_api_error(ApiError::ServerOverloaded);
    assert!(matches!(err, CodexErr::ServerOverloaded));
}

#[test]
fn map_api_error_maps_server_overloaded_from_503_body() {
    let body = serde_json::json!({
        "error": {
            "code": "server_is_overloaded"
        }
    })
    .to_string();
    let err = map_api_error(ApiError::Transport(TransportError::Http {
        status: http::StatusCode::SERVICE_UNAVAILABLE,
        url: Some("http://example.com/v1/responses".to_string()),
        headers: None,
        body: Some(body),
    }));

    assert!(matches!(err, CodexErr::ServerOverloaded));
}

#[test]
fn map_api_error_maps_usage_limit_limit_name_header() {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACTIVE_LIMIT_HEADER,
        http::HeaderValue::from_static("codex_other"),
    );
    headers.insert(
        "x-codex-other-limit-name",
        http::HeaderValue::from_static("codex_other"),
    );
    let body = serde_json::json!({
        "error": {
            "type": "usage_limit_reached",
            "plan_type": "pro",
        }
    })
    .to_string();
    let err = map_api_error(ApiError::Transport(TransportError::Http {
        status: http::StatusCode::TOO_MANY_REQUESTS,
        url: Some("http://example.com/v1/responses".to_string()),
        headers: Some(headers),
        body: Some(body),
    }));

    let CodexErr::UsageLimitReached(usage_limit) = err else {
        panic!("expected CodexErr::UsageLimitReached, got {err:?}");
    };
    assert_eq!(
        usage_limit
            .rate_limits
            .as_ref()
            .and_then(|snapshot| snapshot.limit_name.as_deref()),
        Some("codex_other")
    );
}

#[test]
fn map_api_error_does_not_fallback_limit_name_to_limit_id() {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACTIVE_LIMIT_HEADER,
        http::HeaderValue::from_static("codex_other"),
    );
    let body = serde_json::json!({
        "error": {
            "type": "usage_limit_reached",
            "plan_type": "pro",
        }
    })
    .to_string();
    let err = map_api_error(ApiError::Transport(TransportError::Http {
        status: http::StatusCode::TOO_MANY_REQUESTS,
        url: Some("http://example.com/v1/responses".to_string()),
        headers: Some(headers),
        body: Some(body),
    }));

    let CodexErr::UsageLimitReached(usage_limit) = err else {
        panic!("expected CodexErr::UsageLimitReached, got {err:?}");
    };
    assert_eq!(
        usage_limit
            .rate_limits
            .as_ref()
            .and_then(|snapshot| snapshot.limit_name.as_deref()),
        None
    );
}

#[test]
fn map_api_error_extracts_identity_auth_details_from_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(REQUEST_ID_HEADER, http::HeaderValue::from_static("req-401"));
    headers.insert(CF_RAY_HEADER, http::HeaderValue::from_static("ray-401"));
    headers.insert(
        X_OPENAI_AUTHORIZATION_ERROR_HEADER,
        http::HeaderValue::from_static("missing_authorization_header"),
    );
    let x_error_json =
        base64::engine::general_purpose::STANDARD.encode(r#"{"error":{"code":"token_expired"}}"#);
    headers.insert(
        X_ERROR_JSON_HEADER,
        http::HeaderValue::from_str(&x_error_json).expect("valid x-error-json header"),
    );

    let err = map_api_error(ApiError::Transport(TransportError::Http {
        status: http::StatusCode::UNAUTHORIZED,
        url: Some("https://chatgpt.com/backend-api/codex/models".to_string()),
        headers: Some(headers),
        body: Some(r#"{"detail":"Unauthorized"}"#.to_string()),
    }));

    let CodexErr::UnexpectedStatus(err) = err else {
        panic!("expected CodexErr::UnexpectedStatus, got {err:?}");
    };
    assert_eq!(err.request_id.as_deref(), Some("req-401"));
    assert_eq!(err.cf_ray.as_deref(), Some("ray-401"));
    assert_eq!(
        err.identity_authorization_error.as_deref(),
        Some("missing_authorization_header")
    );
    assert_eq!(err.identity_error_code.as_deref(), Some("token_expired"));
}

#[test]
fn core_auth_provider_reports_when_auth_header_will_attach() {
    let auth = CoreAuthProvider {
        token: Some("access-token".to_string()),
        account_id: None,
    };

    assert!(auth.auth_header_attached());
    assert_eq!(auth.auth_header_name(), Some("authorization"));
}

// ---------------------------------------------------------------------------
// Rate-limit → retryable vs non-retryable tests
// ---------------------------------------------------------------------------

#[test]
fn rate_limit_transient_message_is_retryable() {
    let err = map_api_error(ApiError::RateLimit("Rate limited".to_string()));
    assert!(
        err.is_retryable(),
        "transient rate limit should be retryable, got {err:?}"
    );
    assert!(
        matches!(err, CodexErr::Stream(..)),
        "expected CodexErr::Stream, got {err:?}"
    );
}

#[test]
fn rate_limit_plan_limit_exceeded_is_not_retryable() {
    let err = map_api_error(ApiError::RateLimit(
        "plan limit exceeded, please upgrade".to_string(),
    ));
    assert!(
        !err.is_retryable(),
        "permanent plan limit should NOT be retryable, got {err:?}"
    );
    assert!(
        matches!(err, CodexErr::RetryLimit(..)),
        "expected CodexErr::RetryLimit, got {err:?}"
    );
}

#[test]
fn rate_limit_quota_exhausted_is_not_retryable() {
    let err = map_api_error(ApiError::RateLimit(
        "Your quota has been exhausted".to_string(),
    ));
    assert!(
        !err.is_retryable(),
        "permanent quota exhaustion should NOT be retryable, got {err:?}"
    );
    assert!(
        matches!(err, CodexErr::RetryLimit(..)),
        "expected CodexErr::RetryLimit, got {err:?}"
    );
}

#[test]
fn rate_limit_budget_is_not_retryable() {
    let err = map_api_error(ApiError::RateLimit("budget exceeded".to_string()));
    assert!(
        !err.is_retryable(),
        "budget exhaustion should NOT be retryable, got {err:?}"
    );
}

#[test]
fn rate_limit_usage_limit_is_not_retryable() {
    let err = map_api_error(ApiError::RateLimit(
        "usage_limit reached for this account".to_string(),
    ));
    assert!(!err.is_retryable());
    assert!(matches!(err, CodexErr::RetryLimit(..)));
}

#[test]
fn rate_limit_with_retry_after_parses_delay() {
    let msg =
        "Rate limit reached for gpt-5.1 on tokens per min (TPM). Please try again in 11.054s."
            .to_string();
    let err = map_api_error(ApiError::RateLimit(msg));
    assert!(err.is_retryable(), "should be retryable, got {err:?}");
    match &err {
        CodexErr::Stream(_, Some(delay)) => {
            // 11.054s ± small float tolerance
            assert!(
                delay.as_secs_f64() > 11.0 && delay.as_secs_f64() < 11.1,
                "expected ~11.054s delay, got {delay:?}"
            );
        }
        other => panic!("expected CodexErr::Stream with delay, got {other:?}"),
    }
}

#[test]
fn rate_limit_without_retry_after_has_no_delay() {
    let err = map_api_error(ApiError::RateLimit("Rate limited".to_string()));
    match &err {
        CodexErr::Stream(_, delay) => {
            assert!(delay.is_none(), "expected no delay, got {delay:?}");
        }
        other => panic!("expected CodexErr::Stream, got {other:?}"),
    }
}

#[test]
fn rate_limit_case_insensitive_detection() {
    // "Plan" with uppercase should still be caught
    let err = map_api_error(ApiError::RateLimit("Plan limit exceeded".to_string()));
    assert!(!err.is_retryable());
    assert!(matches!(err, CodexErr::RetryLimit(..)));
}

#[test]
fn parse_retry_after_from_message_parses_seconds() {
    let delay = parse_retry_after_from_message("Please try again in 5.2s.");
    assert!(delay.is_some());
    let d = delay.unwrap();
    assert!(d.as_secs_f64() > 5.1 && d.as_secs_f64() < 5.3);
}

#[test]
fn parse_retry_after_from_message_parses_milliseconds() {
    let delay = parse_retry_after_from_message("try again in 500ms");
    assert_eq!(delay, Some(std::time::Duration::from_millis(500)));
}

#[test]
fn parse_retry_after_from_message_returns_none_for_unrelated_text() {
    let delay = parse_retry_after_from_message("something unrelated");
    assert!(delay.is_none());
}
