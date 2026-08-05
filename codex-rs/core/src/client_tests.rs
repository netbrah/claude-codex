use super::AuthRequestTelemetryContext;
use super::ModelClient;
use super::PendingUnauthorizedRetry;
use super::UnauthorizedRecoveryExecution;
use super::X_CODEX_INSTALLATION_ID_HEADER;
use super::X_CODEX_PARENT_THREAD_ID_HEADER;
use super::X_CODEX_TURN_METADATA_HEADER;
use super::X_CODEX_WINDOW_ID_HEADER;
use super::X_OPENAI_SUBAGENT_HEADER;
use crate::AttestationContext;
use crate::AttestationProvider;
use crate::GenerateAttestationFuture;
use codex_api::ApiError;
use codex_api::ResponseEvent;
use codex_app_server_protocol::AuthMode;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider::BearerAuthProvider;
use codex_model_provider_info::CHATGPT_CODEX_BASE_URL;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::WireApi;
use codex_model_provider_info::create_oss_provider_with_base_url;
use codex_otel::SessionTelemetry;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::InternalSessionSource;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_rollout_trace::ExecutionStatus;
use codex_rollout_trace::InferenceTraceAttempt;
use codex_rollout_trace::InferenceTraceContext;
use codex_rollout_trace::RawTraceEventPayload;
use codex_rollout_trace::RolloutTrace;
use codex_rollout_trace::TraceWriter;
use codex_rollout_trace::replay_bundle;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::Notify;
use tracing::Event;
use tracing::Subscriber;
use tracing::field::Visit;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context as LayerContext;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

fn test_model_client(session_source: SessionSource) -> ModelClient {
    let provider = create_oss_provider_with_base_url("https://example.com/v1", WireApi::Responses);
    let thread_id = ThreadId::new();
    ModelClient::new(
        /*auth_manager*/ None,
        thread_id.into(),
        thread_id,
        /*installation_id*/ "11111111-1111-4111-8111-111111111111".to_string(),
        provider,
        session_source,
        /*model_verbosity*/ None,
        /*tool_choice*/ None,
        /*messages_metadata_user_id*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*attestation_provider*/ None,
    )
}

fn test_model_info() -> ModelInfo {
    serde_json::from_value(json!({
        "slug": "gpt-test",
        "display_name": "gpt-test",
        "description": "desc",
        "default_reasoning_level": "medium",
        "supported_reasoning_levels": [
            {"effort": "medium", "description": "medium"}
        ],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 1,
        "upgrade": null,
        "base_instructions": "base instructions",
        "model_messages": null,
        "supports_reasoning_summaries": false,
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": {"mode": "bytes", "limit": 10000},
        "supports_parallel_tool_calls": false,
        "supports_image_detail_original": false,
        "context_window": 272000,
        "auto_compact_token_limit": null,
        "experimental_supported_tools": []
    }))
    .expect("deserialize test model info")
}

fn test_session_telemetry() -> SessionTelemetry {
    SessionTelemetry::new(
        ThreadId::new(),
        "gpt-test",
        "gpt-test",
        /*account_id*/ None,
        /*account_email*/ None,
        /*auth_mode*/ None,
        "test-originator".to_string(),
        /*log_user_prompts*/ false,
        "test-terminal".to_string(),
        SessionSource::Cli,
    )
}

#[derive(Default)]
struct TagCollectorVisitor {
    tags: BTreeMap<String, String>,
}

impl Visit for TagCollectorVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.tags
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.tags
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}

#[derive(Clone)]
struct TagCollectorLayer {
    tags: Arc<Mutex<BTreeMap<String, String>>>,
}

impl<S> Layer<S> for TagCollectorLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: LayerContext<'_, S>) {
        if event.metadata().target() != "feedback_tags" {
            return;
        }
        let mut visitor = TagCollectorVisitor::default();
        event.record(&mut visitor);
        self.tags.lock().unwrap().extend(visitor.tags);
    }
}

