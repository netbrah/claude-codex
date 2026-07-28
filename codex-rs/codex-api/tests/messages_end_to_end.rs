#![allow(clippy::unwrap_used, clippy::expect_used)]
//! End-to-end integration tests for the Anthropic `/messages` wire protocol.
//!
//! Uses a fixture HTTP transport to feed canned SSE bytes through the full
//! `MessagesClient` → SSE parser → `ResponseEvent` pipeline without network.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use codex_api::AuthProvider;
use codex_api::MessagesApiRequest;
use codex_api::MessagesClient;
use codex_api::Provider;
use codex_api::ResponseEvent;
use codex_client::HttpTransport;
use codex_client::Request;
use codex_client::Response;
use codex_client::StreamResponse;
use codex_client::TransportError;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use http::HeaderMap;
use http::StatusCode;
use pretty_assertions::assert_eq;
use serde_json::json;

#[derive(Clone)]
struct FixtureSseTransport {
    body: String,
}

impl FixtureSseTransport {
    fn new(body: String) -> Self {
        Self { body }
    }
}

#[async_trait]
impl HttpTransport for FixtureSseTransport {
    async fn execute(&self, _req: Request) -> Result<Response, TransportError> {
        Err(TransportError::Build("execute should not run".to_string()))
    }

    async fn stream(&self, _req: Request) -> Result<StreamResponse, TransportError> {
        let stream = futures::stream::iter(vec![Ok::<Bytes, TransportError>(Bytes::from(
            self.body.clone(),
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

fn provider() -> Provider {
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

fn build_messages_sse(lines: &[&str]) -> String {
    let mut body = String::new();
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    body
}

fn simple_request() -> MessagesApiRequest {
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
        metadata: None,
    }
}

async fn collect_events(transport: FixtureSseTransport) -> Vec<Result<ResponseEvent, String>> {
    let client = MessagesClient::new(transport, provider(), std::sync::Arc::new(NoAuth));
    let stream = client
        .stream_request(simple_request(), HeaderMap::new())
        .await
        .expect("stream_request should succeed");

    let mut rx = stream.rx_event;
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event.map_err(|e| format!("{e:?}")));
    }
    events
}

#[tokio::test]
async fn messages_text_streaming_end_to_end() -> Result<()> {
    let body = build_messages_sse(&[
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_e2e_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":15,\"output_tokens\":0}}}",
        "",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
        "",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"4\"}}",
        "",
        "data: {\"type\":\"content_block_stop\",\"index\":0}",
        "",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}",
        "",
        "data: {\"type\":\"message_stop\"}",
        "",
    ]);

    let events = collect_events(FixtureSseTransport::new(body)).await;
    let ok_events: Vec<_> = events.iter().filter_map(|e| e.as_ref().ok()).collect();

    assert!(
        ok_events
            .iter()
            .any(|e| matches!(e, ResponseEvent::Created)),
        "must emit Created"
    );
    assert!(
        ok_events
            .iter()
            .any(|e| matches!(e, ResponseEvent::OutputTextDelta(t) if t == "4")),
        "must emit text delta"
    );

    let mut found_message = false;
    let mut found_completed = false;
    for event in &ok_events {
        match event {
            ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }) => {
                if let Some(ContentItem::OutputText { text }) = content.first() {
                    assert_eq!(text, "4");
                    found_message = true;
                }
            }
            ResponseEvent::Completed {
                response_id,
                token_usage,
                ..
            } => {
                assert_eq!(response_id, "msg_e2e_1");
                let usage = token_usage.as_ref().expect("usage present");
                assert_eq!(usage.input_tokens, 15);
                assert_eq!(usage.output_tokens, 1);
                found_completed = true;
            }
            _ => {}
        }
    }
    assert!(found_message, "must emit OutputItemDone(Message)");
    assert!(found_completed, "must emit Completed with usage");
    Ok(())
}

#[tokio::test]
async fn messages_tool_use_end_to_end() -> Result<()> {
    let body = build_messages_sse(&[
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_e2e_2\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":50,\"output_tokens\":0}}}",
        "",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
        "",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me read that file.\"}}",
        "",
        "data: {\"type\":\"content_block_stop\",\"index\":0}",
        "",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_e2e_01\",\"name\":\"read_file\",\"input\":{}}}",
        "",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\"\"}}",
        "",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\": \\\"src/main.rs\\\"}\"}}",
        "",
        "data: {\"type\":\"content_block_stop\",\"index\":1}",
        "",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":30}}",
        "",
        "data: {\"type\":\"message_stop\"}",
        "",
    ]);

    let events = collect_events(FixtureSseTransport::new(body)).await;
    let ok_events: Vec<_> = events.iter().filter_map(|e| e.as_ref().ok()).collect();

    let mut found_text = false;
    let mut found_tool = false;
    let mut found_completed = false;
    for event in &ok_events {
        match event {
            ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }) => {
                if let Some(ContentItem::OutputText { text }) = content.first() {
                    assert_eq!(text, "Let me read that file.");
                    found_text = true;
                }
            }
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            }) => {
                assert_eq!(call_id, "toolu_e2e_01");
                assert_eq!(name, "read_file");
                assert_eq!(arguments, "{\"path\": \"src/main.rs\"}");
                found_tool = true;
            }
            ResponseEvent::Completed { response_id, .. } => {
                assert_eq!(response_id, "msg_e2e_2");
                found_completed = true;
            }
            _ => {}
        }
    }
    assert!(found_text, "must emit text OutputItemDone");
    assert!(found_tool, "must emit FunctionCall OutputItemDone");
    assert!(found_completed, "must emit Completed");
    Ok(())
}

