//! End-to-end tests for F1 (extended finish_reason fidelity) and NB1
//! (token_usage via `stream_options.include_usage=true`).
//!
//! F1 — Upstream's frozen `SseReassembler` at
//! `netbrah/codex-agent@26ff8f1d`, `codex-copilot/src/sse.rs:88` only surfaces
//! `Done(reason)` for `reason ∈ {tool_calls, stop, length}`. Any other value
//! (`content_filter`, `function_call`, `error`, future CAPI extensions) is
//! dropped — and for `content_filter` specifically, the reassembler returns
//! `[]` for that chunk, so the turn loop would hang with no terminal event.
//!
//! The adapter's wire layer runs a `SideChannel` pass over the raw SSE bytes
//! that captures these dropped reasons and the `usage` object, emits them as
//! `WireEvent::FinishReasonExtended(..)` / `WireEvent::Usage(..)`, and the
//! Mapper's `finalize()` projects them onto `ResponseEvent::Completed`.
//!
//! These tests exercise the path end-to-end against wiremock with hand-crafted
//! SSE bodies.

use codex_api::ResponseEvent;
use codex_copilot::CopilotAuth;
use codex_copilot::CopilotConfig;
use codex_copilot_adapter::stream_inner;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use futures::StreamExt;
use std::time::Duration;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn user_msg(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".into(),
        content: vec![ContentItem::InputText { text: text.into() }],
        phase: None,
    }
}

fn jwt_body(api_base: &str) -> serde_json::Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    serde_json::json!({
        "token": "jwt_fake",
        "expires_at": now + 1500,
        "refresh_in": 1500,
        "endpoints": {
            "api": api_base,
            "telemetry": null,
            "proxy": null,
        }
    })
}

fn sse_body(chunks: &[&str]) -> String {
    let mut out = String::new();
    for c in chunks {
        out.push_str("data: ");
        out.push_str(c);
        out.push('\n');
    }
    out.push_str("data: [DONE]\n");
    out
}

fn content_chunk(text: &str) -> String {
    format!(r#"{{"choices":[{{"delta":{{"content":"{text}"}}}}]}}"#)
}

fn test_config_from(server: &MockServer) -> CopilotConfig {
    CopilotConfig {
        github_api_base: server.uri(),
        github_oauth_base: server.uri(),
        copilot_api_base: server.uri(),
        force_allow_headless: true,
        device_poll_interval: Some(Duration::from_millis(10)),
        device_poll_max: Some(Duration::from_secs(30)),
    }
}

async fn stub_jwt(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwt_body(&server.uri())))
        .mount(server)
        .await;
}

async fn drive(server: &MockServer) -> Vec<ResponseEvent> {
    let config = test_config_from(server);
    let http_inner = reqwest::Client::new();
    let mut auth = CopilotAuth::from_token(http_inner.clone(), "ghu_ff".into(), config);
    let mut stream = stream_inner(&http_inner, &mut auth, &[user_msg("go")], "gpt-4o", &[])
        .await
        .expect("stream_inner");
    let mut out = Vec::new();
    while let Some(ev) = stream.next().await {
        out.push(ev.expect("no adapter error"));
    }
    out
}

// ============================================================================
// F1: content_filter finish_reason — upstream drops it; we must synthesize
// ============================================================================

#[tokio::test]
async fn content_filter_finish_reason_surfaces_as_completed() {
    let server = MockServer::start().await;
    stub_jwt(&server).await;

    // Upstream `SseReassembler` returns `[]` for this chunk because
    // `content_filter` is not in its allow-list (see sse.rs:88 at 26ff8f1d).
    // Pre-F1, the stream would end with NO terminal event and the turn loop
    // would hang.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&[
                    &content_chunk("I cannot"),
                    r#"{"choices":[{"finish_reason":"content_filter","delta":{}}]}"#,
                ]))
                .insert_header("content-type", "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let events = drive(&server).await;
    let completed = events
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Completed { stop_reason, .. } => stop_reason.as_deref(),
            _ => None,
        })
        .expect("adapter MUST emit Completed even when upstream drops the terminal frame");
    assert_eq!(completed, "content_filter");
}