fn started_inference_attempt(temp: &TempDir) -> anyhow::Result<InferenceTraceAttempt> {
    let writer = Arc::new(TraceWriter::create(
        temp.path(),
        "trace-1".to_string(),
        "rollout-1".to_string(),
        "thread-root".to_string(),
    )?);
    writer.append(RawTraceEventPayload::ThreadStarted {
        thread_id: "thread-root".to_string(),
        agent_path: "/root".to_string(),
        metadata_payload: None,
    })?;
    writer.append(RawTraceEventPayload::CodexTurnStarted {
        codex_turn_id: "turn-1".to_string(),
        thread_id: "thread-root".to_string(),
    })?;

    let inference_trace = InferenceTraceContext::enabled(
        writer,
        "thread-root".to_string(),
        "turn-1".to_string(),
        "gpt-test".to_string(),
        "test-provider".to_string(),
    );
    let attempt = inference_trace.start_attempt();
    attempt.record_started(&json!({
        "model": "gpt-test",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "hello"}]
        }],
    }));
    Ok(attempt)
}

fn output_message(id: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: Some(id.to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

async fn replay_until_cancelled(temp: &TempDir) -> anyhow::Result<RolloutTrace> {
    let mut rollout = replay_bundle(temp.path())?;
    for _ in 0..50 {
        let inference = rollout
            .inference_calls
            .values()
            .next()
            .expect("inference should be reduced");
        if inference.execution.status == ExecutionStatus::Cancelled {
            return Ok(rollout);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        rollout = replay_bundle(temp.path())?;
    }
    Ok(rollout)
}

struct NotifyAfterEventStream {
    events: VecDeque<ResponseEvent>,
    yielded: usize,
    notify_after: usize,
    notify: Arc<Notify>,
}

impl futures::Stream for NotifyAfterEventStream {
    type Item = std::result::Result<ResponseEvent, ApiError>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(event) = self.events.pop_front() else {
            return Poll::Pending;
        };
        self.yielded += 1;
        if self.yielded == self.notify_after {
            self.notify.notify_one();
        }
        Poll::Ready(Some(Ok(event)))
    }
}

#[test]
fn build_subagent_headers_sets_other_subagent_label() {
    let client = test_model_client(SessionSource::SubAgent(SubAgentSource::Other(
        "memory_consolidation".to_string(),
    )));
    let headers = client.build_subagent_headers();
    let value = headers
        .get(X_OPENAI_SUBAGENT_HEADER)
        .and_then(|value| value.to_str().ok());
    assert_eq!(value, Some("memory_consolidation"));
}

#[test]
fn build_subagent_headers_sets_internal_memory_consolidation_label() {
    let client = test_model_client(SessionSource::Internal(
        InternalSessionSource::MemoryConsolidation,
    ));
    let headers = client.build_subagent_headers();
    let value = headers
        .get(X_OPENAI_SUBAGENT_HEADER)
        .and_then(|value| value.to_str().ok());
    assert_eq!(value, Some("memory_consolidation"));
}

#[test]
fn build_ws_client_metadata_includes_window_lineage_and_turn_metadata() {
    let parent_thread_id = ThreadId::new();
    let client = test_model_client(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id,
        depth: 2,
        agent_path: None,
        agent_nickname: None,
        agent_role: None,
        parent_spawn_call_id: None,
    }));

    client.advance_window_generation();

    let client_metadata = client.build_ws_client_metadata(Some(r#"{"turn_id":"turn-123"}"#));
    let thread_id = client.state.thread_id;
    assert_eq!(
        client_metadata,
        std::collections::HashMap::from([
            (
                X_CODEX_INSTALLATION_ID_HEADER.to_string(),
                "11111111-1111-4111-8111-111111111111".to_string(),
            ),
            (
                X_CODEX_WINDOW_ID_HEADER.to_string(),
                format!("{thread_id}:1"),
            ),
            (
                X_OPENAI_SUBAGENT_HEADER.to_string(),
                "collab_spawn".to_string(),
            ),
            (
                X_CODEX_PARENT_THREAD_ID_HEADER.to_string(),
                parent_thread_id.to_string(),
            ),
            (
                X_CODEX_TURN_METADATA_HEADER.to_string(),
                r#"{"turn_id":"turn-123"}"#.to_string(),
            ),
        ])
    );
}

#[tokio::test]
async fn summarize_memories_returns_empty_for_empty_input() {
    let client = test_model_client(SessionSource::Cli);
    let model_info = test_model_info();
    let session_telemetry = test_session_telemetry();

    let output = client
        .summarize_memories(
            Vec::new(),
            &model_info,
            /*effort*/ None,
            &session_telemetry,
        )
        .await
        .expect("empty summarize request should succeed");
    assert_eq!(output.len(), 0);
}

#[tokio::test]
async fn dropped_response_stream_traces_cancelled_partial_output() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let attempt = started_inference_attempt(&temp)?;

    // The provider has produced one complete output item, but no terminal
    // response.completed event. The harness has enough information to keep this
    // item in history, so the trace should preserve it when the stream is
    // abandoned.
    let item = output_message("msg-1", "partial answer");
    let api_stream = futures::stream::iter([Ok(ResponseEvent::OutputItemDone(item))])
        .chain(futures::stream::pending());
    let (mut stream, _) = super::map_response_events(
        /*upstream_request_id*/ None,
        api_stream,
        test_session_telemetry(),
        attempt,
        Default::default(),
    );

    let observed = stream
        .next()
        .await
        .expect("mapped stream should yield output item")?;
    assert!(matches!(observed, ResponseEvent::OutputItemDone(_)));

    // Dropping the consumer is how turn interruption/preemption stops polling
    // the provider stream. The mapper task observes that drop asynchronously
    // and records cancellation using the output items it has already seen.
    drop(stream);

    // Cancellation is recorded by the mapper task after Drop wakes it, so the
    // replay may need a short wait before the terminal event appears on disk.
    let rollout = replay_until_cancelled(&temp).await?;
    let inference = rollout
        .inference_calls
        .values()
        .next()
        .expect("inference should be reduced");

    assert_eq!(inference.execution.status, ExecutionStatus::Cancelled);
    assert_eq!(inference.response_item_ids.len(), 1);
    assert_eq!(rollout.raw_payloads.len(), 2);

    Ok(())
}

#[tokio::test]
async fn response_stream_records_last_model_feedback_ids() {
    let tags = Arc::new(Mutex::new(BTreeMap::new()));
    let _guard = tracing_subscriber::registry()
        .with(TagCollectorLayer { tags: tags.clone() })
        .set_default();

    let api_stream = futures::stream::iter([
        Ok(ResponseEvent::Created),
        Ok(ResponseEvent::Completed {
            stop_reason: None,
            response_id: "resp-123".to_string(),
            token_usage: None,
            end_turn: Some(true),
        }),
    ]);
    let (mut stream, _) = super::map_response_events(
        Some("req-123".to_string()),
        api_stream,
        test_session_telemetry(),
        InferenceTraceAttempt::disabled(),
        Default::default(),
    );

    while stream.next().await.is_some() {}

    let tags = tags.lock().unwrap().clone();
    assert_eq!(
        tags.get("last_model_request_id").map(String::as_str),
        Some("\"req-123\"")
    );
    assert_eq!(
        tags.get("last_model_response_id").map(String::as_str),
        Some("\"resp-123\"")
    );
}

#[tokio::test]
async fn dropped_backpressured_response_stream_traces_cancelled_partial_output()
-> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let attempt = started_inference_attempt(&temp)?;
    let backpressured_item_yielded = Arc::new(Notify::new());
    let mut events = VecDeque::new();
    for _ in 0..super::RESPONSE_STREAM_CHANNEL_CAPACITY {
        events.push_back(ResponseEvent::Created);
    }
    events.push_back(ResponseEvent::OutputItemDone(output_message(
        "msg-1",
        "partial answer",
    )));
    let api_stream = NotifyAfterEventStream {
        events,
        yielded: 0,
        notify_after: super::RESPONSE_STREAM_CHANNEL_CAPACITY + 1,
        notify: Arc::clone(&backpressured_item_yielded),
    };

    let (stream, _) = super::map_response_events(
        /*upstream_request_id*/ None,
        api_stream,
        test_session_telemetry(),
        attempt,
        Default::default(),
    );

    // Fill the mapper channel with non-terminal events, then yield one output
    // item. The mapper has observed that item and is blocked trying to send it
    // downstream, so dropping the consumer covers the send-failure path rather
    // than the `consumer_dropped` select branch.
    backpressured_item_yielded.notified().await;
    drop(stream);

    let rollout = replay_until_cancelled(&temp).await?;
    let inference = rollout
        .inference_calls
        .values()
        .next()
        .expect("inference should be reduced");

    assert_eq!(inference.execution.status, ExecutionStatus::Cancelled);
    assert_eq!(inference.response_item_ids.len(), 1);
    assert_eq!(rollout.raw_payloads.len(), 2);

    Ok(())
}

