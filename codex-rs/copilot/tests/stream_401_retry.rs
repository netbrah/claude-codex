//! Invariant 10 under test: the wire layer owns the 401-retry path.
//! The adapter must exhibit the same behavior end-to-end — a single 401 from
//! the chat endpoint must trigger a JWT refresh (via
//! `CopilotAuth::cache_mut().force_refresh()`) and a second chat call, and
//! the eventual events must flow through the adapter's Mapper as normal.
//!
//! The wire layer (`codex_copilot_adapter::wire::stream_chat`) mirrors
//! upstream's `CopilotHttpClient::chat_stream_with_auth` line-for-line for
//! this behavior — see 02-design.md §8 invariant 10.
//!
//! See upstream `codex-agent/codex-copilot/tests/e2e_wiremock.rs` for the
//! canonical 401-retry test that exercises upstream's own implementation.

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

fn jwt_body(token: &str, exp_offset_secs: i64, api_base: &str) -> serde_json::Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    serde_json::json!({
        "token": token,
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

#[tokio::test]
async fn stream_inner_retries_once_on_401_and_succeeds() {
    let server = MockServer::start().await;

    // First JWT exchange hands out `jwt_stale`. Second hands out `jwt_fresh`.
    // wiremock matches in the order mocks are mounted (most-recent-first for
    // overlapping matchers), so we rely on `up_to_n_times` to sequence.
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwt_body(
            "jwt_stale",
            1_500,
            &server.uri(),
        )))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwt_body(
            "jwt_fresh",
            1_500,
            &server.uri(),
        )))
        .expect(1)
        .mount(&server)
        .await;

    // First chat call with stale JWT → 401.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer jwt_stale"))
        .respond_with(ResponseTemplate::new(401).set_body_string(""))
        .expect(1)
        .mount(&server)
        .await;

    // Retry with fresh JWT → 200 with one content delta + stop.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer jwt_fresh"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&[&content_chunk("recovered"), STOP_CHUNK]))
                .insert_header("content-type", "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let config = test_config_from(&server);
    let http_inner = reqwest::Client::new();
    let mut auth = CopilotAuth::from_token(http_inner.clone(), "ghu_401".into(), config);

    let mut stream = stream_inner(
        &http_inner,
        &mut auth,
        &[user_msg("try again")],
        "gpt-4o",
        &[],
    )
    .await
    .expect("stream_inner should recover from 401 via wire::stream_chat");

    let mut got_text = None::<String>;
    let mut completed_stop: Option<String> = None;
    while let Some(ev) = stream.next().await {
        match ev.expect("no adapter error after retry") {
            ResponseEvent::OutputTextDelta(s) => got_text = Some(s),
            ResponseEvent::Completed { stop_reason, .. } => completed_stop = stop_reason,
            _ => {}
        }
    }

    assert_eq!(got_text.as_deref(), Some("recovered"));
    assert_eq!(completed_stop.as_deref(), Some("end_turn"));
}
