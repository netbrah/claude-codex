//! Anthropic `/messages` SSE stream accumulator with invariant guards.
//!
//! Provider-side mirror of the Python SDK `accumulate_event` discipline
//! (HCE-01b). Production `codex-api` SSE parser re-wire is Phase 2.

use std::collections::BTreeMap;

use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use serde_json::json;

use crate::stream_invariants::BlockKind;
use crate::stream_invariants::BlockState;
use crate::stream_invariants::StreamInvariantViolation;
use crate::stream_invariants::UsageSnapshot;
use crate::stream_invariants::check_content_block_delta;
use crate::stream_invariants::check_content_block_stop;
use crate::stream_invariants::check_usage_monotonicity;

/// Errors surfaced while accumulating an Anthropic message stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// Provider emitted an `error` SSE event mid-stream.
    InStreamError { payload: String },
    /// Stream invariant violated in a way that corrupts accumulator state.
    StreamStateCorruption(StreamInvariantViolation),
}

/// Top-level SSE events fed into the accumulator (test + provider client).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    Ping,
    Error { payload: String },
    Unknown { tag: String },
    ContentBlockStart {
        index: u32,
        kind: BlockKind,
    },
    ContentBlockDelta {
        index: u32,
        delta_type: String,
        text: Option<String>,
        thinking: Option<String>,
        signature: Option<String>,
        partial_json: Option<String>,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        usage: UsageSnapshot,
    },
    MessageStop,
}

/// In-memory accumulator for one Anthropic streaming message.
#[derive(Debug, Default)]
pub struct StreamAccumulator {
    blocks: BTreeMap<u32, BlockState>,
    usage: UsageSnapshot,
    finished_items: Vec<ResponseItem>,
    stopped: bool,
}

impl StreamAccumulator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn blocks(&self) -> &BTreeMap<u32, BlockState> {
        &self.blocks
    }

    pub fn usage(&self) -> UsageSnapshot {
        self.usage
    }

    pub fn finished_items(&self) -> &[ResponseItem] {
        &self.finished_items
    }

    pub fn accumulate_event(&mut self, event: StreamEvent) -> Result<(), WireError> {
        match event {
            StreamEvent::Ping => {
                tracing::debug!("anthropic-stream:ping");
            }
            StreamEvent::Error { payload } => {
                tracing::warn!(?payload, "anthropic-stream:error-event");
                return Err(WireError::InStreamError { payload });
            }
            StreamEvent::Unknown { tag } => {
                let violation = StreamInvariantViolation::UnknownEventType { event: tag.clone() };
                tracing::warn!(?violation, "anthropic-stream:unknown-event");
            }
            StreamEvent::ContentBlockStart { index, kind } => {
                self.blocks.insert(
                    index,
                    BlockState {
                        index,
                        kind,
                        thinking_text: None,
                        signature: None,
                        input_json: None,
                        data: None,
                        signature_seen: false,
                        stopped: false,
                    },
                );
            }
            StreamEvent::ContentBlockDelta {
                index,
                delta_type,
                text,
                thinking,
                signature,
                partial_json,
            } => {
                if let Some(v) = check_content_block_delta(index, &delta_type, &self.blocks) {
                    if v.is_fatal() {
                        return Err(WireError::StreamStateCorruption(v));
                    }
                    tracing::warn!(?v, "anthropic-stream:invariant-warn");
                }

                match delta_type.as_str() {
                    "text_delta" => {
                        if let Some(block) = self.blocks.get_mut(&index)
                            && block.kind == BlockKind::Text
                        {
                            if let Some(t) = text {
                                block
                                    .thinking_text
                                    .get_or_insert_with(String::new)
                                    .push_str(&t);
                            }
                        }
                    }
                    "thinking_delta" => {
                        if let Some(block) = self.blocks.get_mut(&index)
                            && block.kind == BlockKind::Thinking
                        {
                            if let Some(t) = thinking {
                                block
                                    .thinking_text
                                    .get_or_insert_with(String::new)
                                    .push_str(&t);
                            }
                        }
                    }
                    "signature_delta" => {
                        if let Some(sig) = signature {
                            if let Some(block) = self.blocks.get(&index) {
                                let thinking_empty = block
                                    .thinking_text
                                    .as_deref()
                                    .unwrap_or("")
                                    .is_empty();
                                if thinking_empty && !block.signature_seen {
                                    tracing::warn!(
                                        block_index = index,
                                        "anthropic-stream:signature-before-thinking"
                                    );
                                }
                                if block.signature.is_some() {
                                    tracing::warn!(
                                        block_index = index,
                                        "anthropic-stream:duplicate-signature-delta"
                                    );
                                }
                            }
                            if let Some(block) = self.blocks.get_mut(&index) {
                                let acc = block.signature.get_or_insert_with(String::new);
                                acc.push_str(&sig);
                                block.signature_seen = true;
                            }
                        }
                    }
                    "input_json_delta" => {
                        if let Some(block) = self.blocks.get_mut(&index) {
                            let acc = block.input_json.get_or_insert_with(String::new);
                            if let Some(p) = partial_json {
                                acc.push_str(&p);
                            }
                        }
                    }
                    other => {
                        tracing::warn!(
                            block_index = index,
                            delta_type = other,
                            "anthropic-stream:unknown-delta-subtype"
                        );
                    }
                }
            }
            StreamEvent::ContentBlockStop { index } => {
                if let Some(v) = check_content_block_stop(index, &self.blocks) {
                    return Err(WireError::StreamStateCorruption(v));
                }
                if let Some(mut block) = self.blocks.remove(&index) {
                    block.stopped = true;
                    if let Some(item) = block_to_response_item(&block) {
                        self.finished_items.push(item);
                    }
                }
            }
            StreamEvent::MessageDelta { usage } => {
                if let Some(v) = check_usage_monotonicity(usage, self.usage) {
                    tracing::warn!(?v, "anthropic-stream:usage-non-monotonic");
                } else {
                    self.usage = usage;
                }
            }
            StreamEvent::MessageStop => {
                self.stopped = true;
            }
        }
        Ok(())
    }
}

