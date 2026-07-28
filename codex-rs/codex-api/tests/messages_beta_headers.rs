#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Tests for the `anthropic-beta` header on `/v1/messages` requests.
//!
//! Covers:
//! - `prompt-caching-2024-07-31` is always sent (S-X1 — gives us
//!   cache_read_input_tokens / cache_creation_input_tokens telemetry and
//!   ensures `cache_control` breakpoints are honored on every proxy path)
//! - `interleaved-thinking-2025-05-14` is appended when `thinking` is set
//! - The two beta tokens coexist when both apply

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use codex_api::AuthProvider;
use codex_api::MessagesApiRequest;
use codex_api::MessagesClient;
use codex_api::Provider;
use codex_api::RetryConfig;
use codex_client::HttpTransport;
use codex_client::Request;
use codex_client::Response;
use codex_client::StreamResponse;
use codex_client::TransportError;
use http::HeaderMap;
use http::StatusCode;
use serde_json::json;

#[derive(Clone)]
struct HeaderCapturingTransport {
    captured_headers: Arc<Mutex<Option<HeaderMap>>>,
    response_body: String,
}

impl HeaderCapturingTransport {
    fn new(response_body: String) -> Self {
        Self {
            captured_headers: Arc::new(Mutex::new(None)),
            response_body,
        }
    }

    fn captured(&self) -> HeaderMap {
        self.captured_headers
            .lock()
            .unwrap()
            .clone()
            .expect("no request was captured")
    }
}

#[async_trait]
impl HttpTransport for HeaderCapturingTransport {
    async fn execute(&self, _req: Request) -> Result<Response, TransportError> {
        Err(TransportError::Build("execute should not run".into()))
    }

    async fn stream(&self, req: Request) -> Result<StreamResponse, TransportError> {
        *self.captured_headers.lock().unwrap() = Some(req.headers);
        let stream = futures::stream::iter(vec![Ok::<Bytes, TransportError>(Bytes::from(
            self.response_body.clone(),
        ))]);
        Ok(StreamResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            bytes: Box::pin(stream),
        })
    }
}

#[derive(Clone, Default)]
struct NoAuth;

#[async_trait::async_trait]
impl AuthProvider for NoAuth {
    fn add_auth_headers(&self, _headers: &mut http::HeaderMap) {}
}

fn test_provider() -> Provider {
    Provider {
        name: "test-anthropic".into(),
        base_url: "https://example.com/v1".into(),
        query_params: None,
        headers: HeaderMap::new(),
        retry: RetryConfig {
            max_attempts: 1,
            base_delay: Duration::from_millis(1),
            retry_429: false,
            retry_5xx: false,
            retry_transport: true,
        },
        stream_idle_timeout: Duration::from_millis(500),
    }
}

fn minimal_sse_response() -> String {
    [
        "event: message_start",
        r#"data: {"type":"message_start","message":{"id":"msg_test","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#,
        "",
        "event: content_block_start",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        "",
        "event: content_block_delta",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        "",
        "event: content_block_stop",
        r#"data: {"type":"content_block_stop","index":0}"#,
        "",
        "event: message_delta",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":1}}"#,
        "",
        "event: message_stop",
        r#"data: {"type":"message_stop"}"#,
        "",
    ]
    .join("\n")
}

fn base_request(thinking: Option<serde_json::Value>) -> MessagesApiRequest {
    MessagesApiRequest {
        model: "claude-sonnet-4.6".into(),
        messages: vec![json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})],
        max_tokens: 64,
        stream: true,
        system: None,
        tools: None,
        tool_choice: None,
        thinking,
        output_config: None,
        temperature: None,
        top_p: None,
        top_k: None,
        stop_sequences: None,
        metadata: None,
    }
}

async fn send_and_capture(thinking: Option<serde_json::Value>) -> HeaderMap {
    let transport = HeaderCapturingTransport::new(minimal_sse_response());
    let client = MessagesClient::new(
        transport.clone(),
        test_provider(),
        std::sync::Arc::new(NoAuth),
    );
    let stream = client
        .stream_request(base_request(thinking), HeaderMap::new())
        .await
        .expect("request to succeed");
    // Drain the stream so the transport's stream() actually executes.
    use futures::StreamExt;
    let mut s = stream;
    while let Some(_event) = s.next().await {}
    transport.captured()
}

#[tokio::test]
async fn prompt_caching_beta_header_always_present() {
    let headers = send_and_capture(None).await;
    let beta = headers
        .get("anthropic-beta")
        .expect("anthropic-beta header must be sent")
        .to_str()
        .unwrap();
    assert!(
        beta.contains("prompt-caching-2024-07-31"),
        "anthropic-beta must include prompt-caching-2024-07-31 (got: {beta})"
    );
}

#[tokio::test]
async fn prompt_caching_beta_present_without_thinking() {
    let headers = send_and_capture(None).await;
    let beta = headers.get("anthropic-beta").unwrap().to_str().unwrap();
    assert!(beta.contains("prompt-caching-2024-07-31"));
    assert!(
        !beta.contains("interleaved-thinking"),
        "thinking beta must NOT appear when thinking is None (got: {beta})"
    );
}

#[tokio::test]
async fn both_beta_tokens_present_when_thinking_enabled() {
    let headers = send_and_capture(Some(json!({"type": "adaptive"}))).await;
    let beta = headers.get("anthropic-beta").unwrap().to_str().unwrap();
    assert!(
        beta.contains("prompt-caching-2024-07-31"),
        "prompt-caching beta must coexist with thinking (got: {beta})"
    );
    assert!(
        beta.contains("interleaved-thinking-2025-05-14"),
        "interleaved-thinking beta must be present when thinking is set (got: {beta})"
    );
}