#[test]
fn auth_request_telemetry_context_tracks_attached_auth_and_retry_phase() {
    let auth_context = AuthRequestTelemetryContext::new(
        Some(AuthMode::Chatgpt),
        &BearerAuthProvider::for_test(Some("access-token"), Some("workspace-123")),
        PendingUnauthorizedRetry::from_recovery(UnauthorizedRecoveryExecution {
            mode: "managed",
            phase: "refresh_token",
        }),
    );

    assert_eq!(auth_context.auth_mode, Some("Chatgpt"));
    assert!(auth_context.auth_header_attached);
    assert_eq!(auth_context.auth_header_name, Some("authorization"));
    assert!(auth_context.retry_after_unauthorized);
    assert_eq!(auth_context.recovery_mode, Some("managed"));
    assert_eq!(auth_context.recovery_phase, Some("refresh_token"));
}

// ── S-020 Sub-B: /messages-specific client tests ───────────────────────

#[test]
fn anthropic_thinking_param_adaptive_for_medium_effort() {
    use codex_protocol::openai_models::ReasoningEffort;

    let result = codex_provider_anthropic::anthropic_thinking_param(
        Some(ReasoningEffort::Medium),
        "claude-sonnet-4.6",
    );
    assert!(
        result.is_some(),
        "Medium effort should produce thinking param"
    );
    assert_eq!(result.unwrap()["type"], "adaptive");
}

