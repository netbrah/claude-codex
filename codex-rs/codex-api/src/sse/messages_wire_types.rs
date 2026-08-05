//! Typed wire vocabulary for the Anthropic `/messages` SSE protocol.
//!
//! Rung-3 of the harness-invariant ladder (S-WIRE-VOCAB-MAX-TEETH).
//!
//! Every Anthropic SSE event maps to a Rust enum variant. Unknown upstream
//! types land in an `Unknown { tag, raw }` variant rather than silently
//! falling through a `_ =>` arm in the parser. This makes "we forgot to
//! handle a wire type" a compile-time discovery, not a quarter-long
//! mystery (see commit `ae52f83511`, ToolSpec::Namespace silent drop).
//!
//! Source of truth for the wire vocabulary:
//!   https://docs.anthropic.com/en/api/messages-streaming

use serde::Deserialize;
use serde::Deserializer;

/// Top-level SSE event dispatcher.
#[derive(Debug)]
pub(crate) enum MessageStreamEvent {
    MessageStart {
        message: MessageStartPayload,
    },
    ContentBlockStart {
        index: u64,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        index: u64,
        delta: ContentBlockDelta,
    },
    ContentBlockStop {
        index: u64,
    },
    MessageDelta {
        delta: MessageDeltaPayload,
        usage: Option<serde_json::Value>,
    },
    MessageStop,
    Ping,
    Error {
        error: ErrorPayload,
    },
    /// Wire type not in our known vocabulary. Carries the original tag and
    /// raw JSON so the parser can `tracing::warn!` with full fidelity.
    Unknown {
        tag: String,
        #[allow(dead_code)] // surfaced via tests + tracing warn on the deserialize side
        raw: serde_json::Value,
    },
}

/// `content_block` payload from `content_block_start`.
#[derive(Debug)]
pub(crate) enum ContentBlock {
    Text {
        #[allow(dead_code)]
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[allow(dead_code)]
        input: serde_json::Value,
    },
    Thinking {
        #[allow(dead_code)]
        thinking: String,
    },
    RedactedThinking {
        data: String,
    },
    ServerToolUse {
        #[allow(dead_code)]
        id: String,
        #[allow(dead_code)]
        name: String,
        #[allow(dead_code)]
        input: serde_json::Value,
    },
    WebSearchToolResult {
        #[allow(dead_code)]
        tool_use_id: String,
        #[allow(dead_code)]
        content: serde_json::Value,
    },
    CodeExecutionToolUse {
        #[allow(dead_code)]
        id: String,
        #[allow(dead_code)]
        name: String,
        #[allow(dead_code)]
        input: serde_json::Value,
    },
    Unknown {
        tag: String,
        #[allow(dead_code)]
        raw: serde_json::Value,
    },
}

/// `delta` payload from `content_block_delta`.
#[derive(Debug)]
pub(crate) enum ContentBlockDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    SignatureDelta {
        signature: String,
    },
    CitationsDelta {
        #[allow(dead_code)]
        citation: serde_json::Value,
    },
    Unknown {
        tag: String,
        #[allow(dead_code)]
        raw: serde_json::Value,
    },
}

/// Anthropic stop reasons.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    PauseTurn,
    Refusal,
    /// Unknown stop reason received from upstream; treated as graceful continuation.
    #[serde(other)]
    Unknown,
}

impl StopReason {
    /// Convert to the canonical snake_case wire string used downstream.
    pub(crate) fn as_wire_str(&self) -> &'static str {
        match self {
            StopReason::EndTurn => "end_turn",
            StopReason::MaxTokens => "max_tokens",
            StopReason::StopSequence => "stop_sequence",
            StopReason::ToolUse => "tool_use",
            StopReason::PauseTurn => "pause_turn",
            StopReason::Refusal => "refusal",
            StopReason::Unknown => "unknown",
        }
    }
}

/// Anthropic streaming `error.type` values, typed so the harness retry/
/// classification logic switches on a variant instead of a raw `&str`.
///
/// Mirrors the rung-3 wire-vocab pattern (S-WIRE-VOCAB-MAX-TEETH): every known
/// upstream token is an explicit arm; anything new lands in `Unknown(String)`
/// carrying the raw token so the caller can `warn!` it (visible drift) rather
/// than silently collapsing to a generic error.
///
/// S-ERROR-TYPED note: this list is the *currently observed* Anthropic error
/// vocabulary. As new upstream sources/criteria are pulled into the wire fleet
/// (cli-ops refactor), re-audit `from_wire` against each source's error schema —
/// the golden net in v4 `S-XWIRE-EQUIV` is the standing guard for that drift.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AnthropicErrorKind {
    /// Server transiently overloaded; retryable.
    Overloaded,
    /// Rate limited; retryable with backoff.
    RateLimit,
    /// Any error type not yet modeled. Carries the raw wire token.
    Unknown(String),
}