#[tokio::test]
async fn messages_thinking_with_signature_end_to_end() -> Result<()> {
    let body = build_messages_sse(&[
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_e2e_3\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}",
        "",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}",
        "",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Let me reason about this carefully.\"}}",
        "",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"ErUmSignatureABC123==\"}}",
        "",
        "data: {\"type\":\"content_block_stop\",\"index\":0}",
        "",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
        "",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"The answer is 42.\"}}",
        "",
        "data: {\"type\":\"content_block_stop\",\"index\":1}",
        "",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":20}}",
        "",
        "data: {\"type\":\"message_stop\"}",
        "",
    ]);

    let events = collect_events(FixtureSseTransport::new(body)).await;
    let ok_events: Vec<_> = events.iter().filter_map(|e| e.as_ref().ok()).collect();

    let mut found_thinking_delta = false;
    let mut found_reasoning = false;
    let mut found_text = false;
    for event in &ok_events {
        match event {
            ResponseEvent::ReasoningContentDelta { delta, .. }
                if delta.contains("reason about this") =>
            {
                found_thinking_delta = true;
            }
            ResponseEvent::OutputItemDone(ResponseItem::Reasoning {
                encrypted_content, ..
            }) => {
                assert_eq!(
                    encrypted_content.as_deref(),
                    Some("ErUmSignatureABC123=="),
                    "signature must be preserved in encrypted_content"
                );
                found_reasoning = true;
            }
            ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }) => {
                if let Some(ContentItem::OutputText { text }) = content.first() {
                    assert_eq!(text, "The answer is 42.");
                    found_text = true;
                }
            }
            _ => {}
        }
    }
    assert!(found_thinking_delta, "must stream thinking deltas");
    assert!(found_reasoning, "must emit Reasoning with signature");
    assert!(found_text, "must emit text after thinking");
    Ok(())
}

#[tokio::test]
async fn messages_error_event_end_to_end() -> Result<()> {
    let body = build_messages_sse(&[
        "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}",
        "",
    ]);

    let events = collect_events(FixtureSseTransport::new(body)).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Err(msg) if msg.contains("ServerOverloaded"))),
        "must propagate ServerOverloaded error"
    );
    Ok(())
}

