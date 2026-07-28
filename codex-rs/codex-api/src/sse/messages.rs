//! SSE parser for the Anthropic `/messages` wire protocol.
//!
//! Maps Anthropic SSE events into [`ResponseEvent`] so the rest of codex-rs
//! is wire-protocol agnostic.

use crate::common::ResponseEvent;
use crate::error::ApiError;
use codex_client::ByteStream;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::WebSearchAction;

use crate::sse::usage::RawUsage;
use crate::sse::usage::normalize_token_usage;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::debug;
use tracing::trace;
use tracing::warn;

use crate::common::ResponseStream;
use crate::sse::messages_wire_types::AnthropicErrorKind;
use crate::sse::messages_wire_types::ContentBlock;
use crate::sse::messages_wire_types::ContentBlockDelta;
use crate::sse::messages_wire_types::MessageStreamEvent;

/// Parse a flat wire tool name back into a structured
/// `(namespace, bare_name)` pair so the tool-router HashMap lookup
/// (which keys on the structured `ToolName` shape used at
/// registration time) succeeds.
///
/// For inputs that start with `mcp__<server>__<tool>` (the canonical
/// XLI flat wire form for namespaced MCP tools on `/messages`),
/// returns `(Some("mcp__<server>"), "<tool>")`. For built-in tool
/// names and any input without the `mcp__<server>__<tool>` shape,
/// returns `(None, name)` so the call still dispatches as a plain
/// function tool.
///
/// Inlined here (not imported from `codex-mcp`) because
/// `codex-mcp` depends on `codex-api`; the reverse dependency is
/// not available. Keep this in sync with
/// `codex_mcp::parse_flat_mcp_tool_name`.
pub(crate) fn parse_flat_mcp_tool_name_pub(name: &str) -> (Option<String>, String) {
    parse_flat_mcp_tool_name(name)
}

fn parse_flat_mcp_tool_name(name: &str) -> (Option<String>, String) {
    const MCP_PREFIX: &str = "mcp__";
    const DELIM: &str = "__";
    let Some(rest) = name.strip_prefix(MCP_PREFIX) else {
        return (None, name.to_string());
    };
    let Some(idx) = rest.find(DELIM) else {
        return (None, name.to_string());
    };
    let (server, after) = rest.split_at(idx);
    let tool = &after[DELIM.len()..];
    if server.is_empty() || tool.is_empty() {
        return (None, name.to_string());
    }
    (Some(format!("{MCP_PREFIX}{server}")), tool.to_string())
}

/// Curated catalogue of Anthropic `/messages` wire-vocabulary strings
/// XLI knows about. **This is rung-2 of the harness-invariant ladder
/// (see `cli-ops/sortie-board/xli-v3/`)** — a stringly-typed table
/// preserved as documentation alongside the rung-3 typed enums in
/// [`crate::sse::messages_wire_types`].
///
/// Every entry is either:
///   - **handled**: the parser has a dedicated arm for it.
///   - **drop**: explicit silent drop (we know what it is, we don't
///     surface it; document the policy here so it's not invisible).
///
/// With rung-3 typed enums in place, unknown upstream types now route
/// to a typed `Unknown { tag, raw }` variant rather than a silent
/// fall-through, and exhaustive matching on the enum is the compile-time
/// guard that a new variant doesn't get forgotten.
///
/// Source-of-truth for the wire vocabulary:
///   https://docs.anthropic.com/en/api/messages-streaming
pub(crate) mod wire_vocab {
    /// Top-level SSE event types we receive on /messages stream.
    #[allow(dead_code)] // consumed by wire_vocab_consistent_with_parser regression test
    pub(crate) const STREAM_EVENTS: &[(&str, WirePolicy)] = &[
        ("message_start", WirePolicy::Handled),
        ("content_block_start", WirePolicy::Handled),
        ("content_block_delta", WirePolicy::Handled),
        ("content_block_stop", WirePolicy::Handled),
        ("message_delta", WirePolicy::Handled),
        ("message_stop", WirePolicy::Handled),
        (
            "ping",
            WirePolicy::DropExplicit("keepalive; no payload to surface"),
        ),
        ("error", WirePolicy::Handled),
    ];

    /// `content_block.type` discriminator. Set by `content_block_start`.
    #[allow(dead_code)] // consumed by wire_vocab_consistent_with_parser regression test
    pub(crate) const CONTENT_BLOCKS: &[(&str, WirePolicy)] = &[
        ("text", WirePolicy::Handled),
        ("tool_use", WirePolicy::Handled),
        ("thinking", WirePolicy::Handled),
        ("redacted_thinking", WirePolicy::Handled),
        (
            "server_tool_use",
            WirePolicy::DropExplicit("server-side beta tool; not currently surfaced to XLI"),
        ),
        (
            "web_search_tool_result",
            WirePolicy::DropExplicit("server-side beta tool; not currently surfaced to XLI"),
        ),
        (
            "code_execution_tool_use",
            WirePolicy::DropExplicit("server-side beta tool; not currently surfaced to XLI"),
        ),
    ];

    /// `delta.type` discriminator on `content_block_delta` events.
    #[allow(dead_code)] // consumed by wire_vocab_consistent_with_parser regression test
    pub(crate) const CONTENT_BLOCK_DELTAS: &[(&str, WirePolicy)] = &[
        ("text_delta", WirePolicy::Handled),
        ("input_json_delta", WirePolicy::Handled),
        ("thinking_delta", WirePolicy::Handled),
        ("signature_delta", WirePolicy::Handled),
        (
            "citations_delta",
            WirePolicy::DropExplicit("citations not surfaced in XLI yet; tracked separately"),
        ),
    ];

