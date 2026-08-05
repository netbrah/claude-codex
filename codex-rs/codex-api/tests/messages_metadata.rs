#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Tests for `metadata.user_id` propagation in Anthropic Messages API requests.
//!
//! Covers:
//! - Serialization of `MessagesApiMetadata` to the expected JSON shape
//! - `MessagesApiRequest` with/without metadata
//! - Request body capture to verify metadata flows through the transport layer
//! - Edge cases: empty user_id, unicode, long values

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use codex_api::AuthProvider;
use codex_api::MessagesApiMetadata;
use codex_api::MessagesApiRequest;
use codex_api::MessagesClient;
use codex_api::Provider;
use codex_client::HttpTransport;
use codex_client::Request;
use codex_client::Response;
use codex_client::StreamResponse;
use codex_client::TransportError;
use http::HeaderMap;
use http::StatusCode;
use pretty_assertions::assert_eq;
use serde_json::json;

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

/// Transport that captures the serialized request body for assertion.
#[derive(Clone)]
struct CapturingTransport {
    captured_body: Arc<Mutex<Option<serde_json::Value>>>,
    response_body: String,
}

impl CapturingTransport {
    fn new(response_body: String) -> Self {
        Self {
            captured_body: Arc::new(Mutex::new(None)),
            response_body,
        }
    }

    fn captured_body(&self) -> serde_json::Value {
        self.captured_body
            .lock()
            .unwrap()
            .clone()
            .expect("no request was captured")
    }
}

#[async_trait]
impl HttpTransport for CapturingTransport {
    async fn execute(&self, _req: Request) -> Result<Response, TransportError> {
        Err(TransportError::Build("execute should not run".to_string()))
    }