/// Simulates the stream produced by a LiteLLM proxy when routing an OpenAI
/// model through /v1/messages → Responses API and translating
/// reasoning_summary_text.delta back into thinking_delta events.
///
/// This is the exact scenario fixed by adding `"summary"` to the thinking
/// JSON in client.rs: once the proxy receives the summary hint, it passes
/// `reasoning: {summary: "detailed"}` to the Responses API, which then emits
/// `reasoning_summary_text.delta` events that the proxy translates to
/// `thinking_delta` events.
#[tokio::test]
async fn messages_proxy_translated_reasoning_deltas_end_to_end() -> Result<()> {
    // This fixture mimics what LiteLLM's AnthropicResponsesStreamWrapper
    // produces when Responses API reasoning summary deltas are available.
    let body = build_messages_sse(&[
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_proxy_reason\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"gpt-5.3-codex\",\"usage\":{\"input_tokens\":20,\"output_tokens\":0}}}",
        "",
        // Reasoning block start (translated from response.output_item.added type=reasoning)
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}",
        "",
        // Reasoning summary deltas (translated from response.reasoning_summary_text.delta)
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"**Calculating\"}}",
        "",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\" the product**\"}}",
        "",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"\\nMultiplying 27 by 453 step by step.\"}}",
        "",
        "data: {\"type\":\"content_block_stop\",\"index\":0}",
        "",
        // Text response
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
        "",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"12,231\"}}",
        "",
        "data: {\"type\":\"content_block_stop\",\"index\":1}",
        "",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":15}}",
        "",
        "data: {\"type\":\"message_stop\"}",
        "",
    ]);

    let events = collect_events(FixtureSseTransport::new(body)).await;
    let ok_events: Vec<_> = events.iter().filter_map(|e| e.as_ref().ok()).collect();

    // Verify model name reflects the proxied OpenAI model
    assert!(
        ok_events
            .iter()
            .any(|e| matches!(e, ResponseEvent::ServerModel(m) if m == "gpt-5.3-codex")),
        "must emit ServerModel(gpt-5.3-codex)"
    );

    // Verify OutputItemAdded(Reasoning) precedes thinking deltas
    let reasoning_added_idx = ok_events.iter().position(|e| {
        matches!(
            e,
            ResponseEvent::OutputItemAdded(ResponseItem::Reasoning { .. })
        )
    });
    assert!(
        reasoning_added_idx.is_some(),
        "must emit OutputItemAdded(Reasoning) for thinking block start"
    );

    // Verify we get streaming thinking deltas (3 of them)
    let thinking_deltas: Vec<_> = ok_events
        .iter()
        .filter(|e| matches!(e, ResponseEvent::ReasoningContentDelta { .. }))
        .collect();
    assert_eq!(
        thinking_deltas.len(),
        3,
        "must emit 3 thinking deltas from proxy-translated stream"
    );

    // Verify the first delta contains the bold header for TUI shimmer
    if let ResponseEvent::ReasoningContentDelta { delta, .. } = thinking_deltas[0] {
        assert!(
            delta.contains("**Calculating"),
            "first delta should contain bold header pattern for TUI: {delta}"
        );
    }

    // Verify the accumulated reasoning text in OutputItemDone
    let mut found_reasoning_done = false;
    for event in &ok_events {
        if let ResponseEvent::OutputItemDone(ResponseItem::Reasoning { summary, .. }) = event {
            let combined: String = summary
                .iter()
                .map(|s| match s {
                    codex_protocol::models::ReasoningItemReasoningSummary::SummaryText { text } => {
                        text.as_str()
                    }
                })
                .collect();
            assert!(
                combined.contains("Calculating"),
                "reasoning summary must contain accumulated thinking text"
            );
            assert!(
                combined.contains("step by step"),
                "reasoning summary must contain all deltas"
            );
            found_reasoning_done = true;
        }
    }
    assert!(
        found_reasoning_done,
        "must emit OutputItemDone(Reasoning) with accumulated text"
    );

    // Verify text response follows
    let mut found_text = false;
    for event in &ok_events {
        if let ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }) = event
            && let Some(ContentItem::OutputText { text }) = content.first()
        {
            assert_eq!(text, "12,231");
            found_text = true;
        }
    }
    assert!(found_text, "must emit text OutputItemDone after reasoning");

    // Verify Completed
    assert!(
        ok_events
            .iter()
            .any(|e| matches!(e, ResponseEvent::Completed { response_id, .. } if response_id == "msg_proxy_reason")),
        "must emit Completed"
    );

    Ok(())
}