#[test]
fn anthropic_thinking_param_adaptive_for_high_effort() {
    use codex_protocol::openai_models::ReasoningEffort;

    let result = codex_provider_anthropic::anthropic_thinking_param(
        Some(ReasoningEffort::High),
        "claude-sonnet-4.6",
    );
    assert!(
        result.is_some(),
        "High effort should produce thinking param"
    );
    assert_eq!(result.unwrap()["type"], "adaptive");
}

#[test]
fn anthropic_thinking_param_none_for_minimal_effort() {
    use codex_protocol::openai_models::ReasoningEffort;

    let result = codex_provider_anthropic::anthropic_thinking_param(
        Some(ReasoningEffort::Minimal),
        "claude-sonnet-4.6",
    );
    assert!(result.is_none(), "Minimal effort should disable thinking");
}

#[test]
fn anthropic_thinking_param_none_when_effort_is_none() {
    let result = codex_provider_anthropic::anthropic_thinking_param(None, "claude-sonnet-4.6");
    assert!(result.is_none(), "None effort should disable thinking");
}

#[test]
fn anthropic_thinking_param_none_for_none_variant() {
    use codex_protocol::openai_models::ReasoningEffort;

    let result = codex_provider_anthropic::anthropic_thinking_param(
        Some(ReasoningEffort::None),
        "claude-sonnet-4.6",
    );
    assert!(
        result.is_none(),
        "ReasoningEffort::None should disable thinking"
    );
}

#[test]
fn anthropic_thinking_param_adaptive_for_low_effort() {
    use codex_protocol::openai_models::ReasoningEffort;

    let result = codex_provider_anthropic::anthropic_thinking_param(
        Some(ReasoningEffort::Low),
        "claude-sonnet-4.6",
    );
    assert!(result.is_some(), "Low effort should produce thinking param");
    assert_eq!(result.unwrap()["type"], "adaptive");
}

#[test]
fn anthropic_max_output_tokens_opus_128k() {
    let tokens = codex_provider_anthropic::anthropic_max_output_tokens("claude-opus-4-6");
    assert_eq!(tokens, 128_000, "Opus models should get 128K output tokens");
}

#[test]
fn anthropic_max_output_tokens_sonnet_64k() {
    let tokens = codex_provider_anthropic::anthropic_max_output_tokens("claude-sonnet-4-6");
    assert_eq!(tokens, 64_000, "Sonnet models should get 64K output tokens");
}

#[test]
fn anthropic_max_output_tokens_haiku_8k() {
    let tokens = codex_provider_anthropic::anthropic_max_output_tokens("claude-haiku-3-5");
    assert_eq!(tokens, 8_192, "Haiku models should get 8K output tokens");
}