// ============================================================================
// F1: function_call finish_reason — legacy alias for tool_calls
// ============================================================================

#[tokio::test]
async fn function_call_finish_reason_maps_to_tool_use() {
    let server = MockServer::start().await;
    stub_jwt(&server).await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&[
                    r#"{"choices":[{"finish_reason":"function_call","delta":{}}]}"#,
                ]))
                .insert_header("content-type", "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let events = drive(&server).await;
    let completed = events
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Completed { stop_reason, .. } => stop_reason.as_deref(),
            _ => None,
        })
        .expect("Completed");
    assert_eq!(completed, "tool_use");
}

// ============================================================================
// F1: error finish_reason — pass through for downstream handling
// ============================================================================

#[tokio::test]
async fn error_finish_reason_surfaces_verbatim() {
    let server = MockServer::start().await;
    stub_jwt(&server).await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&[
                    &content_chunk("partial"),
                    r#"{"choices":[{"finish_reason":"error","delta":{}}]}"#,
                ]))
                .insert_header("content-type", "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let events = drive(&server).await;
    let completed = events
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Completed { stop_reason, .. } => stop_reason.as_deref(),
            _ => None,
        })
        .expect("Completed");
    assert_eq!(completed, "error");
}

// ============================================================================
// NB1: build_body emits stream_options.include_usage=true
// ============================================================================

#[tokio::test]
async fn request_body_includes_stream_options_include_usage() {
    let server = MockServer::start().await;
    stub_jwt(&server).await;

    // `body_partial_json` asserts a subset match — any other keys in the body
    // are fine. We only care that stream_options.include_usage=true is there.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(serde_json::json!({
            "stream": true,
            "stream_options": { "include_usage": true }
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&[
                    r#"{"choices":[{"finish_reason":"stop","delta":{}}]}"#,
                ]))
                .insert_header("content-type", "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let events = drive(&server).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ResponseEvent::Completed { .. }))
    );
}

// ============================================================================
// NB1: usage object parsed and projected into TokenUsage
// ============================================================================

#[tokio::test]
async fn usage_chunk_populates_token_usage() {
    let server = MockServer::start().await;
    stub_jwt(&server).await;

    // Final chunk carries `usage` (server honors stream_options.include_usage).
    // Shape mirrors OpenAI's APIUsage — prompt/completion totals plus cached
    // and reasoning detail breakdowns.
    let usage_chunk = r#"{"choices":[],"usage":{"prompt_tokens":120,"completion_tokens":45,"total_tokens":165,"prompt_tokens_details":{"cached_tokens":30},"completion_tokens_details":{"reasoning_tokens":15}}}"#;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&[
                    &content_chunk("hi"),
                    r#"{"choices":[{"finish_reason":"stop","delta":{}}]}"#,
                    usage_chunk,
                ]))
                .insert_header("content-type", "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let events = drive(&server).await;
    let tu = events
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Completed { token_usage, .. } => token_usage.clone(),
            _ => None,
        })
        .expect("token_usage should be populated from the usage chunk");
    assert_eq!(tu.input_tokens, 120);
    assert_eq!(tu.output_tokens, 45);
    assert_eq!(tu.total_tokens, 165);
    assert_eq!(tu.cached_input_tokens, 30);
    assert_eq!(tu.reasoning_output_tokens, 15);
    assert_eq!(tu.cache_creation_input_tokens, 0);
}

// ============================================================================
// NB1: usage absent is fine — token_usage stays None
// ============================================================================

#[tokio::test]
async fn missing_usage_leaves_token_usage_none() {
    let server = MockServer::start().await;
    stub_jwt(&server).await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&[
                    &content_chunk("hi"),
                    r#"{"choices":[{"finish_reason":"stop","delta":{}}]}"#,
                ]))
                .insert_header("content-type", "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let events = drive(&server).await;
    let tu_opt = events.iter().find_map(|e| match e {
        ResponseEvent::Completed { token_usage, .. } => Some(token_usage.clone()),
        _ => None,
    });
    assert!(tu_opt.is_some(), "Completed MUST fire");
    assert!(
        tu_opt.unwrap().is_none(),
        "token_usage should be None when server omits the usage chunk"
    );
}
