//! End-to-end adapter test: Prompt-shaped input → `stream_inner` →
//! claude-codex `ResponseEvent` sequence. No real network; wiremock fakes
//! `/chat/completions`.
//!
//! The public `stream()` is skipped here because it touches the TTY probe,
//! the TOS banner (printed to stderr — pollutes test output), and
//! `CopilotAuth::init` (which does disk + device flow). `stream_inner` is
//! the test seam documented in `src/adapter.rs`.

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
use wiremock::matchers::header;
use wiremock::matchers::header_exists;
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

fn jwt_body(exp_offset_secs: i64, api_base: &str) -> serde_json::Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    serde_json::json!({
        "token": "jwt_fake",
        "expires_at": now + exp_offset_secs,
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
const STOP_CHUNK: &str = r#"{"choices":[{"finish_reason":"stop","delta":{}}]}"#;

fn tool_start(idx: usize, id: &str, name: &str) -> String {
    format!(
        r#"{{"choices":[{{"delta":{{"tool_calls":[{{"index":{idx},"id":"{id}","type":"function","function":{{"name":"{name}","arguments":""}}}}]}}}}]}}"#
    )
}
fn tool_args(idx: usize, args: &str) -> String {
    let esc = args.replace('"', "\\\"");
    format!(
        r#"{{"choices":[{{"delta":{{"tool_calls":[{{"index":{idx},"function":{{"arguments":"{esc}"}}}}]}}}}]}}"#
    )
}
const TOOL_FINISH: &str = r#"{"choices":[{"finish_reason":"tool_calls","delta":{}}]}"#;

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

// ============================================================================
// happy path: user -> content deltas -> stop
// ============================================================================

#[tokio::test]
async fn stream_inner_content_then_completed_end_turn() {
    let server = MockServer::start().await;

    // JWT exchange — uses the `ghu_*` token we pass in.
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .and(header("authorization", "token ghu_adapter_test"))
        .and(header("editor-version", "vscode/1.95.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwt_body(1_500, &server.uri())))
        .expect(1)
        .mount(&server)
        .await;

    // Chat completions — two content deltas then stop.
    //
    // Assertions cover invariants 11-13 (new in the wire layer):
    //   • x-initiator: agent (NOT the upstream default `user`)
    //   • openai-intent: conversation-agent (NOT `conversation-panel`)
    //   • x-interaction-id: <session uuid> (NOT absent)
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer jwt_fake"))
        .and(header("copilot-integration-id", "vscode-chat"))
        .and(header("x-initiator", "agent"))
        .and(header("openai-intent", "conversation-agent"))
        .and(header("x-github-api-version", "2025-05-01"))
        .and(header_exists("x-interaction-id"))
        .and(header_exists("x-request-id"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&[
                    &content_chunk("Hello"),
                    &content_chunk(", world"),
                    STOP_CHUNK,
                ]))
                .insert_header("content-type", "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let config = test_config_from(&server);
    let http_inner = reqwest::Client::new();
    let mut auth = CopilotAuth::from_token(http_inner.clone(), "ghu_adapter_test".into(), config);

    let input = vec![user_msg("say hi")];
    let mut stream = stream_inner(&http_inner, &mut auth, &input, "gpt-4o", &[])
        .await
        .expect("stream_inner");

    let mut texts: Vec<String> = Vec::new();
    let mut completed_stop: Option<String> = None;
    while let Some(ev) = stream.next().await {
        match ev.expect("no adapter error") {
            ResponseEvent::OutputTextDelta(s) => texts.push(s),
            ResponseEvent::Completed { stop_reason, .. } => completed_stop = stop_reason,
            _ => {}
        }
    }

    assert_eq!(texts, vec!["Hello".to_string(), ", world".to_string()]);
    assert_eq!(completed_stop.as_deref(), Some("end_turn"));
}

// ============================================================================
// tool call: content + tool call + finish_reason=tool_calls -> tool_use
// ============================================================================

#[tokio::test]
async fn stream_inner_tool_call_emits_added_done_and_tool_use_stop() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwt_body(1_500, &server.uri())))
        .expect(1)
        .mount(&server)
        .await;

    // A tool call split across start + args + finish frames.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&[
                    &content_chunk("I will call a tool."),
                    &tool_start(0, "call_abc", "shell"),
                    &tool_args(0, r#"{"cmd":"ls"}"#),
                    TOOL_FINISH,
                ]))
                .insert_header("content-type", "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let config = test_config_from(&server);
    let http_inner = reqwest::Client::new();
    let mut auth = CopilotAuth::from_token(http_inner.clone(), "ghu_tc".into(), config);

    let mut stream = stream_inner(&http_inner, &mut auth, &[user_msg("do it")], "gpt-4o", &[])
        .await
        .expect("stream_inner");

    let mut saw_added = false;
    let mut saw_done = false;
    let mut completed_stop: Option<String> = None;
    let mut got_text = false;
    while let Some(ev) = stream.next().await {
        match ev.expect("no adapter error") {
            ResponseEvent::OutputTextDelta(_) => got_text = true,
            ResponseEvent::OutputItemAdded(_) => saw_added = true,
            ResponseEvent::OutputItemDone(_) => saw_done = true,
            ResponseEvent::Completed { stop_reason, .. } => completed_stop = stop_reason,
            _ => {}
        }
    }

    assert!(got_text, "expected at least one OutputTextDelta");
    assert!(saw_added, "expected OutputItemAdded for the tool call");
    assert!(saw_done, "expected OutputItemDone for the tool call");
    assert_eq!(completed_stop.as_deref(), Some("tool_use"));
}