    async fn stream(&self, req: Request) -> Result<StreamResponse, TransportError> {
        // Capture the JSON request body (already a serde_json::Value).
        if let Some(codex_client::RequestBody::Json(body)) = req.body {
            *self.captured_body.lock().unwrap() = Some(body);
        }

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
        name: "test-anthropic".to_string(),
        base_url: "https://example.com/v1".to_string(),
        query_params: None,
        headers: HeaderMap::new(),
        retry: codex_api::RetryConfig {
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
    // Minimal valid SSE that completes a message.
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

fn request_with_metadata(metadata: Option<MessagesApiMetadata>) -> MessagesApiRequest {
    MessagesApiRequest {
        model: "claude-sonnet-4.6".to_string(),
        messages: vec![json!({"role": "user", "content": [{"type": "text", "text": "hello"}]})],
        max_tokens: 1024,
        stream: true,
        system: None,
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        temperature: None,
        top_p: None,
        top_k: None,
        stop_sequences: None,
        metadata,
    }
}

// ---------------------------------------------------------------------------
// Unit tests: serialization
// ---------------------------------------------------------------------------

#[test]
fn metadata_serializes_to_expected_json() {
    let metadata = MessagesApiMetadata {
        user_id: "test-user".to_string(),
    };
    let json = serde_json::to_value(&metadata).unwrap();
    assert_eq!(json, json!({"user_id": "test-user"}));
}

#[test]
fn metadata_serializes_with_unicode_user_id() {
    let metadata = MessagesApiMetadata {
        user_id: "\u{7528}\u{6237}@\u{4f8b}\u{3048}.com".to_string(),
    };
    let json = serde_json::to_value(&metadata).unwrap();
    assert_eq!(
        json,
        json!({"user_id": "\u{7528}\u{6237}@\u{4f8b}\u{3048}.com"})
    );
}

#[test]
fn metadata_serializes_with_empty_user_id() {
    let metadata = MessagesApiMetadata {
        user_id: String::new(),
    };
    let json = serde_json::to_value(&metadata).unwrap();
    assert_eq!(json, json!({"user_id": ""}));
}

#[test]
fn metadata_serializes_with_long_user_id() {
    let long_id = "a".repeat(256);
    let metadata = MessagesApiMetadata {
        user_id: long_id.clone(),
    };
    let json = serde_json::to_value(&metadata).unwrap();
    assert_eq!(json["user_id"].as_str().unwrap(), long_id);
}

#[test]
fn request_with_metadata_includes_metadata_field() {
    let request = request_with_metadata(Some(MessagesApiMetadata {
        user_id: "test-user".to_string(),
    }));
    let json = serde_json::to_value(&request).unwrap();

    assert_eq!(json["metadata"]["user_id"], "test-user");
    // Verify it's a nested object, not a flat field.
    assert!(json["metadata"].is_object());
}

#[test]
fn request_without_metadata_omits_field() {
    let request = request_with_metadata(None);
    let json = serde_json::to_value(&request).unwrap();

    // metadata should be absent (skip_serializing_if = "Option::is_none").
    assert!(
        json.get("metadata").is_none(),
        "metadata field should be absent when None, got: {json:?}"
    );
}

#[test]
fn request_preserves_all_fields_alongside_metadata() {
    let request = MessagesApiRequest {
        model: "claude-opus-4.6".to_string(),
        messages: vec![json!({"role": "user", "content": "test"})],
        max_tokens: 4096,
        stream: true,
        system: Some(json!([{"type": "text", "text": "system prompt"}])),
        tools: Some(vec![json!({"type": "function", "name": "shell"})]),
        tool_choice: Some(json!({"type": "any"})),
        thinking: Some(json!({"type": "enabled", "budget_tokens": 8192})),
        output_config: None,
        temperature: Some(0.0),
        top_p: Some(0.9),
        top_k: Some(40),
        stop_sequences: Some(vec!["STOP".to_string()]),
        metadata: Some(MessagesApiMetadata {
            user_id: "test-user".to_string(),
        }),
    };
    let json = serde_json::to_value(&request).unwrap();

    // Verify metadata is present alongside all other fields.
    assert_eq!(json["model"], "claude-opus-4.6");
    assert_eq!(json["max_tokens"], 4096);
    assert_eq!(json["stream"], true);
    assert!(json["system"].is_array());
    assert!(json["tools"].is_array());
    assert_eq!(json["tool_choice"]["type"], "any");
    assert_eq!(json["thinking"]["type"], "enabled");
    assert_eq!(json["temperature"], 0.0);
    assert_eq!(json["top_p"], 0.9);
    assert_eq!(json["top_k"], 40);
    assert_eq!(json["metadata"]["user_id"], "test-user");
}

// ---------------------------------------------------------------------------
// Integration test: transport-level request body capture
// ---------------------------------------------------------------------------

#[tokio::test]
async fn metadata_flows_through_transport_layer() {
    let transport = CapturingTransport::new(minimal_sse_response());
    let client = MessagesClient::new(
        transport.clone(),
        test_provider(),
        std::sync::Arc::new(NoAuth),
    );

    let request = request_with_metadata(Some(MessagesApiMetadata {
        user_id: "test-user".to_string(),
    }));
    let stream = client
        .stream_request(request, HeaderMap::new())
        .await
        .expect("stream_request should succeed");

    // Drain the stream to completion.
    let mut rx = stream.rx_event;
    while rx.recv().await.is_some() {}

    // Verify the captured request body contains metadata.
    let body = transport.captured_body();
    assert_eq!(
        body["metadata"],
        json!({"user_id": "test-user"}),
        "metadata.user_id should be in the request body sent to the transport"
    );
}

#[tokio::test]
async fn no_metadata_when_none_flows_through_transport() {
    let transport = CapturingTransport::new(minimal_sse_response());
    let client = MessagesClient::new(
        transport.clone(),
        test_provider(),
        std::sync::Arc::new(NoAuth),
    );

    let request = request_with_metadata(None);
    let stream = client
        .stream_request(request, HeaderMap::new())
        .await
        .expect("stream_request should succeed");

    // Drain the stream to completion.
    let mut rx = stream.rx_event;
    while rx.recv().await.is_some() {}

    // Verify metadata is absent from the request body.
    let body = transport.captured_body();
    assert!(
        body.get("metadata").is_none(),
        "metadata should not be present when None, got body: {body:?}"
    );
}

#[tokio::test]
async fn metadata_does_not_interfere_with_response_parsing() {
    let transport = CapturingTransport::new(minimal_sse_response());
    let client = MessagesClient::new(
        transport.clone(),
        test_provider(),
        std::sync::Arc::new(NoAuth),
    );

    let request = request_with_metadata(Some(MessagesApiMetadata {
        user_id: "test-parsing".to_string(),
    }));
    let stream = client
        .stream_request(request, HeaderMap::new())
        .await
        .expect("stream_request should succeed");

    let mut events = Vec::new();
    let mut rx = stream.rx_event;
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    // The response should still parse correctly -- we should get response items.
    assert!(
        !events.is_empty(),
        "should receive events even with metadata set"
    );

    // Verify we got a text content block.
    let has_text = events.iter().any(|e| {
        matches!(
            e,
            Ok(codex_api::ResponseEvent::OutputItemAdded(
                codex_protocol::models::ResponseItem::Message { .. }
            ))
        )
    });
    assert!(has_text, "should receive text content in response events");
}