fn block_to_response_item(block: &BlockState) -> Option<ResponseItem> {
    match block.kind {
        BlockKind::Thinking => {
            let thinking = block.thinking_text.clone().unwrap_or_default();
            let signature = block.signature.clone().unwrap_or_default();
            if thinking.is_empty() {
                return None;
            }
            let raw_block = if signature.is_empty() {
                json!({
                    "type": "thinking",
                    "thinking": &thinking,
                })
            } else {
                json!({
                    "type": "thinking",
                    "thinking": &thinking,
                    "signature": &signature,
                })
            };
            Some(ResponseItem::Reasoning {
                id: None,
                summary: vec![ReasoningItemReasoningSummary::SummaryText { text: thinking }],
                content: None,
                encrypted_content: if signature.is_empty() {
                    None
                } else {
                    Some(signature)
                },
                internal_chat_message_metadata_passthrough: None,

                raw_wire_block: Some(raw_block),
            })
        }
        BlockKind::Text => {
            let text = block.thinking_text.clone().unwrap_or_default();
            if text.is_empty() {
                return None;
            }
            Some(ResponseItem::Message {
                id: None,
                role: "assistant".to_owned(),
                content: vec![codex_protocol::models::ContentItem::OutputText { text }],
                phase: None,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn text_delta_uses_thinking_text_field_for_text_blocks() {
        let mut acc = StreamAccumulator::new();
        acc.accumulate_event(StreamEvent::ContentBlockStart {
            index: 0,
            kind: BlockKind::Text,
        })
        .unwrap();
        acc.accumulate_event(StreamEvent::ContentBlockDelta {
            index: 0,
            delta_type: "text_delta".to_owned(),
            text: Some("hi".to_owned()),
            thinking: None,
            signature: None,
            partial_json: None,
        })
        .unwrap();
        assert_eq!(
            acc.blocks().get(&0).unwrap().thinking_text.as_deref(),
            Some("hi")
        );
    }
}