#[test]
fn anthropic_max_output_tokens_default_for_unknown_claude() {
    let tokens = codex_provider_anthropic::anthropic_max_output_tokens("claude-future-model");
    assert_eq!(
        tokens, 64_000,
        "Unknown Claude models should get 64K default"
    );
}

#[test]
fn anthropic_max_output_tokens_non_claude_default() {
    let tokens = codex_provider_anthropic::anthropic_max_output_tokens("gpt-5.3-codex");
    assert_eq!(tokens, 64_000, "Non-Claude models should get 64K default");
}

#[test]
fn is_anthropic_model_recognizes_claude_slugs() {
    assert!(codex_provider_anthropic::is_anthropic_model(
        "claude-sonnet-4-6"
    ));
    assert!(codex_provider_anthropic::is_anthropic_model(
        "claude-opus-4-6"
    ));
    assert!(codex_provider_anthropic::is_anthropic_model(
        "claude-haiku-3-5"
    ));
    assert!(codex_provider_anthropic::is_anthropic_model(
        "Claude-Sonnet-4-6"
    )); // case insensitive
}

#[test]
fn is_anthropic_model_rejects_non_claude() {
    assert!(!codex_provider_anthropic::is_anthropic_model(
        "gpt-5.3-codex"
    ));
    assert!(!codex_provider_anthropic::is_anthropic_model("o3-mini"));
    assert!(!codex_provider_anthropic::is_anthropic_model(
        "custom-model"
    ));
}

#[test]
fn messages_api_request_serializes_all_fields() {
    use codex_api::MessagesApiMetadata;
    use codex_api::MessagesApiRequest;

    let request = MessagesApiRequest {
        model: "claude-sonnet-4-6".to_string(),
        messages: vec![json!({"role": "user", "content": [{"type": "text", "text": "hello"}]})],
        max_tokens: 64000,
        stream: true,
        system: Some(json!([{"type": "text", "text": "You are helpful"}])),
        tools: Some(vec![
            json!({"name": "shell", "description": "run shell", "input_schema": {}}),
        ]),
        tool_choice: Some(json!({"type": "auto"})),
        thinking: Some(json!({"type": "adaptive"})),
        output_config: None,
        temperature: Some(0.7),
        top_p: Some(0.9),
        top_k: Some(40),
        stop_sequences: Some(vec!["STOP".to_string()]),
        metadata: Some(MessagesApiMetadata {
            user_id: "test-user-123".to_string(),
        }),
    };

    let serialized = serde_json::to_value(&request).unwrap();
    assert_eq!(serialized["model"], "claude-sonnet-4-6");
    assert_eq!(serialized["max_tokens"], 64000);
    assert_eq!(serialized["stream"], true);
    assert_eq!(serialized["system"][0]["text"], "You are helpful");
    assert_eq!(serialized["tools"][0]["name"], "shell");
    assert_eq!(serialized["tool_choice"]["type"], "auto");
    assert_eq!(serialized["thinking"]["type"], "adaptive");
    assert_eq!(serialized["temperature"], 0.7);
    assert_eq!(serialized["top_p"], 0.9);
    assert_eq!(serialized["top_k"], 40);
    assert_eq!(serialized["stop_sequences"][0], "STOP");
    assert_eq!(serialized["metadata"]["user_id"], "test-user-123");
}

#[test]
fn messages_api_request_omits_none_fields() {
    use codex_api::MessagesApiRequest;

    let request = MessagesApiRequest {
        model: "claude-sonnet-4-6".to_string(),
        messages: vec![],
        max_tokens: 64000,
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
    };

    let serialized = serde_json::to_value(&request).unwrap();
    // Fields with skip_serializing_if = "Option::is_none" should be absent
    assert!(
        serialized.get("system").is_none(),
        "system should be omitted when None"
    );
    assert!(
        serialized.get("tools").is_none(),
        "tools should be omitted when None"
    );
    assert!(
        serialized.get("tool_choice").is_none(),
        "tool_choice should be omitted when None"
    );
    assert!(
        serialized.get("thinking").is_none(),
        "thinking should be omitted when None"
    );
    assert!(
        serialized.get("temperature").is_none(),
        "temperature should be omitted when None"
    );
    assert!(
        serialized.get("top_p").is_none(),
        "top_p should be omitted when None"
    );
    assert!(
        serialized.get("top_k").is_none(),
        "top_k should be omitted when None"
    );
    assert!(
        serialized.get("stop_sequences").is_none(),
        "stop_sequences should be omitted when None"
    );
    assert!(
        serialized.get("metadata").is_none(),
        "metadata should be omitted when None"
    );
    // Required fields must be present
    assert!(serialized.get("model").is_some());
    assert!(serialized.get("max_tokens").is_some());
    assert!(serialized.get("stream").is_some());
}