impl AnthropicErrorKind {
    /// Classify a raw Anthropic `error.type` string into a typed kind.
    pub(crate) fn from_wire(error_type: &str) -> Self {
        match error_type {
            "overloaded_error" => AnthropicErrorKind::Overloaded,
            "rate_limit_error" => AnthropicErrorKind::RateLimit,
            other => AnthropicErrorKind::Unknown(other.to_owned()),
        }
    }
}

/// Payload of `message_start` events.
#[derive(Debug, Deserialize)]
pub(crate) struct MessageStartPayload {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) usage: Option<serde_json::Value>,
}

/// Payload of `message_delta.delta`.
#[derive(Debug, Deserialize, Default)]
pub(crate) struct MessageDeltaPayload {
    #[serde(default)]
    pub(crate) stop_reason: Option<StopReason>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) stop_sequence: Option<String>,
}

/// Payload of `error.error`.
#[derive(Debug, Deserialize, Default)]
pub(crate) struct ErrorPayload {
    #[serde(default, rename = "type")]
    pub(crate) error_type: Option<String>,
    #[serde(default)]
    pub(crate) message: Option<String>,
}

// ── Custom Deserialize impls ───────────────────────────────────────────
//
// Option B from the sortie spec: deserialize to `serde_json::Value` first,
// dispatch on the `type` tag, and route unknowns to `Unknown { tag, raw }`
// preserving the original JSON value. This costs one extra allocation per
// SSE event vs. `#[serde(tag = ...)]` but gives a named, inspectable
// fallback rather than a silent drop.

#[derive(Deserialize)]
struct MessageStartInner {
    message: MessageStartPayload,
}

#[derive(Deserialize)]
struct ContentBlockStartInner {
    index: u64,
    content_block: ContentBlock,
}

#[derive(Deserialize)]
struct ContentBlockDeltaInner {
    index: u64,
    delta: ContentBlockDelta,
}

#[derive(Deserialize)]
struct ContentBlockStopInner {
    index: u64,
}

#[derive(Deserialize)]
struct MessageDeltaInner {
    #[serde(default)]
    delta: MessageDeltaPayload,
    #[serde(default)]
    usage: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ErrorInner {
    #[serde(default)]
    error: ErrorPayload,
}

impl<'de> Deserialize<'de> for MessageStreamEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let tag = raw
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("<missing>")
            .to_owned();

