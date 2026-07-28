//! Project complete Anthropic `/v1/messages` assistant responses (non-stream) into
//! `ResponseItem[]`, mirroring `messages.rs` `content_block_stop` emission.

use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use serde_json::Value;

use super::messages::parse_flat_mcp_tool_name_pub;

/// Non-stream assistant `Message` object → `ResponseItem[]` (HI-C5-009 comparator).
pub fn project_anthropic_assistant_message(message: &Value) -> Vec<ResponseItem> {
    let Some(blocks) = message.get("content").and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for block in blocks {
        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match block_type {
            "text" => {
                let text = block
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                if !text.is_empty() {
                    items.push(ResponseItem::Message {
                        id: None,
                        role: "assistant".to_string(),
                        content: vec![ContentItem::OutputText { text }],
                        phase: None,
                    });
                }
            }
            "tool_use" => {
                let call_id = block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = block
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Default::default()));
                let arguments = serde_json::to_string(&input).unwrap_or_else(|_| "{}".into());
                let (namespace, bare_name) = parse_flat_mcp_tool_name_pub(&name);
                items.push(ResponseItem::FunctionCall {
                    id: None,
                    name: bare_name,
                    namespace,
                    arguments,
                    call_id,
                });
            }
            "thinking" => {
                let thinking = block
                    .get("thinking")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let signature = block
                    .get("signature")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                if thinking.is_empty() {
                    continue;
                }
                let raw_block = if signature.is_empty() {
                    serde_json::json!({"type": "thinking", "thinking": &thinking})
                } else {
                    serde_json::json!({
                        "type": "thinking",
                        "thinking": &thinking,
                        "signature": &signature,
                    })
                };
                items.push(ResponseItem::Reasoning {
                    id: None,
                    summary: vec![ReasoningItemReasoningSummary::SummaryText {
                        text: thinking,
                    }],
                    content: None,
                    encrypted_content: if signature.is_empty() {
                        None
                    } else {
                        Some(signature)
                    },
                    internal_chat_message_metadata_passthrough: None,
                    raw_wire_block: Some(raw_block),
                });
            }
            "redacted_thinking" => {
                let data = block
                    .get("data")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let raw_block = serde_json::json!({
                    "type": "redacted_thinking",
                    "data": &data,
                });
                items.push(ResponseItem::Reasoning {
                    id: None,
                    summary: Vec::new(),
                    content: None,
                    encrypted_content: Some(format!("\0REDACTED\0{data}")),
                    internal_chat_message_metadata_passthrough: None,
                    raw_wire_block: Some(raw_block),
                });
            }
            _ => {}
        }
    }
    items
}

/// Non-stream OpenAI Responses `response.output[]` → `ResponseItem[]`.
pub fn project_responses_api_output(response: &Value) -> Vec<ResponseItem> {
    let output = response
        .get("output")
        .and_then(|v| v.as_array())
        .or_else(|| response.get("items").and_then(|v| v.as_array()));
    let Some(output) = output else {
        return Vec::new();
    };
    output
        .iter()
        .filter_map(|item| serde_json::from_value::<ResponseItem>(item.clone()).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_text_and_tool_use_blocks() {
        let message = serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Hello world"},
                {"type": "tool_use", "id": "call_1", "name": "shell", "input": {"command": "ls"}}
            ]
        });
        let items = project_anthropic_assistant_message(&message);
        assert_eq!(items.len(), 2);
        assert!(matches!(items[0], ResponseItem::Message { .. }));
        assert!(matches!(items[1], ResponseItem::FunctionCall { .. }));
    }

    #[test]
    fn project_responses_output_deserializes_function_call() {
        let response = serde_json::json!({
            "output": [{
                "type": "function_call",
                "call_id": "c1",
                "name": "shell",
                "arguments": "{\"command\":\"ls\"}"
            }]
        });
        let items = project_responses_api_output(&response);
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], ResponseItem::FunctionCall { .. }));
    }

    #[test]
    fn project_ignores_empty_text_blocks() {
        let message = serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text", "text": ""}]
        });
        assert!(project_anthropic_assistant_message(&message).is_empty());
    }

    #[test]
    fn project_redacted_thinking_sentinel() {
        let message = serde_json::json!({
            "role": "assistant",
            "content": [{"type": "redacted_thinking", "data": "opaque"}]
        });
        let items = project_anthropic_assistant_message(&message);
        assert_eq!(items.len(), 1);
        if let ResponseItem::Reasoning {
            encrypted_content, ..
        } = &items[0]
        {
            assert_eq!(
                encrypted_content.as_deref(),
                Some("\0REDACTED\0opaque")
            );
        } else {
            panic!("expected reasoning");
        }
    }

    #[test]
    fn project_mcp_flat_name_splits_namespace() {
        let message = serde_json::json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "t1",
                "name": "mcp__clangd_rs__analyze_symbol",
                "input": {"path": "main.rs"}
            }]
        });
        let items = project_anthropic_assistant_message(&message);
        if let ResponseItem::FunctionCall {
            name,
            namespace,
            ..
        } = &items[0]
        {
            assert_eq!(name, "analyze_symbol");
            assert_eq!(namespace.as_deref(), Some("mcp__clangd_rs"));
        } else {
            panic!("expected function call");
        }
    }
}