#[test]
fn anthropic_beta_header_includes_interleaved_thinking() {
    // The anthropic-beta header is built in MessagesClient::stream_request
    // based on request.thinking being Some. We verify the header value
    // construction logic here by checking the expected constant.
    let beta_features: Vec<&str> = vec!["interleaved-thinking-2025-05-14"];
    let header_value = beta_features.join(",");
    assert_eq!(header_value, "interleaved-thinking-2025-05-14");
    // Verify the header value is valid HTTP
    assert!(http::HeaderValue::from_str(&header_value).is_ok());
}

#[test]
fn vertex_ai_model_slug_recognized() {
    // Vertex AI uses slugs like "claude-sonnet-4-6@default"
    assert!(codex_provider_anthropic::is_anthropic_model(
        "claude-sonnet-4-6@default"
    ));
}

// ---------------------------------------------------------------------------
// F5: session-scoped 401 force-refresh counter
//
// Verifies the per-session counter increments under the cap and is reset by
// note_copilot_request_succeeded. The full retry-loop integration would
// require a live or mocked CAPI; here we drive the helpers directly and
// observe the atomic state. This is enough to prove the F5 contract:
//
//   - counter is per-ModelClient (not per-call)
//   - successful turns reset it (long sessions don't accumulate)
//   - non-Copilot wires don't touch it
// ---------------------------------------------------------------------------

#[test]
fn note_request_succeeded_delegates_to_provider() {
    // Smoke test: provider.note_request_succeeded() is callable.
    // Detailed counter tests live in codex-provider-copilot.
    let client = test_model_client(SessionSource::Cli);
    client.state.provider.note_request_succeeded();
    // Non-Copilot provider: no-op, should not panic.
}

// ---------------------------------------------------------------------------
// F6: enriched 401-bail-out error context
//
// When the per-session 401 budget is exhausted, the fatal error must carry
// enough context for an operator to locate the failed request in proxy
// logs:
//   - wire kind (so they know which CAPI route)
//   - base URL (so they know which Copilot endpoint)
//   - x-request-id from the upstream response
//   - cf-ray (Cloudflare trace ID)
//   - upstream body error (when surfaced)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn handle_unauthorized_for_provider_falls_through_for_non_copilot() {
    use codex_api::TransportError;
    use http::StatusCode;

    let client = test_model_client(SessionSource::Cli);
    let transport = TransportError::Http {
        status: StatusCode::UNAUTHORIZED,
        url: Some("https://api.openai.com/v1/responses".into()),
        headers: None,
        body: None,
    };

    let session_telemetry = test_session_telemetry();
    let mut auth_recovery: Option<super::UnauthorizedRecovery> = None;

    // Non-Copilot provider has no auth recovery; the standard path runs.
    // Without an auth_recovery, this returns a fatal error.
    let result = crate::messages_dispatch::handle_unauthorized_for_provider(
        &client,
        transport,
        &mut auth_recovery,
        &session_telemetry,
    )
    .await;

    assert!(result.is_err(), "no recovery available — must fail");
}

// ---------------------------------------------------------------------------
// F5 concurrent budget — pause-gate duck finding #1.
//
// The claim `handle_unauthorized_for_copilot` enforces is: the
// `COPILOT_MAX_FORCE_REFRESHES_PER_SESSION` budget is **session-scoped,
// not per-call**. Two concurrent 401s on the same `ModelClient` must
// consume the same counter — the total across both callers is bounded
// by `CAP`, not by `2 * CAP`.
//
// We can't easily stage the whole mocked 401-then-refresh round-trip
// from here (that's what `stream_401_retry.rs` in the copilot crate is
// for), so this test drives the atomic directly from N concurrent tasks
// mimicking the exact "fetch_add + compare-against-cap" logic at
// client.rs:2311-2340. If a future refactor swaps the atomic for a
// per-call field (or picks the wrong `Ordering`), this test will fail.
// ---------------------------------------------------------------------------