        match tag.as_str() {
            "message_start" => match serde_json::from_value::<MessageStartInner>(raw.clone()) {
                Ok(inner) => Ok(MessageStreamEvent::MessageStart {
                    message: inner.message,
                }),
                Err(e) => {
                    tracing::warn!(
                        wire_type = %tag,
                        error = %e,
                        "known Anthropic SSE tag with malformed payload; routing to Unknown",
                    );
                    Ok(MessageStreamEvent::Unknown { tag, raw })
                }
            },
            "content_block_start" => {
                match serde_json::from_value::<ContentBlockStartInner>(raw.clone()) {
                    Ok(inner) => Ok(MessageStreamEvent::ContentBlockStart {
                        index: inner.index,
                        content_block: inner.content_block,
                    }),
                    Err(e) => {
                        tracing::warn!(
                            wire_type = %tag,
                            error = %e,
                            "known Anthropic SSE tag with malformed payload; routing to Unknown",
                        );
                        Ok(MessageStreamEvent::Unknown { tag, raw })
                    }
                }
            }
            "content_block_delta" => {
                match serde_json::from_value::<ContentBlockDeltaInner>(raw.clone()) {
                    Ok(inner) => Ok(MessageStreamEvent::ContentBlockDelta {
                        index: inner.index,
                        delta: inner.delta,
                    }),
                    Err(e) => {
                        tracing::warn!(
                            wire_type = %tag,
                            error = %e,
                            "known Anthropic SSE tag with malformed payload; routing to Unknown",
                        );
                        Ok(MessageStreamEvent::Unknown { tag, raw })
                    }
                }
            }
            "content_block_stop" => {
                match serde_json::from_value::<ContentBlockStopInner>(raw.clone()) {
                    Ok(inner) => Ok(MessageStreamEvent::ContentBlockStop { index: inner.index }),
                    Err(e) => {
                        tracing::warn!(
                            wire_type = %tag,
                            error = %e,
                            "known Anthropic SSE tag with malformed payload; routing to Unknown",
                        );
                        Ok(MessageStreamEvent::Unknown { tag, raw })
                    }
                }
            }
            "message_delta" => match serde_json::from_value::<MessageDeltaInner>(raw.clone()) {
                Ok(inner) => Ok(MessageStreamEvent::MessageDelta {
                    delta: inner.delta,
                    usage: inner.usage,
                }),
                Err(e) => {
                    tracing::warn!(
                        wire_type = %tag,
                        error = %e,
                        "known Anthropic SSE tag with malformed payload; routing to Unknown",
                    );
                    Ok(MessageStreamEvent::Unknown { tag, raw })
                }
            },
            "message_stop" => Ok(MessageStreamEvent::MessageStop),
            "ping" => Ok(MessageStreamEvent::Ping),
            "error" => match serde_json::from_value::<ErrorInner>(raw.clone()) {
                Ok(inner) => Ok(MessageStreamEvent::Error { error: inner.error }),
                Err(e) => {
                    tracing::warn!(
                        wire_type = %tag,
                        error = %e,
                        "known Anthropic SSE tag with malformed payload; routing to Unknown",
                    );
                    Ok(MessageStreamEvent::Unknown { tag, raw })
                }
            },
            _ => {
                tracing::warn!(
                    wire_type = %tag,
                    "unknown Anthropic SSE event type; routing to MessageStreamEvent::Unknown",
                );
                Ok(MessageStreamEvent::Unknown { tag, raw })
            }
        }
    }
}

#[derive(Deserialize)]
struct TextBlockInner {
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct ToolUseBlockInner {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    input: serde_json::Value,
}

#[derive(Deserialize)]
struct ThinkingBlockInner {
    #[serde(default)]
    thinking: String,
}

#[derive(Deserialize)]
struct RedactedThinkingBlockInner {
    #[serde(default)]
    data: String,
}

#[derive(Deserialize)]
struct WebSearchToolResultInner {
    #[serde(default)]
    tool_use_id: String,
    #[serde(default)]
    content: serde_json::Value,
}

impl<'de> Deserialize<'de> for ContentBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let tag = raw
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("<missing>")
            .to_owned();