    #[allow(dead_code)] // consumed by wire_vocab_* regression tests
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum WirePolicy {
        /// Parser has a dedicated arm; payload is surfaced as a `ResponseEvent`.
        Handled,
        /// Wire type is recognized but intentionally not surfaced. The
        /// `&'static str` is human-readable rationale that shows up in
        /// the regression test if anyone tries to remove it.
        DropExplicit(&'static str),
    }
}

/// Tracks in-flight content blocks by index.
struct BlockTracker {
    blocks: HashMap<u64, BlockState>,
}

enum BlockState {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    ToolUse {
        call_id: String,
        name: String,
        arguments: String,
    },
    ServerToolUse {
        id: String,
        name: String,
        arguments: String,
    },
    WebSearchToolResult {
        tool_use_id: String,
    },
    RedactedThinking {
        data: String,
    },
}

impl BlockTracker {
    fn new() -> Self {
        Self {
            blocks: HashMap::new(),
        }
    }
}

/// Spawns a task that reads SSE events from a `/messages` byte stream and maps
/// them into `ResponseEvent`s on the returned channel.
pub fn spawn_messages_stream(stream: ByteStream, idle_timeout: Duration) -> ResponseStream {
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(process_messages_sse(stream, tx_event, idle_timeout));
    ResponseStream {
        rx_event,
        upstream_request_id: None,
    }
}

async fn process_messages_sse(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
) {
    let mut sse_stream = stream.eventsource();
    let mut tracker = BlockTracker::new();
    let mut response_id = String::new();
    let mut usage_holder: Option<AnthropicUsage> = None;
    let mut stop_reason: Option<String> = None;
    let mut tool_use_truncated = false;

    loop {
        let response = timeout(idle_timeout, sse_stream.next()).await;

        let sse = match response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(e))) => {
                debug!("Messages SSE error: {e:#}");
                let _ = tx_event.send(Err(ApiError::Stream(e.to_string()))).await;
                return;
            }
            Ok(None) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(
                        "messages stream closed before message_stop".into(),
                    )))
                    .await;
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(
                        "idle timeout waiting for messages SSE".into(),
                    )))
                    .await;
                return;
            }
        };

        if sse.data.is_empty() {
            continue;
        }

        trace!("Messages SSE event: {}", &sse.data);

        let event: MessageStreamEvent = match serde_json::from_str(&sse.data) {
            Ok(event) => event,
            Err(e) => {
                debug!(
                    "Failed to parse messages SSE event: {e}, data: {}",
                    &sse.data
                );
                continue;
            }
        };

        // Rung-3 (S-WIRE-VOCAB-MAX-TEETH): exhaustive match on the typed
        // `MessageStreamEvent` enum. The compiler enforces that every variant
        // is handled; `MessageStreamEvent::Unknown` is the ONLY tolerant arm
        // and it carries the original tag + raw JSON for logging.
        match event {
            MessageStreamEvent::MessageStart { message } => {
                if let Some(id) = message.id.as_deref() {
                    response_id = id.to_owned();
                }
                if let Some(u) = message.usage.as_ref()
                    && let Ok(u) = serde_json::from_value::<AnthropicUsage>(u.clone())
                {
                    usage_holder = Some(u);
                }
                if let Some(model) = message.model.as_deref()
                    && tx_event
                        .send(Ok(ResponseEvent::ServerModel(model.to_owned())))
                        .await
                        .is_err()
                {
                    return;
                }
                if tx_event.send(Ok(ResponseEvent::Created)).await.is_err() {
                    return;
                }
            }

            MessageStreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                // Exhaustive match on the typed ContentBlock enum.
                // Drop arms log via tracing; the Unknown arm captures
                // the original tag + raw JSON for visibility.
                match content_block {
                    ContentBlock::Text { .. } => {
                        tracker.blocks.insert(
                            index,
                            BlockState::Text {
                                text: String::new(),
                            },
                        );
                        let item = ResponseItem::Message {
                            id: None,
                            role: "assistant".to_owned(),
                            content: vec![],
                            phase: None,
                        };
                        if tx_event
                            .send(Ok(ResponseEvent::OutputItemAdded(item)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    ContentBlock::Thinking { .. } => {
                        tracker.blocks.insert(
                            index,
                            BlockState::Thinking {
                                thinking: String::new(),
                                signature: String::new(),
                            },
                        );
                        let item = ResponseItem::Reasoning {
                            id: None,
                            summary: Vec::new(),
                            content: None,
                            encrypted_content: None,
                            internal_chat_message_metadata_passthrough: None,
                            raw_wire_block: None,
                        };
                        if tx_event
                            .send(Ok(ResponseEvent::OutputItemAdded(item)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    ContentBlock::ToolUse { id, name, .. } => {
                        tracker.blocks.insert(
                            index,
                            BlockState::ToolUse {
                                call_id: id.clone(),
                                name: name.clone(),
                                arguments: String::new(),
                            },
                        );
                        let (namespace, bare_name) = parse_flat_mcp_tool_name(&name);
                        let item = ResponseItem::FunctionCall {
                            id: None,
                            name: bare_name,
                            namespace,
                            arguments: String::new(),
                            call_id: id.clone(),
                        };
                        if tx_event
                            .send(Ok(ResponseEvent::OutputItemAdded(item)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    ContentBlock::RedactedThinking { data } => {
                        tracker
                            .blocks
                            .insert(index, BlockState::RedactedThinking { data });
                    }
                    ContentBlock::ServerToolUse { id, name, input } => {
                        if name == "web_search" {
                            let arguments = if input.is_object() && input.as_object().is_some_and(|o| o.is_empty()) {
                                String::new()
                            } else {
                                serde_json::to_string(&input).unwrap_or_default()
                            };
                            tracker.blocks.insert(
                                index,
                                BlockState::ServerToolUse {
                                    id: id.clone(),
                                    name: name.clone(),
                                    arguments,
                                },
                            );
                            let item = ResponseItem::WebSearchCall {
                                id: Some(id.clone()),
                                status: Some("in_progress".to_string()),
                                action: None,
                            };
                            if tx_event
                                .send(Ok(ResponseEvent::OutputItemAdded(item)))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        } else {
                            trace!(
                                "intentional drop: server_tool_use name={name} (wire_vocab::CONTENT_BLOCKS)"
                            );
                        }
                    }
                    ContentBlock::WebSearchToolResult { tool_use_id, .. } => {
                        tracker.blocks.insert(
                            index,
                            BlockState::WebSearchToolResult {
                                tool_use_id: tool_use_id.clone(),
                            },
                        );
                    }
                    ContentBlock::CodeExecutionToolUse { .. } => {
                        // Documented intentional drop — policy lives in
                        // `wire_vocab::CONTENT_BLOCKS` (server-side beta).
                        trace!(
                            "intentional drop: code_execution_tool_use (wire_vocab::CONTENT_BLOCKS)"
                        );
                    }
                    ContentBlock::Unknown { tag, raw } => {
                        warn!(
                            block_type = %tag,
                            raw = ?raw,
                            "Anthropic content_block.type is NOT in known wire vocabulary \
                             — routed to ContentBlock::Unknown. Add a variant in \
                             messages_wire_types.rs and a parser arm here, or document a \
                             drop policy. See cli-ops/sortie-board/xli-v3/.",
                        );
                    }
                }
            }

            MessageStreamEvent::ContentBlockDelta { index, delta } => {
                // Exhaustive match on the typed ContentBlockDelta enum.
                match delta {
                    ContentBlockDelta::TextDelta { text } => {
                        if let Some(BlockState::Text { text: acc, .. }) =
                            tracker.blocks.get_mut(&index)
                        {
                            acc.push_str(&text);
                            if tx_event
                                .send(Ok(ResponseEvent::OutputTextDelta(text)))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        } else {
                            trace!("text_delta for untracked block index {index}, ignoring");
                        }
                    }
                    ContentBlockDelta::ThinkingDelta { thinking } => {
                        if let Some(BlockState::Thinking { thinking: acc, .. }) =
                            tracker.blocks.get_mut(&index)
                        {
                            acc.push_str(&thinking);
                            if tx_event
                                .send(Ok(ResponseEvent::ReasoningContentDelta {
                                    delta: thinking,
                                    content_index: index as i64,
                                }))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        } else {
                            trace!("thinking_delta for untracked block index {index}, ignoring");
                        }
                    }
                    ContentBlockDelta::SignatureDelta { signature } => {
                        if let Some(BlockState::Thinking { signature: acc, .. }) =
                            tracker.blocks.get_mut(&index)
                        {
                            acc.push_str(&signature);
                        }
                    }
                    ContentBlockDelta::InputJsonDelta { partial_json } => {
                        if let Some(BlockState::ToolUse { call_id, arguments: acc, .. }) =
                            tracker.blocks.get_mut(&index)
                        {
                            acc.push_str(&partial_json);
                            if tx_event
                                .send(Ok(ResponseEvent::ToolCallInputDelta {
                                    item_id: call_id.clone(),
                                    call_id: Some(call_id.clone()),
                                    delta: partial_json,
                                }))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        } else if let Some(BlockState::ServerToolUse { arguments: acc, .. }) =
                            tracker.blocks.get_mut(&index)
                        {
                            acc.push_str(&partial_json);
                        }
                    }
                    ContentBlockDelta::CitationsDelta { .. } => {
                        // Documented intentional drop — policy lives in
                        // `wire_vocab::CONTENT_BLOCK_DELTAS`.
                        trace!(
                            "intentional drop: citations_delta (wire_vocab::CONTENT_BLOCK_DELTAS)"
                        );
                    }
                    ContentBlockDelta::Unknown { tag, raw } => {
                        warn!(
                            delta_type = %tag,
                            raw = ?raw,
                            "Anthropic delta.type is NOT in known wire vocabulary \
                             — routed to ContentBlockDelta::Unknown. Add a variant in \
                             messages_wire_types.rs and a parser arm here, or document a \
                             drop policy. See cli-ops/sortie-board/xli-v3/.",
                        );
                    }
                }
            }

            MessageStreamEvent::ContentBlockStop { index } => {
                match tracker.blocks.remove(&index) {
                    Some(BlockState::ServerToolUse {
                        id,
                        name,
                        arguments,
                    }) if name == "web_search" => {
                        let item = ResponseItem::WebSearchCall {
                            id: Some(id),
                            status: Some("completed".to_string()),
                            action: web_search_action_from_arguments(&arguments),
                        };
                        if tx_event
                            .send(Ok(ResponseEvent::OutputItemDone(item)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Some(BlockState::ServerToolUse { name, .. }) => {
                        trace!(
                            "intentional drop on stop: server_tool_use name={name} (wire_vocab::CONTENT_BLOCKS)"
                        );
                    }
                    Some(BlockState::WebSearchToolResult { .. }) => {
                        // Result block follows server_tool_use; IR emitted on server_tool_use stop.
                    }
                    Some(BlockState::ToolUse {
                        call_id,
                        name,
                        arguments,
                    }) => {
                        // S-004: Detect truncated tool call arguments.
                        // If the provider silently truncated output, the JSON
                        // will be incomplete. Flag it so message_stop can
                        // override stop_reason to "max_tokens" for retry.
                        if !arguments.is_empty()
                            && serde_json::from_str::<serde_json::Value>(&arguments).is_err()
                        {
                            warn!(
                                call_id = %call_id,
                                name = %name,
                                args_len = arguments.len(),
                                "truncated tool_use arguments detected (invalid JSON)"
                            );
                            tool_use_truncated = true;
                        }
                        let (namespace, bare_name) = parse_flat_mcp_tool_name(&name);
                        let item = ResponseItem::FunctionCall {
                            id: None,
                            name: bare_name,
                            namespace,
                            arguments,
                            call_id,
                        };
                        if tx_event
                            .send(Ok(ResponseEvent::OutputItemDone(item)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Some(BlockState::Text { text }) => {
                        let item = ResponseItem::Message {
                            id: None,
                            role: "assistant".to_owned(),
                            content: vec![ContentItem::OutputText { text }],
                            phase: None,
                        };
                        if tx_event
                            .send(Ok(ResponseEvent::OutputItemDone(item)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Some(BlockState::Thinking {
                        thinking,
                        signature,
                    }) => {
                        // Build the raw wire block for byte-identical replay.
                        // This is the exact JSON block Anthropic expects when
                        // the conversation history is sent back.
                        // S-OPUS47-EMPTY-THINKING: drop signed-but-empty
                        // thinking blocks. Opus 4.7 adaptive thinking
                        // (display=summarized/omitted), notably on the
                        // Vertex route, streams a signature_delta but
                        // withholds every thinking_delta. Persisting a
                        // raw_wire_block of shape
                        //   { type: "thinking", thinking: "", signature: <real> }
                        // and replaying it on the next turn fails Anthropic's
                        // verifier with `messages.N.content.M: thinking
                        // blocks in the latest assistant message cannot be
                        // modified` because the signature was computed over
                        // content the proxy never returned. Anthropic
                        // regenerates the thought on the next turn, so
                        // replay is not required for correctness.
                        if thinking.is_empty() {
                            if !signature.is_empty() {
                                tracing::debug!(
                                    "dropping empty-text signed thinking block (opus-4.7 adaptive/summarized; signature would fail verifier on replay)"
                                );
                            }
                            continue;
                        }
                        let raw_block = if signature.is_empty() {
                            serde_json::json!({
                                "type": "thinking",
                                "thinking": &thinking,
                            })
                        } else {
                            serde_json::json!({
                                "type": "thinking",
                                "thinking": &thinking,
                                "signature": &signature,
                            })
                        };
                        let item = ResponseItem::Reasoning {
                            id: None,
                            summary: vec![
                                codex_protocol::models::ReasoningItemReasoningSummary::SummaryText {
                                    text: thinking,
                                },
                            ],
                            content: None,
                            encrypted_content: if signature.is_empty() {
                                None
                            } else {
                                Some(signature)
                            },
                            internal_chat_message_metadata_passthrough: None,
                            raw_wire_block: Some(raw_block),
                        };
                        if tx_event
                            .send(Ok(ResponseEvent::OutputItemDone(item)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Some(BlockState::RedactedThinking { data }) => {
                        // Build raw wire block for byte-identical replay.
                        let raw_block = serde_json::json!({
                            "type": "redacted_thinking",
                            "data": &data,
                        });
                        // Sentinel prefix "\0REDACTED\0" distinguishes redacted thinking
                        // from real Anthropic signatures (which are base64 and cannot
                        // contain null bytes). Consumed by messages_wire.rs translator.
                        let item = ResponseItem::Reasoning {
                            id: None,
                            summary: Vec::new(),
                            content: None,
                            encrypted_content: Some(format!("\0REDACTED\0{data}")),
                            internal_chat_message_metadata_passthrough: None,
                            raw_wire_block: Some(raw_block),
                        };
                        if tx_event
                            .send(Ok(ResponseEvent::OutputItemDone(item)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    None => {}
                }
            }

            MessageStreamEvent::MessageDelta { delta, usage } => {
                if let Some(reason) = delta.stop_reason.as_ref() {
                    let reason_str = reason.as_wire_str();
                    trace!("stop_reason: {reason_str}");
                    stop_reason = Some(reason_str.to_owned());
                }
                if let Some(usage_val) = usage
                    && let Ok(u) = serde_json::from_value::<AnthropicUsage>(usage_val)
                {
                    usage_holder = Some(merge_usage(usage_holder, u));
                }
            }

            MessageStreamEvent::MessageStop => {
                // S-004: Check for any tool_use blocks still in the tracker
                // (blocks that never received content_block_stop — hard truncation).
                for state in tracker.blocks.values() {
                    if let BlockState::ToolUse {
                        call_id,
                        name,
                        arguments,
                    } = state
                        && !arguments.is_empty()
                        && serde_json::from_str::<serde_json::Value>(arguments).is_err()
                    {
                        warn!(
                            call_id = %call_id,
                            name = %name,
                            args_len = arguments.len(),
                            "in-flight tool_use block has truncated arguments at message_stop"
                        );
                        tool_use_truncated = true;
                    }
                }

                // S-004: If any tool_use block had invalid JSON arguments,
                // override stop_reason to "max_tokens" so the harness retries.
                if tool_use_truncated {
                    warn!(
                        "overriding stop_reason to max_tokens due to truncated tool_use arguments"
                    );
                    stop_reason = Some("max_tokens".to_owned());
                }

                let token_usage = usage_holder.map(|u| normalize_token_usage(u.to_raw_usage()));
                let end_turn = if stop_reason.as_deref() == Some("end_turn") {
                    Some(true)
                } else {
                    None
                };
                let _ = tx_event
                    .send(Ok(ResponseEvent::Completed {
                        end_turn,
                        stop_reason: stop_reason.take(),
                        response_id: response_id.clone(),
                        token_usage,
                    }))
                    .await;
                return;
            }

            MessageStreamEvent::Ping => {}

            MessageStreamEvent::Error { error } => {
                let message = error
                    .message
                    .as_deref()
                    .unwrap_or("unknown anthropic error");
                let error_type = error.error_type.as_deref().unwrap_or("");

                // S-ERROR-TYPED: classify into a typed kind (rung-3 pattern) so
                // the mapping is exhaustive and unknown error types are visible
                // (warn!) rather than silently collapsing to a generic error.
                let api_error = match AnthropicErrorKind::from_wire(error_type) {
                    AnthropicErrorKind::Overloaded => ApiError::ServerOverloaded,
                    AnthropicErrorKind::RateLimit => ApiError::RateLimit(message.to_owned()),
                    AnthropicErrorKind::Unknown(raw) => {
                        warn!(
                            error_type = %raw,
                            "unmodeled Anthropic error.type; mapping to generic stream error"
                        );
                        ApiError::Stream(format!("Anthropic API error: {message}"))
                    }
                };

                let _ = tx_event.send(Err(api_error)).await;
                return;
            }

            MessageStreamEvent::Unknown { tag, raw } => {
                // The custom Deserialize already logged a warn! with the tag.
                // Re-log here with raw payload so the parser-side context shows
                // up in a single grep alongside the Deserialize-side warn.
                warn!(
                    wire_type = %tag,
                    raw = ?raw,
                    "unhandled Anthropic SSE event in parser; dropping",
                );
            }
        }
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
struct AnthropicOutputTokensDetails {
    thinking_tokens: Option<i64>,
}

/// TTL-band cache write breakdown (`Usage.cache_creation` on the wire).
#[derive(Debug, Deserialize, Default, Clone)]
struct AnthropicCacheCreation {
    ephemeral_5m_input_tokens: Option<i64>,
    ephemeral_1h_input_tokens: Option<i64>,
}

/// Server-side tool request counters on the usage object (deserialize-only for
/// now — projection to protocol `TokenUsage` is S-SERVER-TOOL-USAGE).
#[derive(Debug, Deserialize, Default, Clone)]
struct AnthropicServerToolUsage {
    web_search_requests: Option<i64>,
    web_fetch_requests: Option<i64>,
}

#[derive(Debug, Deserialize, Default, Clone)]
struct AnthropicUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
    #[serde(default)]
    cache_creation: Option<AnthropicCacheCreation>,
    #[serde(default)]
    output_tokens_details: Option<AnthropicOutputTokensDetails>,
    #[serde(default)]
    server_tool_use: Option<AnthropicServerToolUsage>,
}

impl AnthropicUsage {
    fn cache_creation_tokens(&self) -> i64 {
        if let Some(n) = self.cache_creation_input_tokens {
            return n.max(0);
        }
        self.cache_creation
            .as_ref()
            .map(|c| {
                c.ephemeral_5m_input_tokens.unwrap_or(0).max(0)
                    + c.ephemeral_1h_input_tokens.unwrap_or(0).max(0)
            })
            .unwrap_or(0)
    }

    fn reasoning_output_tokens(&self) -> i64 {
        self.output_tokens_details
            .as_ref()
            .and_then(|d| d.thinking_tokens)
            .unwrap_or(0)
            .max(0)
    }

    fn to_raw_usage(&self) -> RawUsage {
        let server = self.server_tool_use.as_ref();
        RawUsage {
            web_search_requests: server
                .and_then(|s| s.web_search_requests)
                .unwrap_or(0)
                .max(0),
            web_fetch_requests: server
                .and_then(|s| s.web_fetch_requests)
                .unwrap_or(0)
                .max(0),
            ..RawUsage::cache_exclusive_prompt(
                self.input_tokens.unwrap_or(0),
                self.cache_read_input_tokens.unwrap_or(0),
                self.cache_creation_tokens(),
                self.output_tokens.unwrap_or(0),
                self.reasoning_output_tokens(),
            )
        }
    }
}

fn web_search_action_from_arguments(arguments: &str) -> Option<WebSearchAction> {
    let value: serde_json::Value = serde_json::from_str(arguments).ok()?;
    let query = value
        .get("query")
        .and_then(|q| q.as_str())
        .map(str::to_owned);
    if query.is_none() {
        return None;
    }
    Some(WebSearchAction::Search {
        query,
        queries: None,
    })
}

fn merge_usage(existing: Option<AnthropicUsage>, new: AnthropicUsage) -> AnthropicUsage {
    match existing {
        None => new,
        Some(prev) => AnthropicUsage {
            input_tokens: new.input_tokens.or(prev.input_tokens),
            output_tokens: new.output_tokens.or(prev.output_tokens),
            cache_read_input_tokens: new.cache_read_input_tokens.or(prev.cache_read_input_tokens),
            cache_creation_input_tokens: new
                .cache_creation_input_tokens
                .or(prev.cache_creation_input_tokens),
            cache_creation: merge_cache_creation(prev.cache_creation, new.cache_creation),
            output_tokens_details: merge_output_tokens_details(
                prev.output_tokens_details,
                new.output_tokens_details,
            ),
            server_tool_use: merge_server_tool_use(prev.server_tool_use, new.server_tool_use),
        },
    }
}

fn merge_cache_creation(
    prev: Option<AnthropicCacheCreation>,
    new: Option<AnthropicCacheCreation>,
) -> Option<AnthropicCacheCreation> {
    match (prev, new) {
        (None, n) => n,
        (Some(p), None) => Some(p),
        (Some(p), Some(n)) => Some(AnthropicCacheCreation {
            ephemeral_5m_input_tokens: n
                .ephemeral_5m_input_tokens
                .or(p.ephemeral_5m_input_tokens),
            ephemeral_1h_input_tokens: n
                .ephemeral_1h_input_tokens
                .or(p.ephemeral_1h_input_tokens),
        }),
    }
}

fn merge_output_tokens_details(
    prev: Option<AnthropicOutputTokensDetails>,
    new: Option<AnthropicOutputTokensDetails>,
) -> Option<AnthropicOutputTokensDetails> {
    match (prev, new) {
        (None, n) => n,
        (Some(p), None) => Some(p),
        (Some(p), Some(n)) => Some(AnthropicOutputTokensDetails {
            thinking_tokens: n.thinking_tokens.or(p.thinking_tokens),
        }),
    }
}

fn merge_server_tool_use(
    prev: Option<AnthropicServerToolUsage>,
    new: Option<AnthropicServerToolUsage>,
) -> Option<AnthropicServerToolUsage> {
    match (prev, new) {
        (None, n) => n,
        (Some(p), None) => Some(p),
        (Some(p), Some(n)) => Some(AnthropicServerToolUsage {
            web_search_requests: n.web_search_requests.or(p.web_search_requests),
            web_fetch_requests: n.web_fetch_requests.or(p.web_fetch_requests),
        }),
    }
}

#[cfg(test)]
mod anthropic_usage_wire_tests {
    use super::*;

    #[test]
    fn deserializes_cache_creation_ttl_breakdown() {
        let v = serde_json::json!({
            "input_tokens": 80,
            "output_tokens": 0,
            "cache_read_input_tokens": 10,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 15,
                "ephemeral_1h_input_tokens": 5
            }
        });
        let u: AnthropicUsage = serde_json::from_value(v).expect("usage json");
        let raw = u.to_raw_usage();
        assert_eq!(raw.cache_read, 10);
        assert_eq!(raw.cache_creation, 20);
        assert_eq!(raw.api_input, 80);
    }

    #[test]
    fn message_start_event_preserves_usage_with_cache_creation_ttl() {
        use crate::sse::messages_wire_types::MessageStreamEvent;
        let line = serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_ttl",
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": "claude-sonnet-4.6",
                "usage": {
                    "input_tokens": 80,
                    "output_tokens": 0,
                    "cache_read_input_tokens": 10,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 15,
                        "ephemeral_1h_input_tokens": 5
                    }
                }
            }
        });
        let event: MessageStreamEvent =
            serde_json::from_value(line).expect("message_start event");
        let MessageStreamEvent::MessageStart { message } = event else {
            panic!("expected message_start");
        };
        let usage_val = message.usage.expect("usage value");
        let u: AnthropicUsage =
            serde_json::from_value(usage_val).expect("anthropic usage");
        assert_eq!(u.cache_creation_tokens(), 20);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use tokio_util::io::ReaderStream;

    /// Rung-2/3 invariant: each `WirePolicy::Handled` entry in the
    /// curated `wire_vocab` tables must correspond to an actual variant
    /// in the typed enums *and* an arm in the parser.
    ///
    /// With rung-3 (S-WIRE-VOCAB-MAX-TEETH), the *enum exhaustiveness*
    /// is the load-bearing guarantee — `match content_block { ... }`
    /// will not compile if a `ContentBlock` variant is missing. This
    /// test is a belt-and-suspenders check that the documentation table
    /// is kept in sync with the typed-variant names.
    ///
    /// Mapping table:
    ///   wire-vocab tag (snake_case) → enum variant (CamelCase identifier
    ///   exactly as written in the parser match arm).
    #[test]
    fn wire_vocab_consistent_with_parser() {
        let source = include_str!("messages.rs");

        // Snake-case → CamelCase variant identifier the parser uses.
        let camel = |snake: &str| -> String {
            snake
                .split('_')
                .map(|part| {
                    let mut cs = part.chars();
                    match cs.next() {
                        Some(c) => c.to_uppercase().collect::<String>() + cs.as_str(),
                        None => String::new(),
                    }
                })
                .collect()
        };

        let check_handled =
            |entries: &[(&str, wire_vocab::WirePolicy)], enum_name: &str, context: &str| {
                for (tag, policy) in entries {
                    if !matches!(policy, wire_vocab::WirePolicy::Handled) {
                        continue;
                    }
                    let variant = camel(tag);
                    let pat = format!("{enum_name}::{variant}");
                    assert!(
                        source.contains(&pat),
                        "wire_vocab[{context}] entry `{tag}` is marked Handled but the \
                     parser has no `{pat}` arm in messages.rs.  Either:\n\
                       - add a parser arm for the typed variant, or\n\
                       - change the table entry to DropExplicit(\"<reason>\")",
                    );
                }
            };

        check_handled(
            wire_vocab::STREAM_EVENTS,
            "MessageStreamEvent",
            "STREAM_EVENTS",
        );
        check_handled(wire_vocab::CONTENT_BLOCKS, "ContentBlock", "CONTENT_BLOCKS");
        check_handled(
            wire_vocab::CONTENT_BLOCK_DELTAS,
            "ContentBlockDelta",
            "CONTENT_BLOCK_DELTAS",
        );
    }

    /// Drop entries must be NON-EMPTY rationales.  This stops a future
    /// "I'll just shut up the warn by adding it to the table" drive-by
    /// that erases institutional knowledge of WHY we drop a wire type.
    #[test]
    fn wire_vocab_drop_entries_have_rationale() {
        let all = [
            wire_vocab::STREAM_EVENTS,
            wire_vocab::CONTENT_BLOCKS,
            wire_vocab::CONTENT_BLOCK_DELTAS,
        ];
        for table in all.iter() {
            for (tag, policy) in *table {
                if let wire_vocab::WirePolicy::DropExplicit(reason) = policy {
                    assert!(
                        !reason.trim().is_empty(),
                        "wire_vocab `{tag}` is DropExplicit but rationale is empty"
                    );
                }
            }
        }
    }

    fn fixture_to_byte_stream(lines: &[&str]) -> ByteStream {
        let mut content = String::new();
        for line in lines {
            content.push_str(line);
            content.push('\n');
        }
        let reader = std::io::Cursor::new(content);
        let stream = ReaderStream::new(reader)
            .map(|r| r.map_err(|e| codex_client::TransportError::Network(e.to_string())));
        Box::pin(stream)
    }

    #[tokio::test]
    async fn test_basic_text_response() {
        let fixture = vec![
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_123\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":0}",
            "",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}",
            "",
            "data: {\"type\":\"message_stop\"}",
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        assert!(events.len() >= 4);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Ok(ResponseEvent::ServerModel(_)))),
            "must emit ServerModel from message_start"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Ok(ResponseEvent::Created))),
            "must emit Created"
        );

        let mut found_text_delta = false;
        let mut found_output_item_done = false;
        let mut found_completed = false;
        for event in &events {
            match event {
                Ok(ResponseEvent::OutputTextDelta(t)) if t == "Hello" => {
                    found_text_delta = true;
                }
                Ok(ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. })) => {
                    if let Some(ContentItem::OutputText { text }) = content.first() {
                        assert_eq!(text, "Hello world");
                    }
                    found_output_item_done = true;
                }
                Ok(ResponseEvent::Completed {
                    response_id,
                    token_usage,
                    ..
                }) => {
                    assert_eq!(response_id, "msg_123");
                    assert!(token_usage.is_some());
                    found_completed = true;
                }
                _ => {}
            }
        }
        assert!(found_text_delta);
        assert!(found_output_item_done);
        assert!(found_completed);
    }

    #[tokio::test]
    async fn test_tool_use_response() {
        let fixture = vec![
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_456\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":20,\"output_tokens\":0}}}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01\",\"name\":\"shell\",\"input\":{}}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\"\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\": \\\"ls\\\"}\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":0}",
            "",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":15}}",
            "",
            "data: {\"type\":\"message_stop\"}",
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        let mut found_tool_call = false;
        for event in &events {
            if let Ok(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            })) = event
            {
                assert_eq!(call_id, "toolu_01");
                assert_eq!(name, "shell");
                assert_eq!(arguments, "{\"command\": \"ls\"}");
                found_tool_call = true;
            }
        }
        assert!(found_tool_call);
    }

    #[tokio::test]
    async fn test_error_event_extracted() {
        let fixture = vec![
            "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}",
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut found_error = false;
        while let Some(event) = rx.recv().await {
            if let Err(ApiError::ServerOverloaded) = event {
                found_error = true;
            }
        }
        assert!(found_error);
    }

    #[tokio::test]
    async fn test_rate_limit_error() {
        let fixture = vec![
            "data: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"Rate limited\"}}",
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut found_error = false;
        while let Some(event) = rx.recv().await {
            if let Err(ApiError::RateLimit(msg)) = event {
                assert_eq!(msg, "Rate limited");
                found_error = true;
            }
        }
        assert!(found_error);
    }

    #[tokio::test]
    async fn test_unknown_error_type_maps_to_generic_stream() {
        // S-ERROR-TYPED: an unmodeled Anthropic error.type must surface as a
        // generic stream error (safe default) rather than be dropped.
        let fixture = vec![
            r#"data: {"type":"error","error":{"type":"api_error","message":"Internal"}}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut found_error = false;
        while let Some(event) = rx.recv().await {
            if let Err(ApiError::Stream(msg)) = event {
                assert!(msg.contains("Internal"), "message should be preserved");
                found_error = true;
            }
        }
        assert!(found_error);
    }

    #[tokio::test]
    async fn test_thinking_block_emits_reasoning_with_signature() {
        let fixture = vec![
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_789\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Let me think\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\" about this\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig_abc123\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":0}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Here is my answer\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":1}",
            "",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":20}}",
            "",
            "data: {\"type\":\"message_stop\"}",
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        let mut found_reasoning = false;
        let mut found_text = false;
        let mut found_thinking_delta = false;
        for event in &events {
            match event {
                Ok(ResponseEvent::ReasoningContentDelta { delta, .. })
                    if delta.contains("Let me think") =>
                {
                    found_thinking_delta = true;
                }
                Ok(ResponseEvent::OutputItemDone(ResponseItem::Reasoning {
                    summary,
                    encrypted_content,
                    ..
                })) => {
                    assert!(!summary.is_empty());
                    assert_eq!(
                        encrypted_content.as_deref(),
                        Some("sig_abc123"),
                        "signature must be preserved"
                    );
                    found_reasoning = true;
                }
                Ok(ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. })) => {
                    if let Some(ContentItem::OutputText { text }) = content.first() {
                        assert_eq!(text, "Here is my answer");
                        found_text = true;
                    }
                }
                _ => {}
            }
        }
        assert!(found_thinking_delta, "should emit thinking deltas");
        assert!(found_reasoning, "should emit Reasoning item on block stop");
        assert!(found_text, "should emit text block");
    }

    #[tokio::test]
    async fn test_redacted_thinking_block() {
        let fixture = vec![
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_red\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"opaque_encrypted_data_xyz\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":0}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Done\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":1}",
            "",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}",
            "",
            "data: {\"type\":\"message_stop\"}",
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        let mut found_redacted = false;
        for event in &events {
            if let Ok(ResponseEvent::OutputItemDone(ResponseItem::Reasoning {
                encrypted_content,
                ..
            })) = event
                && let Some(ec) = encrypted_content
                && ec.starts_with("\0REDACTED\0")
            {
                assert_eq!(ec, "\0REDACTED\0opaque_encrypted_data_xyz");
                found_redacted = true;
            }
        }
        assert!(found_redacted, "should emit redacted thinking as Reasoning");
    }

    #[tokio::test]
    async fn test_interleaved_text_and_tool_use() {
        let fixture = vec![
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_multi\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":30,\"output_tokens\":0}}}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me check\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":0}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_a\",\"name\":\"read_file\",\"input\":{}}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\": \\\"main.rs\\\"}\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":1}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_b\",\"name\":\"shell\",\"input\":{}}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\": \\\"ls\\\"}\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":2}",
            "",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":40}}",
            "",
            "data: {\"type\":\"message_stop\"}",
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        let mut tool_calls = Vec::new();
        let mut found_text = false;
        for event in &events {
            match event {
                Ok(ResponseEvent::OutputItemDone(ResponseItem::Message { .. })) => {
                    found_text = true;
                }
                Ok(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                    call_id,
                    name,
                    ..
                })) => {
                    tool_calls.push((call_id.clone(), name.clone()));
                }
                _ => {}
            }
        }
        assert!(found_text, "should emit text block before tools");
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(
            tool_calls[0],
            ("toolu_a".to_string(), "read_file".to_string())
        );
        assert_eq!(tool_calls[1], ("toolu_b".to_string(), "shell".to_string()));
    }

    #[tokio::test]
    async fn test_usage_token_tracking() {
        let fixture = vec![
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_tok\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":100,\"output_tokens\":0,\"cache_read_input_tokens\":50,\"cache_creation_input_tokens\":25}}}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":0}",
            "",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42}}",
            "",
            "data: {\"type\":\"message_stop\"}",
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        let mut found_usage = false;
        for event in &events {
            if let Ok(ResponseEvent::Completed { token_usage, .. }) = event {
                let usage = token_usage.as_ref().expect("usage should be present");
                // input normalized to cache-inclusive: api 100 + cache_read 50
                // + cache_creation 25 = 175 (S-USAGE-TYPED / F1).
                assert_eq!(usage.input_tokens, 175);
                assert_eq!(usage.output_tokens, 42);
                assert_eq!(usage.cached_input_tokens, 50);
                assert_eq!(usage.cache_creation_input_tokens, 25);
                // total = input + output = 175 + 42 = 217.
                assert_eq!(usage.total_tokens, 217);
                found_usage = true;
            }
        }
        assert!(
            found_usage,
            "should track token usage across message events"
        );
    }

    #[tokio::test]
    async fn test_ping_events_ignored() {
        let fixture = vec![
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_ping\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}",
            "",
            "data: {\"type\":\"ping\"}",
            "",
            "data: {\"type\":\"ping\"}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":0}",
            "",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}",
            "",
            "data: {\"type\":\"message_stop\"}",
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        assert!(
            events
                .iter()
                .any(|e| matches!(e, Ok(ResponseEvent::Completed { .. }))),
            "should complete despite ping events"
        );
    }

    #[tokio::test]
    async fn test_unknown_event_types_ignored() {
        let fixture = vec![
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_unk\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}",
            "",
            "data: {\"type\":\"some_future_event\",\"data\":{\"foo\":\"bar\"}}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":0}",
            "",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}",
            "",
            "data: {\"type\":\"message_stop\"}",
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        assert!(
            events
                .iter()
                .any(|e| matches!(e, Ok(ResponseEvent::Completed { .. }))),
            "should complete despite unknown events"
        );
    }

    /// Rung-3 (S-WIRE-VOCAB-MAX-TEETH): an unknown SSE event type must
    /// parse into `MessageStreamEvent::Unknown` with the original tag
    /// and raw JSON preserved — NOT silently dropped via a wildcard arm.
    #[test]
    fn unknown_sse_event_routes_to_typed_unknown() {
        let json = r#"{"type":"future_stream_event","extra":{"x":1}}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        match event {
            MessageStreamEvent::Unknown { tag, raw } => {
                assert_eq!(tag, "future_stream_event");
                assert_eq!(raw["extra"]["x"], 1);
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    /// Rung-3: unknown content_block.type routes to ContentBlock::Unknown,
    /// preserving the tag for visibility.
    #[test]
    fn unknown_content_block_routes_to_typed_unknown() {
        let json = r#"{"type":"future_block","id":"x","payload":42}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        match block {
            ContentBlock::Unknown { tag, raw } => {
                assert_eq!(tag, "future_block");
                assert_eq!(raw["payload"], 42);
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    /// Rung-3: unknown delta.type routes to ContentBlockDelta::Unknown.
    #[test]
    fn unknown_content_block_delta_routes_to_typed_unknown() {
        let json = r#"{"type":"future_delta","payload":"x"}"#;
        let delta: ContentBlockDelta = serde_json::from_str(json).unwrap();
        match delta {
            ContentBlockDelta::Unknown { tag, .. } => {
                assert_eq!(tag, "future_delta");
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_output_item_added_precedes_text_deltas() {
        let fixture = vec![
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_oia\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":0}",
            "",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}",
            "",
            "data: {\"type\":\"message_stop\"}",
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        let ok_events: Vec<_> = events.iter().filter_map(|e| e.as_ref().ok()).collect();

        let added_idx = ok_events
            .iter()
            .position(|e| {
                matches!(
                    e,
                    ResponseEvent::OutputItemAdded(ResponseItem::Message { .. })
                )
            })
            .expect("must emit OutputItemAdded(Message)");
        let delta_idx = ok_events
            .iter()
            .position(|e| matches!(e, ResponseEvent::OutputTextDelta(_)))
            .expect("must emit OutputTextDelta");
        assert!(
            added_idx < delta_idx,
            "OutputItemAdded must precede OutputTextDelta"
        );
    }

    #[tokio::test]
    async fn test_thinking_output_item_added_precedes_reasoning_delta() {
        let fixture = vec![
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_toia\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig1\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":0}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":1}",
            "",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}",
            "",
            "data: {\"type\":\"message_stop\"}",
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        let ok_events: Vec<_> = events.iter().filter_map(|e| e.as_ref().ok()).collect();

        let added_idx = ok_events
            .iter()
            .position(|e| {
                matches!(
                    e,
                    ResponseEvent::OutputItemAdded(ResponseItem::Reasoning { .. })
                )
            })
            .expect("must emit OutputItemAdded(Reasoning)");
        let delta_idx = ok_events
            .iter()
            .position(|e| matches!(e, ResponseEvent::ReasoningContentDelta { .. }))
            .expect("must emit ReasoningContentDelta");
        assert!(
            added_idx < delta_idx,
            "OutputItemAdded(Reasoning) must precede ReasoningContentDelta"
        );
    }

    #[tokio::test]
    async fn test_tool_use_output_item_added() {
        let fixture = vec![
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_tuoia\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_oia\",\"name\":\"shell\",\"input\":{}}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\": \\\"ls\\\"}\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":0}",
            "",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":10}}",
            "",
            "data: {\"type\":\"message_stop\"}",
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        let ok_events: Vec<_> = events.iter().filter_map(|e| e.as_ref().ok()).collect();

        let added = ok_events.iter().find(|e| {
            matches!(
                e,
                ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall { name, .. })
                if name == "shell"
            )
        });
        assert!(
            added.is_some(),
            "must emit OutputItemAdded(FunctionCall) for tool_use blocks"
        );

        let added_idx = ok_events
            .iter()
            .position(|e| {
                matches!(
                    e,
                    ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall { .. })
                )
            })
            .unwrap();
        let done_idx = ok_events
            .iter()
            .position(|e| {
                matches!(
                    e,
                    ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { .. })
                )
            })
            .expect("must emit OutputItemDone");
        assert!(
            added_idx < done_idx,
            "OutputItemAdded must precede OutputItemDone"
        );
    }

    #[tokio::test]
    async fn test_usage_includes_cache_read_in_total() {
        let fixture = vec![
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_cache\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.6\",\"usage\":{\"input_tokens\":100,\"output_tokens\":0,\"cache_read_input_tokens\":50,\"cache_creation_input_tokens\":25}}}",
            "",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
            "",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}",
            "",
            "data: {\"type\":\"content_block_stop\",\"index\":0}",
            "",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":10}}",
            "",
            "data: {\"type\":\"message_stop\"}",
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        for event in &events {
            if let Ok(ResponseEvent::Completed { token_usage, .. }) = event {
                let usage = token_usage.as_ref().expect("should have token usage");
                // input normalized to cache-inclusive: api 100 + cache_read 50
                // + cache_creation 25 = 175 (S-USAGE-TYPED / F1).
                assert_eq!(usage.input_tokens, 175);
                assert_eq!(usage.cached_input_tokens, 50);
                assert_eq!(usage.cache_creation_input_tokens, 25);
                assert_eq!(usage.output_tokens, 10);
                // total = input + output = 175 + 10 = 185 (cache folded into input).
                assert_eq!(
                    usage.total_tokens, 185,
                    "total must include cache_read and cache_creation via input"
                );
                return;
            }
        }
        panic!("did not find Completed event");
    }

    #[tokio::test]
    async fn test_usage_projects_thinking_tokens_from_output_tokens_details() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_think","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":50,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":120,"output_tokens_details":{"thinking_tokens":45}}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        while let Some(event) = rx.recv().await {
            if let Ok(ResponseEvent::Completed { token_usage, .. }) = event {
                let usage = token_usage.expect("usage");
                assert_eq!(usage.input_tokens, 50);
                assert_eq!(usage.output_tokens, 120);
                assert_eq!(usage.reasoning_output_tokens, 45);
                assert_eq!(usage.total_tokens, 170);
                return;
            }
        }
        panic!("did not find Completed event");
    }

    #[tokio::test]
    async fn test_usage_cache_creation_ttl_breakdown_when_total_absent() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_ttl","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":80,"output_tokens":0,"cache_read_input_tokens":10,"cache_creation":{"ephemeral_5m_input_tokens":15,"ephemeral_1h_input_tokens":5}}}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        while let Some(event) = rx.recv().await {
            if let Ok(ResponseEvent::Completed { token_usage, .. }) = event {
                let usage = token_usage.expect("usage");
                // input = 80 + cache_read 10 + cache_creation (15+5) = 110
                assert_eq!(usage.input_tokens, 110);
                assert_eq!(usage.cached_input_tokens, 10);
                assert_eq!(usage.cache_creation_input_tokens, 20);
                assert_eq!(usage.output_tokens, 12);
                assert_eq!(usage.total_tokens, 122);
                return;
            }
        }
        panic!("did not find Completed event");
    }

    #[tokio::test]
    async fn test_usage_projects_server_tool_use_counters() {
        // Shape from refs/anthropic/anthropic-docs/messages-streaming.md
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_srv","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":0}}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":20,"server_tool_use":{"web_search_requests":1,"web_fetch_requests":0}}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        while let Some(event) = rx.recv().await {
            if let Ok(ResponseEvent::Completed { token_usage, .. }) = event {
                let usage = token_usage.expect("usage");
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 20);
                assert_eq!(usage.total_tokens, 120);
                assert_eq!(usage.web_search_requests, 1);
                assert_eq!(usage.web_fetch_requests, 0);
                return;
            }
        }
        panic!("did not find Completed event");
    }

    #[tokio::test]
    async fn test_stop_reason_propagated_to_completed() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_sr","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut found_stop_reason = false;
        while let Some(event) = rx.recv().await {
            if let Ok(ResponseEvent::Completed { stop_reason, .. }) = event {
                assert_eq!(
                    stop_reason.as_deref(),
                    Some("end_turn"),
                    "stop_reason must be propagated from message_delta"
                );
                found_stop_reason = true;
            }
        }
        assert!(found_stop_reason, "must find Completed with stop_reason");
    }

    #[tokio::test]
    async fn test_stop_reason_tool_use() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_tu","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_1","name":"read_file","input":{}}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"/tmp/test\"}"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":15}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut found = false;
        while let Some(event) = rx.recv().await {
            if let Ok(ResponseEvent::Completed { stop_reason, .. }) = event {
                assert_eq!(stop_reason.as_deref(), Some("tool_use"));
                found = true;
            }
        }
        assert!(found, "must find Completed with stop_reason=tool_use");
    }

    #[tokio::test]
    async fn test_stop_reason_max_tokens() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_mt","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Truncated"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":4096}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut found = false;
        while let Some(event) = rx.recv().await {
            if let Ok(ResponseEvent::Completed { stop_reason, .. }) = event {
                assert_eq!(stop_reason.as_deref(), Some("max_tokens"));
                found = true;
            }
        }
        assert!(found, "must find Completed with stop_reason=max_tokens");
    }

    #[tokio::test]
    async fn test_stop_reason_stop_sequence() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_ss","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Count 1 2 3"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"stop_sequence","stop_sequence":"STOP"},"usage":{"output_tokens":8}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(30));
        let mut rx = response_stream.rx_event;

        let mut found = false;
        while let Some(event) = rx.recv().await {
            if let Ok(ResponseEvent::Completed { stop_reason, .. }) = event {
                assert_eq!(stop_reason.as_deref(), Some("stop_sequence"));
                found = true;
            }
        }
        assert!(found, "must find Completed with stop_reason=stop_sequence");
    }

    // ── T-3-A: idle timeout fires ──────────────────────────────────────

    #[tokio::test]
    async fn test_incomplete_stream_emits_error() {
        // Stream that starts with message_start but ends without message_stop.
        // Should emit a stream-closed error (either timeout or premature close).
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_t","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":1,"output_tokens":0}}}"#,
            "",
            // No further events — stream ends prematurely
        ];
        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_millis(50));
        let mut rx = response_stream.rx_event;
        let mut got_error = false;
        while let Some(event) = rx.recv().await {
            if event.is_err() {
                got_error = true;
            }
        }
        assert!(got_error, "incomplete stream should emit an error");
    }

    // ── T-3-B: error event from API ────────────────────────────────────

    #[tokio::test]
    async fn test_error_event_overloaded_standalone() {
        // Error event without preceding message_start — should still propagate.
        let fixture = vec![
            r#"data: {"type":"error","error":{"type":"overloaded_error","message":"Anthropic API temporarily overloaded"}}"#,
            "",
        ];
        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(5));
        let mut rx = response_stream.rx_event;
        let mut found_error = false;
        while let Some(event) = rx.recv().await {
            if let Err(ApiError::ServerOverloaded) = event {
                found_error = true;
            }
        }
        assert!(
            found_error,
            "overloaded error should be propagated as ApiError::ServerOverloaded"
        );
    }

    // ── T-3-C: malformed JSON SSE data skipped ─────────────────────────

    #[tokio::test]
    async fn test_malformed_sse_data_skipped() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_m","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":1,"output_tokens":0}}}"#,
            "",
            "data: this is not json at all {{{{",
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"recovered"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];
        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(5));
        let mut rx = response_stream.rx_event;
        let mut completed = false;
        while let Some(event) = rx.recv().await {
            if matches!(event, Ok(ResponseEvent::Completed { .. })) {
                completed = true;
            }
        }
        assert!(
            completed,
            "stream should complete despite malformed intermediate events"
        );
    }

    // ── T-3-D: thinking + tool_use interleaved ─────────────────────────

    #[tokio::test]
    async fn test_thinking_then_tool_use_interleaved() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_it","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"I need a shell command"}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SIG="}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_it1","name":"shell","input":{}}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"cmd\": \"ls\"}"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":1}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":15}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];
        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(10));
        let mut rx = response_stream.rx_event;
        let mut events: Vec<Result<ResponseEvent, _>> = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        // Must have Reasoning OutputItemDone
        let has_reasoning = events.iter().any(|e| {
            matches!(
                e,
                Ok(ResponseEvent::OutputItemDone(
                    ResponseItem::Reasoning { .. }
                ))
            )
        });
        assert!(has_reasoning, "must emit Reasoning OutputItemDone");

        // Must have FunctionCall OutputItemDone with correct call_id
        let has_tool = events.iter().any(|e| {
            matches!(
                e,
                Ok(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { call_id, .. }))
                if call_id == "toolu_it1"
            )
        });
        assert!(
            has_tool,
            "must emit FunctionCall OutputItemDone with call_id=toolu_it1"
        );

        // Reasoning must precede tool in event order
        let reasoning_idx = events.iter().position(|e| {
            matches!(
                e,
                Ok(ResponseEvent::OutputItemDone(
                    ResponseItem::Reasoning { .. }
                ))
            )
        });
        let tool_idx = events.iter().position(|e| {
            matches!(
                e,
                Ok(ResponseEvent::OutputItemDone(
                    ResponseItem::FunctionCall { .. }
                ))
            )
        });
        assert!(
            reasoning_idx < tool_idx,
            "thinking must complete before tool_use"
        );
    }

    // ═══════════════════════════════════════════════════════════════════
    // S-004: Tool call truncation detection tests
    // ═══════════════════════════════════════════════════════════════════

    /// S-004-T1: Complete tool_use JSON → no truncation signal.
    /// stop_reason must remain "tool_use" (not overridden to "max_tokens").
    #[tokio::test]
    async fn test_s004_complete_tool_use_no_truncation() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_ok","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_ok","name":"shell","input":{}}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\": \"ls -la\"}"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":20}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(10));
        let mut rx = response_stream.rx_event;

        let mut found = false;
        while let Some(event) = rx.recv().await {
            if let Ok(ResponseEvent::Completed { stop_reason, .. }) = event {
                assert_eq!(
                    stop_reason.as_deref(),
                    Some("tool_use"),
                    "complete tool_use JSON must NOT override stop_reason"
                );
                found = true;
            }
        }
        assert!(found, "must emit Completed event");
    }

    /// S-004-T2: Incomplete tool_use JSON (truncated mid-object) → truncation detected.
    /// stop_reason must be overridden to "max_tokens".
    #[tokio::test]
    async fn test_s004_truncated_tool_use_signals_max_tokens() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_trunc","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_trunc","name":"shell","input":{}}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\": \"ls -la"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":20}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(10));
        let mut rx = response_stream.rx_event;

        let mut found = false;
        while let Some(event) = rx.recv().await {
            if let Ok(ResponseEvent::Completed { stop_reason, .. }) = event {
                assert_eq!(
                    stop_reason.as_deref(),
                    Some("max_tokens"),
                    "truncated tool_use JSON must override stop_reason to max_tokens"
                );
                found = true;
            }
        }
        assert!(found, "must emit Completed event");
    }

    /// S-004-T3: Multiple tool_use blocks, one truncated → truncation detected.
    /// Even if the first tool_use is valid, a second truncated one must trigger.
    #[tokio::test]
    async fn test_s004_multiple_tool_use_one_truncated() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_multi","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_good","name":"read_file","input":{}}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\": \"/tmp/a\"}"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_bad","name":"write_file","input":{}}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\": \"/tmp/b\", \"content\": \"hel"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":1}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":30}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(10));
        let mut rx = response_stream.rx_event;

        let mut found = false;
        while let Some(event) = rx.recv().await {
            if let Ok(ResponseEvent::Completed { stop_reason, .. }) = event {
                assert_eq!(
                    stop_reason.as_deref(),
                    Some("max_tokens"),
                    "one truncated tool_use among many must override stop_reason"
                );
                found = true;
            }
        }
        assert!(found, "must emit Completed event");
    }

    /// S-004-T4: Tool_use with empty arguments → handled gracefully (no truncation).
    /// Empty arguments are legitimate (tool with no params).
    #[tokio::test]
    async fn test_s004_empty_arguments_no_truncation() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_empty","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_empty","name":"get_status","input":{}}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(10));
        let mut rx = response_stream.rx_event;

        let mut found = false;
        while let Some(event) = rx.recv().await {
            if let Ok(ResponseEvent::Completed { stop_reason, .. }) = event {
                assert_eq!(
                    stop_reason.as_deref(),
                    Some("tool_use"),
                    "empty arguments must NOT be treated as truncation"
                );
                found = true;
            }
        }
        assert!(found, "must emit Completed event");
    }

    /// S-004-T5: Hard truncation — tool_use block never receives content_block_stop.
    /// The block stays in the tracker and must be caught at message_stop.
    #[tokio::test]
    async fn test_s004_in_flight_tool_use_at_message_stop() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_inflight","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_inflight","name":"shell","input":{}}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"cmd\":"}}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":10}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(10));
        let mut rx = response_stream.rx_event;

        let mut found = false;
        while let Some(event) = rx.recv().await {
            if let Ok(ResponseEvent::Completed { stop_reason, .. }) = event {
                assert_eq!(
                    stop_reason.as_deref(),
                    Some("max_tokens"),
                    "in-flight truncated tool_use must override stop_reason at message_stop"
                );
                found = true;
            }
        }
        assert!(found, "must emit Completed event");
    }

    /// S-004-T6: Tool_use with valid JSON arguments "{}" → no truncation.
    /// Minimal valid JSON object should pass validation.
    #[tokio::test]
    async fn test_s004_minimal_valid_json_no_truncation() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_minimal","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_min","name":"ping","input":{}}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(10));
        let mut rx = response_stream.rx_event;

        let mut found = false;
        while let Some(event) = rx.recv().await {
            if let Ok(ResponseEvent::Completed { stop_reason, .. }) = event {
                assert_eq!(
                    stop_reason.as_deref(),
                    Some("tool_use"),
                    "valid JSON {{}} must NOT trigger truncation"
                );
                found = true;
            }
        }
        assert!(found, "must emit Completed event");
    }

    // ═══════════════════════════════════════════════════════════════════
    // S-003: text_delta gating tests
    // ═══════════════════════════════════════════════════════════════════

    /// S-003-T1: text_delta with a valid tracked block → OutputTextDelta emitted.
    #[tokio::test]
    async fn test_s003_text_delta_tracked_block_emitted() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_s003a","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":5,"output_tokens":0}}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"tracked text"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(10));
        let mut rx = response_stream.rx_event;

        let mut found_delta = false;
        while let Some(event) = rx.recv().await {
            if let Ok(ResponseEvent::OutputTextDelta(t)) = &event {
                assert_eq!(t, "tracked text");
                found_delta = true;
            }
        }
        assert!(
            found_delta,
            "text_delta for a tracked Text block must emit OutputTextDelta"
        );
    }

    /// S-003-T2: text_delta with an untracked block index → NOT emitted.
    /// A content_block_delta referencing index 99 (never initialized via
    /// content_block_start) must be silently dropped.
    #[tokio::test]
    async fn test_s003_text_delta_untracked_block_ignored() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_s003b","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":5,"output_tokens":0}}}"#,
            "",
            // No content_block_start for index 99 — it is untracked.
            r#"data: {"type":"content_block_delta","index":99,"delta":{"type":"text_delta","text":"orphaned text"}}"#,
            "",
            // Add a valid text block so the stream completes normally.
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"valid"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(10));
        let mut rx = response_stream.rx_event;

        let mut text_deltas = Vec::new();
        while let Some(event) = rx.recv().await {
            if let Ok(ResponseEvent::OutputTextDelta(t)) = event {
                text_deltas.push(t);
            }
        }
        // Only the tracked block's delta should appear; the orphaned one must be dropped.
        assert_eq!(
            text_deltas,
            vec!["valid".to_string()],
            "text_delta for untracked block index must NOT emit OutputTextDelta"
        );
    }

    /// S-003-T3: thinking_delta and input_json_delta gating regression guard.
    /// Deltas referencing untracked block indices must not emit events.
    #[tokio::test]
    async fn test_s003_thinking_and_input_json_delta_gating() {
        let fixture = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_s003c","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4.6","usage":{"input_tokens":5,"output_tokens":0}}}"#,
            "",
            // thinking_delta for untracked index 50
            r#"data: {"type":"content_block_delta","index":50,"delta":{"type":"thinking_delta","thinking":"orphaned thinking"}}"#,
            "",
            // input_json_delta for untracked index 51
            r#"data: {"type":"content_block_delta","index":51,"delta":{"type":"input_json_delta","partial_json":"{\"orphan\": true}"}}"#,
            "",
            // Tracked thinking block at index 0
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"real thinking"}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig_test"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"done"}}"#,
            "",
            r#"data: {"type":"content_block_stop","index":1}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ];

        let stream = fixture_to_byte_stream(&fixture);
        let response_stream = spawn_messages_stream(stream, Duration::from_secs(10));
        let mut rx = response_stream.rx_event;

        let mut thinking_deltas = Vec::new();
        let mut text_deltas = Vec::new();
        while let Some(event) = rx.recv().await {
            match &event {
                Ok(ResponseEvent::ReasoningContentDelta { delta, .. }) => {
                    thinking_deltas.push(delta.clone());
                }
                Ok(ResponseEvent::OutputTextDelta(t)) => {
                    text_deltas.push(t.clone());
                }
                _ => {}
            }
        }
        // Only the tracked thinking block's delta should appear.
        assert_eq!(
            thinking_deltas,
            vec!["real thinking".to_string()],
            "thinking_delta for untracked block must NOT emit ReasoningContentDelta"
        );
        // Only the tracked text block's delta should appear.
        assert_eq!(
            text_deltas,
            vec!["done".to_string()],
            "text_delta for tracked block must emit, untracked must not"
        );
    }
}