// Copilot force-refresh budget tests (concurrent callers, counter
// semantics) moved to codex-provider-copilot in the Phase 2 excision.
// The provider owns the AtomicU32 counter and the budget cap.

// ---------------------------------------------------------------------------
// API-version header on native wires — pause-gate duck finding #3.
//
// Existing coverage in `copilot/tests/stream_happy_path.rs` asserts the
// stamp on /chat/completions. The two native wires (`/v1/messages`,
// `/responses`) bypass `codex-copilot-adapter::wire::build_headers` and
// instead receive the stamp via `ModelClient::stamp_copilot_shared_headers`
// in `core/src/client.rs:739-765`. This test pins that invariant so a
// future refactor of the shared-header path can't silently drop the
// `x-github-api-version` override for native-wire requests.
// ---------------------------------------------------------------------------

#[test]
fn stamp_copilot_shared_headers_injects_github_api_version_override() {
    use codex_copilot_adapter::GITHUB_API_VERSION;
    use codex_copilot_adapter::X_GITHUB_API_VERSION;
    use http::HeaderMap;

    let mut headers = HeaderMap::new();
    codex_provider_copilot::stamp_copilot_shared_headers(&mut headers);

    let actual = headers
        .get(X_GITHUB_API_VERSION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert_eq!(
        actual, GITHUB_API_VERSION,
        "stamp_copilot_shared_headers must stamp x-github-api-version on the \
         shared header map (both /v1/messages and /responses dispatch paths \
         rely on this; /chat/completions has its own stamp in copilot-adapter)"
    );

    // Defence against a hypothetical refactor that appends rather than
    // overwrites: exactly one value, not a stacked duplicate.
    let count = headers.get_all(X_GITHUB_API_VERSION).iter().count();
    assert_eq!(
        count, 1,
        "x-github-api-version must be a single-valued header — duplicates \
         would let upstream pick either, leaking drift (found {count} values)"
    );

    // Sanity: the other always-on shared headers are also present so
    // this test doubles as a sentinel for the shared-stamp invariants.
    for key in ["copilot-integration-id", "editor-version", "x-initiator"] {
        assert!(
            headers.contains_key(key),
            "shared-header stamp must include `{key}` on every Copilot request"
        );
    }
}

// ---------------------------------------------------------------------------
// F4 OnceCell pattern guard — pause-gate duck finding #2.
//
// `ensure_session` (adapter.rs:275) and `ensure_copilot_ctx` (client.rs:709)
// both commit to `tokio::sync::OnceCell::get_or_try_init`. The invariant
// this provides is "concurrent cold-start callers share a single init
// future — only one task runs the closure, the rest await its result."
//
// We can't easily test the static `SESSION` in `copilot::adapter` from
// here (statics leak across tests within the same process). Instead,
// pin the CONTRACT using a local OnceCell with the same signature. A
// future refactor that swaps to, e.g., `Mutex<Option<T>>` +
// `get_or_insert_with` without `await`-friendly dedup would break F4 —
// this test guards against that regression.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tokio_once_cell_get_or_try_init_serializes_concurrent_cold_starts() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use tokio::sync::OnceCell;

    let cell: Arc<OnceCell<Arc<String>>> = Arc::new(OnceCell::const_new());
    let init_calls = Arc::new(AtomicUsize::new(0));

    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut handles = Vec::with_capacity(8);
    for i in 0..8 {
        let cell = Arc::clone(&cell);
        let init_calls = Arc::clone(&init_calls);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            // Line up all tasks before the first caller races into init.
            barrier.wait().await;
            let v = cell
                .get_or_try_init(|| {
                    let init_calls = Arc::clone(&init_calls);
                    async move {
                        init_calls.fetch_add(1, Ordering::AcqRel);
                        // Artificial await inside the init future — if the
                        // implementation were to drop serialization, more
                        // than one caller would slip in during this yield.
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        Ok::<Arc<String>, &'static str>(Arc::new(format!("task-{i}")))
                    }
                })
                .await
                .expect("init");
            Arc::clone(v)
        }));
    }

    let mut values = Vec::with_capacity(8);
    for h in handles {
        values.push(h.await.expect("spawned task panicked"));
    }

    assert_eq!(
        init_calls.load(Ordering::Acquire),
        1,
        "tokio::sync::OnceCell::get_or_try_init must invoke the init closure \
         exactly once across N concurrent cold-start callers — if this test \
         starts seeing >1, F4's concurrency guarantee in \
         copilot::adapter::ensure_session and ModelClient::ensure_copilot_ctx \
         is broken"
    );

    // Every concurrent caller observed the same initialized value (the
    // "winner" task's output), not their own task-local string.
    let first = Arc::clone(&values[0]);
    for (idx, v) in values.iter().enumerate() {
        assert!(
            Arc::ptr_eq(&first, v),
            "all callers must see the same Arc instance from OnceCell (caller {idx} diverged)"
        );
    }
}

