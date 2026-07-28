//! Downstream `/responses` events → Anthropic `/v1/messages` SSE sequence.

use crate::common::ResponseEvent;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use serde::Serialize;
use serde_json::Value;

/// One Anthropic SSE `event:` payload (JSON object after `data:`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AnthropicSseEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(flatten)]
    pub body: Value,
}

/// Synthesize Anthropic streaming events from canonical downstream `ResponseEvent`s.
///
/// First ship: text + `function_call` blocks; reasoning blocks map to thinking deltas.
pub fn synthesize_anthropic_sse_from_response_events(
    model: &str,
    message_id: &str,
    events: &[ResponseEvent],
) -> Vec<AnthropicSseEvent> {
    let mut out = Vec::new();
    let mut block_index: u64 = 0;
    let mut started = false;
    let mut usage: Option<TokenUsage> = None;
    let mut stop_reason = "end_turn".to_string();

    for event in events {
        match event {
            ResponseEvent::OutputItemAdded(item) | ResponseEvent::OutputItemDone(item) => {
                if !started {
                    out.push(message_start(model, message_id));
                    started = true;
                }
                match item {
                    ResponseItem::Message { content, .. } => {
                        for part in content {
                            if let ContentItem::OutputText { text } = part {
                                if text.is_empty() {
                                    continue;
                                }
                                out.extend(text_block_events(block_index, text));
                                block_index += 1;
                            }
                        }
                    }
                    ResponseItem::FunctionCall {
                        name,
                        arguments,
                        call_id,
                        ..
                    } => {
                        let input = serde_json::from_str(arguments).unwrap_or(Value::Object(
                            serde_json::Map::new(),
                        ));
                        out.push(content_block_start(
                            block_index,
                            serde_json::json!({
                                "type": "tool_use",
                                "id": call_id,
                                "name": name,
                                "input": input,
                            }),
                        ));
                        out.push(content_block_stop(block_index));
                        block_index += 1;
                        stop_reason = "tool_use".to_string();
                    }
                    ResponseItem::Reasoning { summary, .. } => {
                        let thinking = summary
                            .iter()
                            .map(|s| match s {
                                codex_protocol::models::ReasoningItemReasoningSummary::SummaryText {
                                    text,
                                } => text.as_str(),
                            })
                            .collect::<Vec<_>>()
                            .join("");
                        if !thinking.is_empty() {
                            out.extend(thinking_block_events(block_index, &thinking));
                            block_index += 1;
                        }
                    }
                    _ => {}
                }
            }
            ResponseEvent::OutputTextDelta(delta) => {
                if !started {
                    out.push(message_start(model, message_id));
                    started = true;
                    out.push(content_block_start(
                        block_index,
                        serde_json::json!({ "type": "text", "text": "" }),
                    ));
                }
                out.push(content_block_delta(
                    block_index,
                    serde_json::json!({ "type": "text_delta", "text": delta }),
                ));
            }
            ResponseEvent::Completed {
                token_usage,
                stop_reason: downstream_stop,
                ..
            } => {
                usage = token_usage.clone();
                if let Some(reason) = downstream_stop {
                    stop_reason = reason.clone();
                }
            }
            _ => {}
        }
    }

    if started {
        if out.last().is_none_or(|e| e.event_type != "content_block_stop") {
            // Close any open text block opened only via deltas.
            if out.iter().any(|e| e.event_type == "content_block_delta") {
                out.push(content_block_stop(block_index));
            }
        }
        out.push(message_delta(&stop_reason, usage.as_ref()));
        out.push(AnthropicSseEvent {
            event_type: "message_stop".into(),
            body: serde_json::json!({}),
        });
    }

    out
}

fn message_start(model: &str, message_id: &str) -> AnthropicSseEvent {
    AnthropicSseEvent {
        event_type: "message_start".into(),
        body: serde_json::json!({
            "message": {
                "id": message_id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": { "input_tokens": 0, "output_tokens": 0 }
            }
        }),
    }
}

fn content_block_start(index: u64, content_block: Value) -> AnthropicSseEvent {
    AnthropicSseEvent {
        event_type: "content_block_start".into(),
        body: serde_json::json!({
            "index": index,
            "content_block": content_block,
        }),
    }
}

fn content_block_delta(index: u64, delta: Value) -> AnthropicSseEvent {
    AnthropicSseEvent {
        event_type: "content_block_delta".into(),
        body: serde_json::json!({
            "index": index,
            "delta": delta,
        }),
    }
}

fn content_block_stop(index: u64) -> AnthropicSseEvent {
    AnthropicSseEvent {
        event_type: "content_block_stop".into(),
        body: serde_json::json!({ "index": index }),
    }
}

fn message_delta(stop_reason: &str, usage: Option<&TokenUsage>) -> AnthropicSseEvent {
    let usage_json = usage.map(|u| {
        serde_json::json!({
            "input_tokens": u.input_tokens,
            "output_tokens": u.output_tokens,
        })
    });
    AnthropicSseEvent {
        event_type: "message_delta".into(),
        body: serde_json::json!({
            "delta": { "stop_reason": stop_reason, "stop_sequence": null },
            "usage": usage_json,
        }),
    }
}

fn text_block_events(index: u64, text: &str) -> Vec<AnthropicSseEvent> {
    vec![
        content_block_start(
            index,
            serde_json::json!({ "type": "text", "text": "" }),
        ),
        content_block_delta(
            index,
            serde_json::json!({ "type": "text_delta", "text": text }),
        ),
        content_block_stop(index),
    ]
}

fn thinking_block_events(index: u64, thinking: &str) -> Vec<AnthropicSseEvent> {
    vec![
        content_block_start(
            index,
            serde_json::json!({ "type": "thinking", "thinking": "" }),
        ),
        content_block_delta(
            index,
            serde_json::json!({ "type": "thinking_delta", "thinking": thinking }),
        ),
        content_block_stop(index),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::ResponseItem;

    #[test]
    fn text_response_synthesizes_anthropic_sse_sequence() {
        let events = vec![
            ResponseEvent::Created,
            ResponseEvent::OutputItemDone(ResponseItem::Message {
                id: None,
                role: "assistant".into(),
                content: vec![ContentItem::OutputText {
                    text: "Hi there".into(),
                }],
                phase: None,
            }),
            ResponseEvent::Completed {
                stop_reason: Some("end_turn".into()),
                response_id: "resp_1".into(),
                token_usage: Some(TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    total_tokens: 15,
                    ..Default::default()
                }),
                end_turn: Some(true),
            },
        ];
        let sse = synthesize_anthropic_sse_from_response_events("gpt-4.1", "msg_1", &events);
        let types: Vec<_> = sse.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(
            types,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
    }
}