/// Validates that when no thinking is configured (effort=None/Minimal), the
/// stream works without any reasoning blocks — regression guard.
#[tokio::test]
async fn messages_no_thinking_no_reasoning_events() -> Result<()> {
    let body = build_messages_sse(&[
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_no_think\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}",
        "",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
        "",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello!\"}}",
        "",
        "data: {\"type\":\"content_block_stop\",\"index\":0}",
        "",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}",
        "",
        "data: {\"type\":\"message_stop\"}",
        "",
    ]);

    let events = collect_events(FixtureSseTransport::new(body)).await;
    let ok_events: Vec<_> = events.iter().filter_map(|e| e.as_ref().ok()).collect();

    // No reasoning events at all
    assert!(
        !ok_events
            .iter()
            .any(|e| matches!(e, ResponseEvent::ReasoningContentDelta { .. })),
        "must NOT emit any reasoning deltas when no thinking block present"
    );
    assert!(
        !ok_events.iter().any(|e| matches!(
            e,
            ResponseEvent::OutputItemAdded(ResponseItem::Reasoning { .. })
        )),
        "must NOT emit OutputItemAdded(Reasoning) when no thinking block present"
    );
    assert!(
        !ok_events.iter().any(|e| matches!(
            e,
            ResponseEvent::OutputItemDone(ResponseItem::Reasoning { .. })
        )),
        "must NOT emit OutputItemDone(Reasoning) when no thinking block present"
    );

    // Text works fine
    assert!(
        ok_events
            .iter()
            .any(|e| matches!(e, ResponseEvent::OutputTextDelta(t) if t == "Hello!")),
        "text deltas must still work"
    );
    assert!(
        ok_events
            .iter()
            .any(|e| matches!(e, ResponseEvent::Completed { .. })),
        "must complete"
    );

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Sampling parameter serialization tests — W-3 sortie
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn messages_request_omits_sampling_params_when_none() {
    let request = simple_request();
    let json = serde_json::to_value(&request).expect("serialize");
    // When None, temperature/top_p/top_k should be omitted entirely
    // (skip_serializing_if on the struct).
    assert!(json.get("temperature").is_none());
    assert!(json.get("top_p").is_none());
    assert!(json.get("top_k").is_none());
}

#[test]
fn messages_request_includes_temperature_when_set() {
    let request = MessagesApiRequest {
        temperature: Some(0.0),
        ..simple_request()
    };
    let json = serde_json::to_value(&request).expect("serialize");
    assert_eq!(json["temperature"], serde_json::json!(0.0));
    // top_p and top_k still omitted
    assert!(json.get("top_p").is_none());
    assert!(json.get("top_k").is_none());
}

#[test]
fn messages_request_includes_top_p_when_set() {
    let request = MessagesApiRequest {
        top_p: Some(0.9),
        ..simple_request()
    };
    let json = serde_json::to_value(&request).expect("serialize");
    assert_eq!(json["top_p"], serde_json::json!(0.9));
    assert!(json.get("temperature").is_none());
    assert!(json.get("top_k").is_none());
}

#[test]
fn messages_request_includes_top_k_when_set() {
    let request = MessagesApiRequest {
        top_k: Some(40),
        ..simple_request()
    };
    let json = serde_json::to_value(&request).expect("serialize");
    assert_eq!(json["top_k"], serde_json::json!(40));
    assert!(json.get("temperature").is_none());
    assert!(json.get("top_p").is_none());
}

#[test]
fn messages_request_includes_all_sampling_params() {
    let request = MessagesApiRequest {
        temperature: Some(0.7),
        top_p: Some(0.95),
        top_k: Some(50),
        ..simple_request()
    };
    let json = serde_json::to_value(&request).expect("serialize");
    assert_eq!(json["temperature"], serde_json::json!(0.7));
    assert_eq!(json["top_p"], serde_json::json!(0.95));
    assert_eq!(json["top_k"], serde_json::json!(50));
}

#[test]
fn messages_request_temperature_zero_serializes_correctly() {
    // temperature=0 is the critical deterministic-mode use case.
    let request = MessagesApiRequest {
        temperature: Some(0.0),
        ..simple_request()
    };
    let json = serde_json::to_value(&request).expect("serialize");
    // Must be present and exactly 0.0, not omitted.
    assert_eq!(json["temperature"], serde_json::json!(0.0));
}

#[test]
fn messages_request_top_k_one_serializes_correctly() {
    // top_k=1 means greedy decoding (only the most likely token).
    let request = MessagesApiRequest {
        top_k: Some(1),
        ..simple_request()
    };
    let json = serde_json::to_value(&request).expect("serialize");
    assert_eq!(json["top_k"], serde_json::json!(1));
}