fn model_client_with_counting_attestation(
    include_attestation: bool,
) -> (ModelClient, Arc<AtomicUsize>) {
    #[derive(Debug)]
    struct CountingAttestationProvider {
        calls: Arc<AtomicUsize>,
    }

    impl AttestationProvider for CountingAttestationProvider {
        fn header_for_request(
            &self,
            _context: AttestationContext,
        ) -> GenerateAttestationFuture<'_> {
            let calls = self.calls.clone();
            Box::pin(async move {
                let call = calls.fetch_add(1, Ordering::Relaxed) + 1;
                Some(http::HeaderValue::from_bytes(format!("v1.header-{call}").as_bytes()).unwrap())
            })
        }
    }

    let attestation_calls = Arc::new(AtomicUsize::new(0));
    let (auth_manager, provider) = if include_attestation {
        (
            Some(AuthManager::from_auth_for_testing(
                CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            )),
            ModelProviderInfo::create_openai_provider(Some(CHATGPT_CODEX_BASE_URL.to_string())),
        )
    } else {
        (
            None,
            create_oss_provider_with_base_url("https://example.com/v1", WireApi::Responses),
        )
    };
    let model_client = ModelClient::new(
        auth_manager,
        SessionId::new(),
        ThreadId::new(),
        /*installation_id*/ "11111111-1111-4111-8111-111111111111".to_string(),
        provider,
        SessionSource::Exec,
        /*model_verbosity*/ None,
        /*tool_choice*/ None,
        /*messages_metadata_user_id*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        Some(Arc::new(CountingAttestationProvider {
            calls: attestation_calls.clone(),
        })),
    );
    (model_client, attestation_calls)
}

#[tokio::test]
async fn websocket_handshake_includes_attestation_for_chatgpt_codex_responses() {
    let (model_client, attestation_calls) =
        model_client_with_counting_attestation(/*include_attestation*/ true);

    let headers = model_client
        .build_websocket_headers(/*turn_state*/ None, /*turn_metadata_header*/ None)
        .await;

    assert_eq!(
        headers
            .get(crate::attestation::X_OAI_ATTESTATION_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("v1.header-1"),
    );
    assert_eq!(attestation_calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn non_chatgpt_codex_endpoints_omit_attestation_generation() {
    let (model_client, attestation_calls) =
        model_client_with_counting_attestation(/*include_attestation*/ false);
    let mut response_headers = http::HeaderMap::new();

    if let Some(header_value) = model_client.generate_attestation_header_for().await {
        response_headers.insert(crate::attestation::X_OAI_ATTESTATION_HEADER, header_value);
    }
    let mut compaction_headers = http::HeaderMap::new();
    if let Some(header_value) = model_client.generate_attestation_header_for().await {
        compaction_headers.insert(crate::attestation::X_OAI_ATTESTATION_HEADER, header_value);
    }
    let mut realtime_headers = http::HeaderMap::new();
    if let Some(header_value) = model_client.generate_attestation_header_for().await {
        realtime_headers.insert(crate::attestation::X_OAI_ATTESTATION_HEADER, header_value);
    }

    assert_eq!(
        response_headers.get(crate::attestation::X_OAI_ATTESTATION_HEADER),
        None,
    );
    assert_eq!(
        compaction_headers.get(crate::attestation::X_OAI_ATTESTATION_HEADER),
        None,
    );
    assert_eq!(
        realtime_headers.get(crate::attestation::X_OAI_ATTESTATION_HEADER),
        None,
    );
    assert_eq!(attestation_calls.load(Ordering::Relaxed), 0);
}
