//! Request builder: claude-codex `ResponseItem` list → Copilot `ChatMessage`
//! list.
//!
//! `codex_copilot::ChatMessage` is intentionally minimal — `role` + `content`.
//! It does NOT carry native `OpenAI` `tool_calls` / `tool_call_id` fields (see
//! `codex-agent/codex-copilot/src/client.rs` around line 20). Editing that
//! crate is off-limits (invariants 1, 4, 5 — the 66-test upstream baseline
//! must stay authoritative upstream).
//!
//! We therefore flatten tool semantics into text on the wire:
//!
//! | claude-codex `ResponseItem`          | Copilot `ChatMessage`          |
//! | ------------------------------------ | ------------------------------ |
//! | `Message { role: "user", .. }`       | `ChatMessage::user(text)`      |
//! | `Message { role: "assistant", .. }`  | `ChatMessage::assistant(text)` |
//! | `Message { role: "system", .. }`     | `ChatMessage::system(text)`    |
//! | `FunctionCall { name, args, .. }`    | assistant text w/ inline tag   |
//! | `FunctionCallOutput { call_id, .. }` | `ChatMessage::tool(text)`      |
//!
//! This is lossy vs. the Responses wire but Copilot's chat-completions
//! endpoint treats payloads as free text anyway. Tool semantics ride back to
//! the model as disciplined inline tags the model learned from its own
//! training distribution; no server-side validation happens at Copilot.
//!
//! See `docs/integration/02-design.md` §4.5 in `netbrah/copilot-codex`.
//!
//! # Non-goals
//!
//! * No reasoning / verbosity / `service_tier` knobs — Copilot does not accept
//!   them; omitting them is correct.
//! * `parallel_tool_calls` has no Copilot chat-completions equivalent; it is
//!   accepted for signature parity with the Responses wire and ignored here.
//! * Tool schemas ARE forwarded (v2+). `ToolSpec::Function` and
//!   `ToolSpec::Freeform` are translated to `OpenAI` chat-completions
//!   `tools: [{type:"function", function:{name, description, parameters}}]`
//!   via `tools_to_openai_chat()`. Unsupported variants (`LocalShell`,
//!   `WebSearch`, `ImageGeneration`, `ToolSearch`) are dropped with a
//!   one-shot warning via `unsupported_tool_count()`. Bypasses upstream
//!   `chat_stream_with_auth`'s tools-less signature via the wire layer's
//!   direct POST (see `wire.rs`).

use codex_copilot::ChatMessage;
use codex_protocol::flat_mcp_tool_name;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_tools::ToolSpec;
use serde_json::Value;
use serde_json::json;

/// Inline tag used when flattening a `ResponseItem::FunctionCall` into assistant
/// text. The tag is intentionally human-readable so it survives a round-trip
/// through Copilot's moderation and shows up sensibly in server logs. It is
/// NOT a commitment to a wire format — upstream's v1 API does not validate
/// content shape beyond plain-text constraints.
const FUNCTION_CALL_OPEN: &str = "<function_call";
const FUNCTION_CALL_CLOSE: &str = "</function_call>";

/// Convert claude-codex input items into a Copilot-shaped `Vec<ChatMessage>`.
///
/// The caller (adapter `stream()` entry) has already run
/// `Prompt::get_formatted_input()` to resolve freeform-apply-patch
/// reserialization, so we can treat the input as a final flat list.
#[must_use]
pub fn items_to_chat_messages(items: &[ResponseItem]) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::with_capacity(items.len());

    for item in items {
        match item {
            ResponseItem::Message { role, content, .. } => {
                let text = flatten_content(content);
                if text.is_empty() {
                    continue;
                }
                out.push(match role.as_str() {
                    "system" => ChatMessage::system(text),
                    "assistant" => ChatMessage::assistant(text),
                    // chat-completions has no `developer` role; promote to
                    // `system` (the strongest available) instead of demoting
                    // to `user` and losing instruction priority.
                    "developer" => ChatMessage::system(text),
                    // Default every unknown role to user — safer than dropping.
                    _ => ChatMessage::user(text),
                });
            }
            ResponseItem::FunctionCall {
                name,
                namespace,
                arguments,
                call_id,
                ..
            } => {
                // Re-encode under the advertised flat name so the model's
                // view of its own past namespaced MCP calls matches the live
                // tool list (prevents intermittent "unsupported call").
                let name = flat_mcp_tool_name(name, namespace.as_deref());
                let rendered = format!(
                    "{FUNCTION_CALL_OPEN} name=\"{name}\" call_id=\"{call_id}\">{arguments}{FUNCTION_CALL_CLOSE}"
                );
                out.push(ChatMessage::assistant(rendered));
            }
            ResponseItem::FunctionCallOutput { call_id, output } => {
                // `FunctionCallOutputPayload` renders to a text body via its
                // own Display/serialization path; we use serde_json to avoid
                // leaking private internals of the payload struct.
                let body = serde_json::to_string(output).unwrap_or_default();
                let rendered = format!("[tool_output call_id={call_id}] {body}");
                out.push(ChatMessage::tool(rendered));
            }
            ResponseItem::CustomToolCall {
                name,
                input,
                call_id,
                ..
            } => {
                let rendered = format!(
                    "{FUNCTION_CALL_OPEN} name=\"{name}\" call_id=\"{call_id}\">{input}{FUNCTION_CALL_CLOSE}"
                );
                out.push(ChatMessage::assistant(rendered));
            }
            ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                let body = serde_json::to_string(output).unwrap_or_default();
                let rendered = format!("[tool_output call_id={call_id}] {body}");
                out.push(ChatMessage::tool(rendered));
            }
            // LocalShellCall, Reasoning, ToolSearchCall, ToolSearchOutput,
            // WebSearchCall, and any future variants collapse to "nothing we
            // can forward to Copilot." They are intentionally dropped — the
            // Copilot model never emitted them and receiving them back would
            // pollute context. A trace at debug level records the drop.
            other => {
                tracing::debug!(
                    variant = std::any::type_name_of_val(other),
                    "copilot adapter: dropping non-chat ResponseItem"
                );
            }
        }
    }

    out
}