        match tag.as_str() {
            "text" => match serde_json::from_value::<TextBlockInner>(raw.clone()) {
                Ok(inner) => Ok(ContentBlock::Text { text: inner.text }),
                Err(e) => {
                    tracing::warn!(
                        wire_type = %tag,
                        error = %e,
                        "known Anthropic content_block tag with malformed payload; routing to Unknown",
                    );
                    Ok(ContentBlock::Unknown { tag, raw })
                }
            },
            "tool_use" => match serde_json::from_value::<ToolUseBlockInner>(raw.clone()) {
                Ok(inner) => Ok(ContentBlock::ToolUse {
                    id: inner.id,
                    name: inner.name,
                    input: inner.input,
                }),
                Err(e) => {
                    tracing::warn!(
                        wire_type = %tag,
                        error = %e,
                        "known Anthropic content_block tag with malformed payload; routing to Unknown",
                    );
                    Ok(ContentBlock::Unknown { tag, raw })
                }
            },
            "thinking" => match serde_json::from_value::<ThinkingBlockInner>(raw.clone()) {
                Ok(inner) => Ok(ContentBlock::Thinking {
                    thinking: inner.thinking,
                }),
                Err(e) => {
                    tracing::warn!(
                        wire_type = %tag,
                        error = %e,
                        "known Anthropic content_block tag with malformed payload; routing to Unknown",
                    );
                    Ok(ContentBlock::Unknown { tag, raw })
                }
            },
            "redacted_thinking" => {
                match serde_json::from_value::<RedactedThinkingBlockInner>(raw.clone()) {
                    Ok(inner) => Ok(ContentBlock::RedactedThinking { data: inner.data }),
                    Err(e) => {
                        tracing::warn!(
                            wire_type = %tag,
                            error = %e,
                            "known Anthropic content_block tag with malformed payload; routing to Unknown",
                        );
                        Ok(ContentBlock::Unknown { tag, raw })
                    }
                }
            }
            "server_tool_use" => match serde_json::from_value::<ToolUseBlockInner>(raw.clone()) {
                Ok(inner) => Ok(ContentBlock::ServerToolUse {
                    id: inner.id,
                    name: inner.name,
                    input: inner.input,
                }),
                Err(e) => {
                    tracing::warn!(
                        wire_type = %tag,
                        error = %e,
                        "known Anthropic content_block tag with malformed payload; routing to Unknown",
                    );
                    Ok(ContentBlock::Unknown { tag, raw })
                }
            },
            "web_search_tool_result" => {
                match serde_json::from_value::<WebSearchToolResultInner>(raw.clone()) {
                    Ok(inner) => Ok(ContentBlock::WebSearchToolResult {
                        tool_use_id: inner.tool_use_id,
                        content: inner.content,
                    }),
                    Err(e) => {
                        tracing::warn!(
                            wire_type = %tag,
                            error = %e,
                            "known Anthropic content_block tag with malformed payload; routing to Unknown",
                        );
                        Ok(ContentBlock::Unknown { tag, raw })
                    }
                }
            }
            "code_execution_tool_use" => {
                match serde_json::from_value::<ToolUseBlockInner>(raw.clone()) {
                    Ok(inner) => Ok(ContentBlock::CodeExecutionToolUse {
                        id: inner.id,
                        name: inner.name,
                        input: inner.input,
                    }),
                    Err(e) => {
                        tracing::warn!(
                            wire_type = %tag,
                            error = %e,
                            "known Anthropic content_block tag with malformed payload; routing to Unknown",
                        );
                        Ok(ContentBlock::Unknown { tag, raw })
                    }
                }
            }
            _ => Ok(ContentBlock::Unknown { tag, raw }),
        }
    }
}

#[derive(Deserialize)]
struct TextDeltaInner {
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct InputJsonDeltaInner {
    #[serde(default)]
    partial_json: String,
}

#[derive(Deserialize)]
struct ThinkingDeltaInner {
    #[serde(default)]
    thinking: String,
}

#[derive(Deserialize)]
struct SignatureDeltaInner {
    #[serde(default)]
    signature: String,
}

#[derive(Deserialize)]
struct CitationsDeltaInner {
    #[serde(default)]
    citation: serde_json::Value,
}

impl<'de> Deserialize<'de> for ContentBlockDelta {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let tag = raw
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("<missing>")
            .to_owned();