/// Verifies that `stop_sequences` is serialized into the request body and that
/// the response correctly reports `stop_reason: "stop_sequence"` when the model
/// halts on one of the configured sequences.
#[tokio::test]
async fn messages_stop_sequences_end_to_end() -> Result<()> {
    let body = build_messages_sse(&[
        r#"data: {"type":"message_start","message":{"id":"msg_stop_seq","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":0}}}"#,
        "",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        "",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"counting 1 2 3"}}"#,
        "",
        r#"data: {"type":"content_block_stop","index":0}"#,
        "",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"stop_sequence","stop_sequence":"STOP"},"usage":{"output_tokens":5}}"#,
        "",
        r#"data: {"type":"message_stop"}"#,
        "",
    ]);

    // Build a request WITH stop_sequences populated.
    let request = MessagesApiRequest {
        model: "claude-sonnet-4.6".to_string(),
        messages: vec![json!({"role": "user", "content": [{"type": "text", "text": "count"}]})],
        max_tokens: 512,
        stream: true,
        system: None,
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        temperature: None,
        top_p: None,
        top_k: None,
        stop_sequences: Some(vec!["STOP".to_string(), "</answer>".to_string()]),
        metadata: None,
    };

    // Verify the field serializes correctly.
    let serialized = serde_json::to_value(&request)?;
    assert_eq!(
        serialized["stop_sequences"],
        json!(["STOP", "</answer>"]),
        "stop_sequences must appear in serialized request body"
    );

    // Verify None omits the field entirely.
    let request_none = simple_request();
    let serialized_none = serde_json::to_value(&request_none)?;
    assert!(
        serialized_none.get("stop_sequences").is_none(),
        "stop_sequences must be omitted when None"
    );

    // Verify SSE response handling for stop_sequence stop_reason.
    let transport = FixtureSseTransport::new(body);
    let client = MessagesClient::new(transport, provider(), std::sync::Arc::new(NoAuth));
    let stream = client
        .stream_request(request, HeaderMap::new())
        .await
        .expect("stream_request should succeed");

    let mut rx = stream.rx_event;
    let mut found_stop_sequence = false;
    while let Some(event) = rx.recv().await {
        if let Ok(ResponseEvent::Completed { stop_reason, .. }) = event {
            assert_eq!(
                stop_reason.as_deref(),
                Some("stop_sequence"),
                "stop_reason must be stop_sequence"
            );
            found_stop_sequence = true;
        }
    }
    assert!(
        found_stop_sequence,
        "must find Completed with stop_reason=stop_sequence"
    );
    Ok(())
}

// ── T-6-A: stop_sequences serialization ──────────────────────────────

/// Transport that captures the request body JSON for assertion.
#[derive(Clone)]
struct CapturingTransport {
    captured_body: std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    response_body: String,
}

impl CapturingTransport {
    fn new(response_body: String) -> Self {
        Self {
            captured_body: std::sync::Arc::new(std::sync::Mutex::new(None)),
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

fn minimal_sse_response() -> String {
    [
        r#"data: {"type":"message_start","message":{"id":"msg_cap","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":1,"output_tokens":0}}}"#,
        "",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        "",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#,
        "",
        r#"data: {"type":"content_block_stop","index":0}"#,
        "",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
        "",
        r#"data: {"type":"message_stop"}"#,
        "",
    ]
    .join("\n")
}

#[tokio::test]
async fn stop_sequences_serialized_in_request() -> Result<()> {
    let request = MessagesApiRequest {
        stop_sequences: Some(vec!["STOP".to_string(), "</s>".to_string()]),
        ..simple_request()
    };
    let transport = CapturingTransport::new(minimal_sse_response());
    let client = MessagesClient::new(transport.clone(), provider(), std::sync::Arc::new(NoAuth));
    let _ = client.stream_request(request, HeaderMap::new()).await?;
    let body = transport.captured_body();
    let seqs = body["stop_sequences"]
        .as_array()
        .expect("stop_sequences in body");
    assert_eq!(seqs[0], "STOP");
    assert_eq!(seqs[1], "</s>");
    Ok(())
}

#[tokio::test]
async fn stop_sequences_absent_when_none() -> Result<()> {
    let request = MessagesApiRequest {
        stop_sequences: None,
        ..simple_request()
    };
    let transport = CapturingTransport::new(minimal_sse_response());
    let client = MessagesClient::new(transport.clone(), provider(), std::sync::Arc::new(NoAuth));
    let _ = client.stream_request(request, HeaderMap::new()).await?;
    let body = transport.captured_body();
    assert!(
        body.get("stop_sequences").is_none(),
        "stop_sequences must be absent from JSON when None"
    );
    Ok(())
}

// ── T-6-B: adaptive thinking serialization ───────────────────────────

#[tokio::test]
async fn thinking_adaptive_serialized_in_request() -> Result<()> {
    let request = MessagesApiRequest {
        thinking: Some(json!({"type": "adaptive"})),
        output_config: None,
        max_tokens: 128_000,
        ..simple_request()
    };
    let transport = CapturingTransport::new(minimal_sse_response());
    let client = MessagesClient::new(transport.clone(), provider(), std::sync::Arc::new(NoAuth));
    let _ = client.stream_request(request, HeaderMap::new()).await?;
    let body = transport.captured_body();
    assert_eq!(body["thinking"]["type"], "adaptive");
    Ok(())
}