/// Concatenate the text content of a `ResponseItem::Message` `content` vec
/// into a single string. Image items are skipped — Copilot's chat-completions
/// wire accepts vision via a different payload shape not modeled here. A
/// message that is pure image collapses to an empty string and the caller
/// drops it.
fn flatten_content(content: &[ContentItem]) -> String {
    let mut buf = String::new();
    for item in content {
        match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(text);
            }
            ContentItem::InputImage { .. } => {
                // Intentionally dropped. See module docs.
            }
        }
    }
    buf
}

/// Returns `true` if the given tools contain any spec that Copilot cannot
/// currently exercise. Callers can use this to log a one-line warning on the
/// first turn — it does not alter request construction.
///
/// This exists so the adapter can be honest about capability gaps without
/// crashing the session: a user with `web_search` or `image_generation`
/// tools wired up will still get a working chat turn, they just won't get
/// those tools at this provider.
#[must_use]
pub fn unsupported_tool_count(tools: &[ToolSpec]) -> usize {
    tools
        .iter()
        .filter(|t| {
            matches!(
                t,
                ToolSpec::WebSearch { .. }
                    | ToolSpec::ImageGeneration { .. }
                    | ToolSpec::ToolSearch { .. }
            )
        })
        .count()
}

/// Translate claude-codex `ToolSpec`s into `OpenAI` chat-completions `tools[]`
/// shape: `{type:"function", function:{name, description, parameters}}`.
///
/// Reference: `microsoft/vscode-copilot-chat` emits exactly this shape when
/// `POSTing` to `/chat/completions` on the same enterprise endpoint we hit
/// (see its `src/platform/endpoint/node/openAIEndpoint.ts` request body
/// builder). `OpenAI`'s own Chat Completions API accepts the identical shape.
///
/// Handles these `ToolSpec` variants:
/// - `Function(ResponsesApiTool)`  — direct translation. Covers `shell`,
///   `apply_patch` (when function-shaped), and every MCP tool
///   (`mcp_tool_to_responses_api_tool` is the feeder).
/// - `Freeform(FreeformTool)`      — best-effort translation. The freeform
///   grammar (lark/regex) can't round-trip into JSON Schema, so we project
///   it to a single-string `input` parameter documented with the syntax
///   hint. The model still gets a stable function call; the adapter
///   receives the raw `arguments` string and threads it to the freeform
///   executor on the claude-codex side.
///
/// Drops these variants (they're already warned via `unsupported_tool_count`):
/// - `LocalShell`        — uses a non-chat-completions protocol.
/// - `WebSearch`         — Responses-API-only construct.
/// - `ImageGeneration`   — Responses-API-only.
/// - `ToolSearch`        — Responses-API-only namespace lookup.
///
/// Returns `Vec<Value>` ready to be dropped into the request body's
/// `"tools"` field. Empty in → empty out (caller can use `.is_empty()` to
/// decide whether to emit the `tools` key at all).
#[must_use]
pub fn tools_to_openai_chat(tools: &[ToolSpec]) -> Vec<Value> {
    let mut out = Vec::with_capacity(tools.len());
    for spec in tools {
        match spec {
            ToolSpec::Function(t) => {
                // `ResponsesApiTool` serializes flat for the Responses API.
                // For Chat Completions we wrap in `{type:"function", function:{..}}`.
                // Serialize `parameters` directly — `JsonSchema` is a valid
                // JSON Schema node already.
                let parameters = serde_json::to_value(&t.parameters)
                    .unwrap_or_else(|_| json!({"type": "object", "properties": {}}));
                let mut function = serde_json::Map::new();
                function.insert("name".into(), Value::String(t.name.clone()));
                function.insert("description".into(), Value::String(t.description.clone()));
                function.insert("parameters".into(), parameters);
                if t.strict {
                    function.insert("strict".into(), Value::Bool(true));
                }
                out.push(json!({ "type": "function", "function": function }));
            }
            ToolSpec::Freeform(t) => {
                // Best-effort projection: single `input: string` parameter,
                // with the grammar description folded into the `input`
                // property's `description` so the model sees it.
                let input_desc = format!(
                    "{}\n\nSyntax: {}\nGrammar/definition:\n{}",
                    t.description, t.format.syntax, t.format.definition
                );
                let parameters = json!({
                    "type": "object",
                    "properties": {
                        "input": {
                            "type": "string",
                            "description": input_desc,
                        }
                    },
                    "required": ["input"],
                    "additionalProperties": false,
                });
                out.push(json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": parameters,
                    }
                }));
            }
            // Variants we can't express on chat-completions are dropped here;
            // `unsupported_tool_count()` already flags them at call entry.
            ToolSpec::WebSearch { .. }
            | ToolSpec::ImageGeneration { .. }
            | ToolSpec::ToolSearch { .. }
            | ToolSpec::Namespace(_) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::FunctionCallOutputPayload;

    fn text_msg(role: &str, text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: role.into(),
            content: vec![ContentItem::InputText { text: text.into() }],
            phase: None,
        }
    }

    #[test]
    fn prompt_to_copilot_body_plain_message_round_trip() {
        let items = vec![
            text_msg("system", "you are helpful"),
            text_msg("user", "hi"),
        ];
        let msgs = items_to_chat_messages(&items);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[0].content, "you are helpful");
        assert_eq!(msgs[1].role, "user");
        assert_eq!(msgs[1].content, "hi");
    }

    #[test]
    fn prompt_to_copilot_body_function_call_becomes_tagged_assistant() {
        let items = vec![ResponseItem::FunctionCall {
            id: None,
            name: "shell".into(),
            namespace: None,
            arguments: r#"{"command":["ls"]}"#.into(),
            call_id: "call_1".into(),
        }];
        let msgs = items_to_chat_messages(&items);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "assistant");
        assert!(msgs[0].content.contains("name=\"shell\""));
        assert!(msgs[0].content.contains("call_id=\"call_1\""));
        assert!(msgs[0].content.contains(r#"{"command":["ls"]}"#));
        assert!(msgs[0].content.contains("</function_call>"));
    }

    #[test]
    fn namespaced_function_call_reencodes_with_double_underscore() {
        // History re-encode must reproduce the advertised flat name
        // (`<namespace>__<name>`), not the bare `name`, or the model echoes a
        // spelling no advertised tool matches -> intermittent "unsupported call".
        let items = vec![ResponseItem::FunctionCall {
            id: None,
            name: "add_jira_comment".into(),
            namespace: Some("mcp__atlassian".into()),
            arguments: r#"{"issue":"X-1"}"#.into(),
            call_id: "call_ns".into(),
        }];
        let msgs = items_to_chat_messages(&items);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "assistant");
        assert!(
            msgs[0]
                .content
                .contains("name=\"mcp__atlassian__add_jira_comment\""),
            "expected flat namespaced name, got {:?}",
            msgs[0].content
        );
    }

    #[test]
    fn advertised_and_reencoded_mcp_names_match() {
        // The flat name the model is told about (advertise side) and the flat
        // name we echo back in re-encoded history MUST be byte-identical, or
        // the model's own past call points at a tool the registry can't find.
        // This is the regression that escaped wire-invariance coverage: every
        // prior fixture used `namespace: None`, so the delimiter break was
        // invisible.
        let flat = "mcp__atlassian__add_jira_comment";

        // Advertise side: MCP tools reach copilot as `ToolSpec::Function`
        // whose `name` is already the flat advertised spelling.
        let tool = ToolSpec::Function(codex_tools::ResponsesApiTool {
            name: flat.into(),
            description: "comment on a jira issue".into(),
            strict: false,
            defer_loading: None,
            parameters: codex_tools::JsonSchema::object(
                std::collections::BTreeMap::new(),
                None,
                Some(false.into()),
            ),
            output_schema: None,
        });
        let advertised = tools_to_openai_chat(&[tool]);
        let advertised_name = advertised[0]["function"]["name"].as_str().unwrap();

        // History side: a structured namespaced call re-encodes to the inline
        // `name="..."` tag.
        let items = vec![ResponseItem::FunctionCall {
            id: None,
            name: "add_jira_comment".into(),
            namespace: Some("mcp__atlassian".into()),
            arguments: "{}".into(),
            call_id: "call_eq".into(),
        }];
        let reencoded = items_to_chat_messages(&items);
        assert!(
            reencoded[0]
                .content
                .contains(&format!("name=\"{advertised_name}\"")),
            "advertised name {advertised_name:?} not echoed in re-encoded history {:?}",
            reencoded[0].content
        );
        assert_eq!(advertised_name, flat);
    }

    #[test]
    fn prompt_to_copilot_body_tool_output_becomes_tool_role() {
        let items = vec![ResponseItem::FunctionCallOutput {
            call_id: "call_1".into(),
            output: FunctionCallOutputPayload::from_text("ok".into()),
        }];
        let msgs = items_to_chat_messages(&items);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "tool");
        assert!(msgs[0].content.contains("call_id=call_1"));
    }

    #[test]
    fn prompt_to_copilot_body_drops_image_only_message() {
        let items = vec![ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputImage {
                image_url: "data:image/png;base64,xxx".into(),
                detail: None,
            }],
            phase: None,
        }];
        let msgs = items_to_chat_messages(&items);
        assert!(
            msgs.is_empty(),
            "image-only message should be dropped, got {msgs:?}"
        );
    }

    #[test]
    fn tools_to_openai_chat_function_shape() {
        use codex_tools::JsonSchema;
        use codex_tools::ResponsesApiTool;
        use std::collections::BTreeMap;

        let tool = ToolSpec::Function(ResponsesApiTool {
            name: "shell".into(),
            description: "run a command".into(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([(
                    "command".into(),
                    JsonSchema::array(JsonSchema::string(None), None),
                )]),
                Some(vec!["command".into()]),
                Some(false.into()),
            ),
            output_schema: None,
        });
        let out = tools_to_openai_chat(&[tool]);
        assert_eq!(out.len(), 1);
        let v = &out[0];
        assert_eq!(v["type"], "function");
        assert_eq!(v["function"]["name"], "shell");
        assert_eq!(v["function"]["description"], "run a command");
        assert_eq!(v["function"]["parameters"]["type"], "object");
        assert!(v["function"]["parameters"]["properties"]["command"].is_object());
        assert_eq!(v["function"]["parameters"]["required"][0], "command");
        // strict=false → key absent (avoids forcing strict semantics on servers
        // that don't implement it).
        assert!(v["function"].get("strict").is_none());
    }

    #[test]
    fn tools_to_openai_chat_freeform_projects_to_input_string() {
        use codex_tools::FreeformTool;
        use codex_tools::FreeformToolFormat;
        let tool = ToolSpec::Freeform(FreeformTool {
            name: "apply_patch".into(),
            description: "apply a unified diff".into(),
            format: FreeformToolFormat {
                r#type: "grammar".into(),
                syntax: "lark".into(),
                definition: "start: \"*** Begin Patch\"".into(),
            },
        });
        let out = tools_to_openai_chat(&[tool]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["function"]["name"], "apply_patch");
        let params = &out[0]["function"]["parameters"];
        assert_eq!(params["type"], "object");
        assert_eq!(params["properties"]["input"]["type"], "string");
        assert!(
            params["properties"]["input"]["description"]
                .as_str()
                .unwrap()
                .contains("lark")
        );
        assert_eq!(params["required"][0], "input");
    }

    #[test]
    fn tools_to_openai_chat_drops_unsupported_variants() {
        let tools = vec![
            ToolSpec::ImageGeneration {
                output_format: "png".into(),
            },
            ToolSpec::WebSearch {
                external_web_access: None,
                filters: None,
                user_location: None,
                search_context_size: None,
                search_content_types: None,
            },
        ];
        assert!(tools_to_openai_chat(&tools).is_empty());
    }

    #[test]
    fn developer_role_promoted_to_system() {
        let items = vec![text_msg("developer", "follow these rules")];
        let msgs = items_to_chat_messages(&items);
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            msgs[0].role, "system",
            "developer role must promote to system, not demote to user"
        );
        assert_eq!(msgs[0].content, "follow these rules");
    }

    #[test]
    fn unsupported_tool_count_includes_image_gen_and_web_search() {
        let tools = vec![
            ToolSpec::ImageGeneration {
                output_format: "png".into(),
            },
            ToolSpec::WebSearch {
                external_web_access: None,
                filters: None,
                user_location: None,
                search_context_size: None,
                search_content_types: None,
            },
        ];
        assert_eq!(
            unsupported_tool_count(&tools),
            2,
            "ImageGeneration and WebSearch must be counted as unsupported so the one-shot warning fires"
        );
    }
}
