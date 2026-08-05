//! Inbound Anthropic `/v1/messages` request → canonical `ResponseItem[]` for `/responses`.

use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct AnthropicInboundRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<AnthropicMessage>,
    #[serde(default)]
    pub system: Option<SystemPrompt>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum SystemPrompt {
    Text(String),
    Blocks(Vec<SystemTextBlock>),
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SystemTextBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: AnthropicMessageContent,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AnthropicMessageContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContentBlock {
    Text { text: String },
    Image { source: AnthropicImageSource },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Option<ToolResultContent>,
    },
    Thinking { thinking: String },
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ToolResultContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

/// Extract system instructions text from an inbound Anthropic request.
pub fn system_instructions(request: &AnthropicInboundRequest) -> Option<String> {
    request.system.as_ref().map(|system| match system {
        SystemPrompt::Text(text) => text.clone(),
        SystemPrompt::Blocks(blocks) => blocks
            .iter()
            .filter(|b| b.block_type == "text")
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    })
}

/// Translate Anthropic `messages[]` to codex canonical input for `/responses`.
pub fn translate_inbound_to_responses_input(
    messages: &[AnthropicMessage],
) -> Vec<ResponseItem> {
    let mut items = Vec::new();
    for message in messages {
        match message.role.as_str() {
            "user" => items.extend(translate_user_message(message)),
            "assistant" => items.extend(translate_assistant_message(message)),
            _ => {}
        }
    }
    items
}

fn translate_user_message(message: &AnthropicMessage) -> Vec<ResponseItem> {
    let mut items = Vec::new();
    match &message.content {
        AnthropicMessageContent::Text(text) => {
            items.push(user_message(vec![ContentItem::InputText {
                text: text.clone(),
            }]));
        }
        AnthropicMessageContent::Blocks(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                match block {
                    AnthropicContentBlock::Text { text } => {
                        parts.push(ContentItem::InputText { text: text.clone() });
                    }
                    AnthropicContentBlock::Image { source } => {
                        if let Some(url) = image_source_to_url(source) {
                            parts.push(ContentItem::InputImage {
                                image_url: url,
                                detail: None,
                            });
                        }
                    }
                    AnthropicContentBlock::ToolResult {
                        tool_use_id,
                        content,
                    } => {
                        items.push(ResponseItem::FunctionCallOutput {
                            call_id: tool_use_id.clone(),
                            output: FunctionCallOutputPayload::from_text(
                                tool_result_text(content),
                            ),
                        });
                    }
                    _ => {}
                }
            }
            if !parts.is_empty() {
                items.push(user_message(parts));
            }
        }
    }
    items
}

fn translate_assistant_message(message: &AnthropicMessage) -> Vec<ResponseItem> {
    let mut items = Vec::new();
    match &message.content {
        AnthropicMessageContent::Text(text) => {
            items.push(assistant_message(vec![ContentItem::OutputText {
                text: text.clone(),
            }]));
        }
        AnthropicMessageContent::Blocks(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                match block {
                    AnthropicContentBlock::Text { text } => {
                        parts.push(ContentItem::OutputText { text: text.clone() });
                    }
                    AnthropicContentBlock::Thinking { thinking } => {
                        parts.push(ContentItem::OutputText {
                            text: thinking.clone(),
                        });
                    }
                    AnthropicContentBlock::ToolUse { id, name, input } => {
                        items.push(ResponseItem::FunctionCall {
                            id: None,
                            name: name.clone(),
                            namespace: None,
                            arguments: serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
                            call_id: id.clone(),
                        });
                    }
                    _ => {}
                }
            }
            if !parts.is_empty() {
                items.push(assistant_message(parts));
            }
        }
    }
    items
}

fn user_message(content: Vec<ContentItem>) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content,
        phase: None,
    }
}

fn assistant_message(content: Vec<ContentItem>) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content,
        phase: None,
    }
}

fn image_source_to_url(source: &AnthropicImageSource) -> Option<String> {
    match source {
        AnthropicImageSource::Base64 { media_type, data } => {
            if data.is_empty() {
                None
            } else {
                Some(format!("data:{media_type};base64,{data}"))
            }
        }
        AnthropicImageSource::Url { url } => Some(url.clone()),
    }
}

fn tool_result_text(content: &Option<ToolResultContent>) -> String {
    match content {
        None => String::new(),
        Some(ToolResultContent::Text(text)) => text.clone(),
        Some(ToolResultContent::Blocks(blocks)) => blocks
            .iter()
            .filter_map(|block| match block {
                AnthropicContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Anthropic tool defs → Responses API `tools[]` JSON.
pub fn translate_inbound_tools(tools: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for tool in tools {
        let Some(obj) = tool.as_object() else {
            continue;
        };
        let tool_type = obj.get("type").and_then(Value::as_str).unwrap_or("");
        let name = obj.get("name").and_then(Value::as_str).unwrap_or("");
        if tool_type.starts_with("web_search") || name == "web_search" {
            out.push(serde_json::json!({ "type": "web_search_preview" }));
            continue;
        }
        let mut func = serde_json::json!({
            "type": "function",
            "name": name,
        });
        if let Some(desc) = obj.get("description") {
            func["description"] = desc.clone();
        }
        if let Some(schema) = obj.get("input_schema") {
            func["parameters"] = schema.clone();
        }
        out.push(func);
    }
    out
}

/// Anthropic `tool_choice` → Responses API `tool_choice` string/object JSON value.
pub fn translate_tool_choice(tool_choice: &Value) -> Value {
    let tc_type = tool_choice.get("type").and_then(Value::as_str).unwrap_or("auto");
    match tc_type {
        "any" => serde_json::json!("required"),
        "tool" => {
            let name = tool_choice
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("");
            serde_json::json!({ "type": "function", "name": name })
        }
        _ => serde_json::json!("auto"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn user_text_message_translates_to_input_text() {
        let messages = vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicMessageContent::Text("hello".into()),
        }];
        let items = translate_inbound_to_responses_input(&messages);
        assert_eq!(items.len(), 1);
        assert!(matches!(
            &items[0],
            ResponseItem::Message {
                role,
                content,
                ..
            } if role == "user"
                && matches!(&content[0], ContentItem::InputText { text } if text == "hello")
        ));
    }

    #[test]
    fn tool_use_and_tool_result_round_trip_shapes() {
        let messages = vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicMessageContent::Blocks(vec![AnthropicContentBlock::ToolResult {
                    tool_use_id: "toolu_1".into(),
                    content: Some(ToolResultContent::Text("ok".into())),
                }]),
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: AnthropicMessageContent::Blocks(vec![
                    AnthropicContentBlock::Text {
                        text: "calling".into(),
                    },
                    AnthropicContentBlock::ToolUse {
                        id: "toolu_2".into(),
                        name: "shell".into(),
                        input: json!({"cmd": "ls"}),
                    },
                ]),
            },
        ];
        let items = translate_inbound_to_responses_input(&messages);
        assert!(items.iter().any(|i| matches!(
            i,
            ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "toolu_1"
        )));
        assert!(items.iter().any(|i| matches!(
            i,
            ResponseItem::FunctionCall { name, call_id, .. }
                if name == "shell" && call_id == "toolu_2"
        )));
    }

    #[test]
    fn translate_tools_maps_function_schema() {
        let tools = vec![json!({
            "name": "grep",
            "description": "search",
            "input_schema": { "type": "object", "properties": {} }
        })];
        let out = translate_inbound_tools(&tools);
        assert_eq!(out[0]["type"], "function");
        assert_eq!(out[0]["name"], "grep");
        assert_eq!(out[0]["parameters"]["type"], "object");
    }
}