        match tag.as_str() {
            "text_delta" => match serde_json::from_value::<TextDeltaInner>(raw.clone()) {
                Ok(inner) => Ok(ContentBlockDelta::TextDelta { text: inner.text }),
                Err(e) => {
                    tracing::warn!(
                        wire_type = %tag,
                        error = %e,
                        "known Anthropic delta tag with malformed payload; routing to Unknown",
                    );
                    Ok(ContentBlockDelta::Unknown { tag, raw })
                }
            },
            "input_json_delta" => {
                match serde_json::from_value::<InputJsonDeltaInner>(raw.clone()) {
                    Ok(inner) => Ok(ContentBlockDelta::InputJsonDelta {
                        partial_json: inner.partial_json,
                    }),
                    Err(e) => {
                        tracing::warn!(
                            wire_type = %tag,
                            error = %e,
                            "known Anthropic delta tag with malformed payload; routing to Unknown",
                        );
                        Ok(ContentBlockDelta::Unknown { tag, raw })
                    }
                }
            }
            "thinking_delta" => match serde_json::from_value::<ThinkingDeltaInner>(raw.clone()) {
                Ok(inner) => Ok(ContentBlockDelta::ThinkingDelta {
                    thinking: inner.thinking,
                }),
                Err(e) => {
                    tracing::warn!(
                        wire_type = %tag,
                        error = %e,
                        "known Anthropic delta tag with malformed payload; routing to Unknown",
                    );
                    Ok(ContentBlockDelta::Unknown { tag, raw })
                }
            },
            "signature_delta" => match serde_json::from_value::<SignatureDeltaInner>(raw.clone()) {
                Ok(inner) => Ok(ContentBlockDelta::SignatureDelta {
                    signature: inner.signature,
                }),
                Err(e) => {
                    tracing::warn!(
                        wire_type = %tag,
                        error = %e,
                        "known Anthropic delta tag with malformed payload; routing to Unknown",
                    );
                    Ok(ContentBlockDelta::Unknown { tag, raw })
                }
            },
            "citations_delta" => match serde_json::from_value::<CitationsDeltaInner>(raw.clone()) {
                Ok(inner) => Ok(ContentBlockDelta::CitationsDelta {
                    citation: inner.citation,
                }),
                Err(e) => {
                    tracing::warn!(
                        wire_type = %tag,
                        error = %e,
                        "known Anthropic delta tag with malformed payload; routing to Unknown",
                    );
                    Ok(ContentBlockDelta::Unknown { tag, raw })
                }
            },
            _ => Ok(ContentBlockDelta::Unknown { tag, raw }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── MessageStreamEvent ────────────────────────────────────────────

    #[test]
    fn message_start_parses() {
        let json = r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        match event {
            MessageStreamEvent::MessageStart { message } => {
                assert_eq!(message.id.as_deref(), Some("msg_1"));
                assert_eq!(message.model.as_deref(), Some("claude-sonnet-4.6"));
                assert!(message.usage.is_some());
            }
            other => panic!("expected MessageStart, got {other:?}"),
        }
    }

    #[test]
    fn content_block_start_text_parses() {
        let json =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        match event {
            MessageStreamEvent::ContentBlockStart {
                index,
                content_block: ContentBlock::Text { .. },
            } => {
                assert_eq!(index, 0);
            }
            other => panic!("expected ContentBlockStart{{Text}}, got {other:?}"),
        }
    }

    #[test]
    fn content_block_start_tool_use_parses() {
        let json = r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_a","name":"shell","input":{}}}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        match event {
            MessageStreamEvent::ContentBlockStart {
                content_block: ContentBlock::ToolUse { id, name, .. },
                ..
            } => {
                assert_eq!(id, "toolu_a");
                assert_eq!(name, "shell");
            }
            other => panic!("expected ContentBlockStart{{ToolUse}}, got {other:?}"),
        }
    }

    #[test]
    fn content_block_delta_text_delta_parses() {
        let json =
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        match event {
            MessageStreamEvent::ContentBlockDelta {
                index,
                delta: ContentBlockDelta::TextDelta { text },
            } => {
                assert_eq!(index, 0);
                assert_eq!(text, "hi");
            }
            other => panic!("expected ContentBlockDelta{{TextDelta}}, got {other:?}"),
        }
    }

    #[test]
    fn content_block_stop_parses() {
        let json = r#"{"type":"content_block_stop","index":2}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            MessageStreamEvent::ContentBlockStop { index: 2 }
        ));
    }

    #[test]
    fn message_delta_parses() {
        let json = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        match event {
            MessageStreamEvent::MessageDelta { delta, usage } => {
                assert_eq!(delta.stop_reason, Some(StopReason::EndTurn));
                assert!(usage.is_some());
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
    }

    #[test]
    fn message_stop_parses() {
        let json = r#"{"type":"message_stop"}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, MessageStreamEvent::MessageStop));
    }

    #[test]
    fn ping_parses() {
        let json = r#"{"type":"ping"}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, MessageStreamEvent::Ping));
    }

    #[test]
    fn error_parses() {
        let json = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        match event {
            MessageStreamEvent::Error { error } => {
                assert_eq!(error.error_type.as_deref(), Some("overloaded_error"));
                assert_eq!(error.message.as_deref(), Some("Overloaded"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_sse_event_type_parses_as_unknown_not_panic() {
        let json = r#"{"type":"future_block","data":"some_value"}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        match event {
            MessageStreamEvent::Unknown { tag, raw } => {
                assert_eq!(tag, "future_block");
                assert_eq!(raw["data"], "some_value");
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    // ── ContentBlock ──────────────────────────────────────────────────

    #[test]
    fn content_block_thinking_parses() {
        let json = r#"{"type":"thinking","thinking":"hmm"}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        assert!(matches!(block, ContentBlock::Thinking { .. }));
    }

    #[test]
    fn content_block_redacted_thinking_parses() {
        let json = r#"{"type":"redacted_thinking","data":"opaque"}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        match block {
            ContentBlock::RedactedThinking { data } => assert_eq!(data, "opaque"),
            other => panic!("expected RedactedThinking, got {other:?}"),
        }
    }

    #[test]
    fn content_block_server_tool_use_parses() {
        let json =
            r#"{"type":"server_tool_use","id":"srv_1","name":"web_search","input":{"q":"x"}}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        assert!(matches!(block, ContentBlock::ServerToolUse { .. }));
    }

    #[test]
    fn content_block_web_search_tool_result_parses() {
        let json = r#"{"type":"web_search_tool_result","tool_use_id":"srv_1","content":[]}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        assert!(matches!(block, ContentBlock::WebSearchToolResult { .. }));
    }

    #[test]
    fn content_block_code_execution_tool_use_parses() {
        let json =
            r#"{"type":"code_execution_tool_use","id":"ce_1","name":"exec","input":{"code":"1"}}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        assert!(matches!(block, ContentBlock::CodeExecutionToolUse { .. }));
    }

    #[test]
    fn unknown_content_block_type_parses_as_unknown() {
        let json = r#"{"type":"future_tool","id":"x","name":"exec","input":{}}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        match block {
            ContentBlock::Unknown { tag, raw } => {
                assert_eq!(tag, "future_tool");
                assert_eq!(raw["id"], "x");
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    // ── ContentBlockDelta ─────────────────────────────────────────────

    #[test]
    fn input_json_delta_parses() {
        let json = r#"{"type":"input_json_delta","partial_json":"{\"k\":1}"}"#;
        let delta: ContentBlockDelta = serde_json::from_str(json).unwrap();
        match delta {
            ContentBlockDelta::InputJsonDelta { partial_json } => {
                assert_eq!(partial_json, "{\"k\":1}");
            }
            other => panic!("expected InputJsonDelta, got {other:?}"),
        }
    }

    #[test]
    fn thinking_delta_parses() {
        let json = r#"{"type":"thinking_delta","thinking":"step"}"#;
        let delta: ContentBlockDelta = serde_json::from_str(json).unwrap();
        assert!(matches!(delta, ContentBlockDelta::ThinkingDelta { .. }));
    }

    #[test]
    fn signature_delta_parses() {
        let json = r#"{"type":"signature_delta","signature":"sig="}"#;
        let delta: ContentBlockDelta = serde_json::from_str(json).unwrap();
        assert!(matches!(delta, ContentBlockDelta::SignatureDelta { .. }));
    }

    #[test]
    fn citations_delta_parses() {
        let json = r#"{"type":"citations_delta","citation":{"url":"x"}}"#;
        let delta: ContentBlockDelta = serde_json::from_str(json).unwrap();
        assert!(matches!(delta, ContentBlockDelta::CitationsDelta { .. }));
    }

    #[test]
    fn unknown_content_block_delta_parses_as_unknown() {
        let json = r#"{"type":"future_delta","payload":"x"}"#;
        let delta: ContentBlockDelta = serde_json::from_str(json).unwrap();
        match delta {
            ContentBlockDelta::Unknown { tag, .. } => assert_eq!(tag, "future_delta"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    // ── StopReason ──────────────────────────────────────────────────────────────

    #[test]
    fn stop_reason_known_variants_parse() {
        for (s, expected) in [
            ("end_turn", StopReason::EndTurn),
            ("max_tokens", StopReason::MaxTokens),
            ("stop_sequence", StopReason::StopSequence),
            ("tool_use", StopReason::ToolUse),
            ("pause_turn", StopReason::PauseTurn),
            ("refusal", StopReason::Refusal),
        ] {
            let json = format!("\"{s}\"");
            let parsed: StopReason = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, expected, "stop_reason {s} should round-trip");
        }
    }

    #[test]
    fn stop_reason_unknown_parses_as_unknown() {
        let parsed: StopReason = serde_json::from_str("\"future_reason\"").unwrap();
        assert_eq!(parsed, StopReason::Unknown);
    }

    // ── Malformed-known-tag → Unknown (Fix #1) ─────────────────────────────────

    #[test]
    fn message_start_missing_payload_routes_to_unknown_not_drop() {
        let json = r#"{"type":"message_start"}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        assert!(
            matches!(event, MessageStreamEvent::Unknown { ref tag, .. } if tag == "message_start"),
            "expected Unknown, got {event:?}",
        );
    }

    #[test]
    fn content_block_start_missing_payload_routes_to_unknown_not_drop() {
        let json = r#"{"type":"content_block_start","index":0}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            MessageStreamEvent::Unknown { ref tag, .. } if tag == "content_block_start"
        ));
    }

    // ── Empty-tag sentinel for missing/non-string `type` (Fix #3) ──────────────

    #[test]
    fn missing_type_field_routes_to_unknown_with_sentinel_tag() {
        let json = r#"{"data":"some_value"}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            MessageStreamEvent::Unknown { ref tag, .. } if tag == "<missing>"
        ));
    }

    #[test]
    fn null_type_field_routes_to_unknown() {
        let json = r#"{"type":null,"data":"val"}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            MessageStreamEvent::Unknown { ref tag, .. } if tag == "<missing>"
        ));
    }

    #[test]
    fn non_string_type_field_routes_to_unknown() {
        let json = r#"{"type":42,"data":"val"}"#;
        let event: MessageStreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            MessageStreamEvent::Unknown { ref tag, .. } if tag == "<missing>"
        ));
    }

    #[test]
    fn content_block_missing_type_field_routes_to_unknown_sentinel() {
        let json = r#"{"id":"x"}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        assert!(matches!(
            block,
            ContentBlock::Unknown { ref tag, .. } if tag == "<missing>"
        ));
    }

    #[test]
    fn content_block_null_type_field_routes_to_unknown_sentinel() {
        let json = r#"{"type":null,"id":"x"}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        assert!(matches!(
            block,
            ContentBlock::Unknown { ref tag, .. } if tag == "<missing>"
        ));
    }

    #[test]
    fn content_block_non_string_type_field_routes_to_unknown_sentinel() {
        let json = r#"{"type":7,"id":"x"}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        assert!(matches!(
            block,
            ContentBlock::Unknown { ref tag, .. } if tag == "<missing>"
        ));
    }

    #[test]
    fn content_block_delta_missing_type_field_routes_to_unknown_sentinel() {
        let json = r#"{"payload":"x"}"#;
        let delta: ContentBlockDelta = serde_json::from_str(json).unwrap();
        assert!(matches!(
            delta,
            ContentBlockDelta::Unknown { ref tag, .. } if tag == "<missing>"
        ));
    }

    #[test]
    fn content_block_delta_null_type_field_routes_to_unknown_sentinel() {
        let json = r#"{"type":null,"payload":"x"}"#;
        let delta: ContentBlockDelta = serde_json::from_str(json).unwrap();
        assert!(matches!(
            delta,
            ContentBlockDelta::Unknown { ref tag, .. } if tag == "<missing>"
        ));
    }

    #[test]
    fn content_block_delta_non_string_type_field_routes_to_unknown_sentinel() {
        let json = r#"{"type":3.14,"payload":"x"}"#;
        let delta: ContentBlockDelta = serde_json::from_str(json).unwrap();
        assert!(matches!(
            delta,
            ContentBlockDelta::Unknown { ref tag, .. } if tag == "<missing>"
        ));
    }

    // ── StopReason wiring in MessageDeltaPayload (Fix #2) ──────────────────────

    #[test]
    fn message_delta_payload_parses_stop_reason_and_stop_sequence() {
        let json = r#"{"stop_reason":"stop_sequence","stop_sequence":"STOP"}"#;
        let p: MessageDeltaPayload = serde_json::from_str(json).unwrap();
        assert_eq!(p.stop_reason, Some(StopReason::StopSequence));
        assert_eq!(p.stop_sequence.as_deref(), Some("STOP"));
    }

    #[test]
    fn stop_reason_as_wire_str_round_trips_each_variant() {
        for (s, variant) in [
            ("end_turn", StopReason::EndTurn),
            ("max_tokens", StopReason::MaxTokens),
            ("stop_sequence", StopReason::StopSequence),
            ("tool_use", StopReason::ToolUse),
            ("pause_turn", StopReason::PauseTurn),
            ("refusal", StopReason::Refusal),
        ] {
            assert_eq!(variant.as_wire_str(), s);
        }
    }

    #[test]
    fn anthropic_error_kind_classifies_known_and_unknown() {
        assert_eq!(
            AnthropicErrorKind::from_wire("overloaded_error"),
            AnthropicErrorKind::Overloaded
        );
        assert_eq!(
            AnthropicErrorKind::from_wire("rate_limit_error"),
            AnthropicErrorKind::RateLimit
        );
        assert_eq!(
            AnthropicErrorKind::from_wire("api_error"),
            AnthropicErrorKind::Unknown("api_error".to_owned())
        );
        assert_eq!(
            AnthropicErrorKind::from_wire(""),
            AnthropicErrorKind::Unknown(String::new())
        );
    }
}
