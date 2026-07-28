//! Translators between codex-rs internal types and the Google Gemini
//! `generateContent` wire format.
//!
//! Mirrors `messages_wire.rs` for the Anthropic wire — same role:
//! convert `ResponseItem[]` to the provider-specific request shape.

use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::ResponseItem;
use codex_protocol::config_types::ToolChoice;
use codex_protocol::openai_models::ReasoningEffort;
use codex_tools::ToolSpec;
use serde_json::Value;
use serde_json::json;

use crate::schema_sanitize::sanitize_for_gemini;
use codex_api::truncate_tool_name;

/// Synthetic `thoughtSignature` value matching `google-gemini/gemini-cli`'s
/// `geminiChat.ts:99` constant. Gemini's verifier accepts this string in
/// place of a real signature for the FIRST functionCall in every model turn
/// within the active loop -- without it the API returns HTTP 400
/// "Function call must have thoughtSignature".
///
/// The active loop starts at the last user turn that contains text
/// (i.e. NOT a functionResponse-only user turn). Every model-role turn
/// from the active-loop start through the end of the conversation must
/// have a thoughtSignature on its first functionCall part. If the wire
/// captured a real signature via `raw_wire_block`, that wins; this
/// constant is only used when reconstructed/synthesized FunctionCall
/// items reach egress without one.
///
/// Mirrors:
/// - geminiChat.ts:99   (SYNTHETIC_THOUGHT_SIGNATURE)
/// - geminiChat.ts:777  (ensureActiveLoopHasThoughtSignatures)
/// Reference: refs/api-references/harness-invariants-gemini-reference.md
///   row `lifecycle:ensureActiveLoopHasThoughtSignatures:777` -- was MISSING
///   before this commit landed.
pub const SYNTHETIC_THOUGHT_SIGNATURE: &str = "skip_thought_signature_validator";

/// Translates the codex-rs conversation history (`&[ResponseItem]`) into
/// Gemini's `contents[]` array.
///
/// Gemini uses `"user"` and `"model"` roles. System/developer messages are
/// excluded here — they go into `systemInstruction` separately.
///
/// Consecutive `FunctionCall` items are merged into a single `model`-role
/// turn with multiple `functionCall` parts (parallel tool calls).
/// Consecutive `FunctionCallOutput` items are merged into a single
/// `user`-role turn with matching `functionResponse` parts.
pub fn conversation_to_gemini_contents(input: &[ResponseItem]) -> Vec<Value> {
    let mut contents: Vec<Value> = Vec::new();

    // Build call_id -> function name mapping so FunctionCallOutput can
    // supply the correct name in Gemini's functionResponse (which requires
    // the function name, not the opaque call_id).
    let call_id_to_name: std::collections::HashMap<&str, &str> = input
        .iter()
        .filter_map(|item| match item {
            ResponseItem::FunctionCall { call_id, name, .. } => {
                Some((call_id.as_str(), name.as_str()))
            }
            _ => None,
        })
        .collect();

    let mut i = 0;
    while i < input.len() {
        match &input[i] {
            ResponseItem::Message { role, content, .. } => {
                let gemini_role = match role.as_str() {
                    "system" | "developer" => {
                        i += 1;
                        continue;
                    }
                    "assistant" => "model",
                    _ => "user",
                };

                let parts: Vec<Value> = content
                    .iter()
                    .filter_map(|c| match c {
                        ContentItem::InputText { text }
                        | ContentItem::OutputText { text } => Some(json!({"text": text})),
                        _ => None,
                    })
                    .collect();

                if !parts.is_empty() {
                    contents.push(json!({
                        "role": gemini_role,
                        "parts": parts,
                    }));
                }
                i += 1;
            }
            ResponseItem::FunctionCall { .. } => {
                // Merge consecutive FunctionCall items into one model turn.
                let mut parts: Vec<Value> = Vec::new();
                while i < input.len() {
                    if let ResponseItem::FunctionCall {
                        name, arguments, ..
                    } = &input[i]
                    {
                        let args: Value =
                            serde_json::from_str(arguments).unwrap_or(json!({}));
                        parts.push(json!({
                            "functionCall": {
                                "name": name,
                                "args": args,
                            }
                        }));
                        i += 1;
                    } else {
                        break;
                    }
                }
                contents.push(json!({
                    "role": "model",
                    "parts": parts,
                }));
            }
            ResponseItem::FunctionCallOutput { .. } => {
                // Merge consecutive FunctionCallOutput items into one user turn.
                let mut parts: Vec<Value> = Vec::new();
                while i < input.len() {
                    if let ResponseItem::FunctionCallOutput { call_id, output } =
                        &input[i]
                    {
                        let output_text = extract_function_output_text(output);
                        let fn_name = call_id_to_name
                            .get(call_id.as_str())
                            .copied()
                            .unwrap_or("unknown_function");
                        parts.push(json!({
                            "functionResponse": {
                                "name": fn_name,
                                "response": {
                                    "output": output_text,
                                }
                            }
                        }));
                        i += 1;
                    } else {
                        break;
                    }
                }
                contents.push(json!({
                    "role": "user",
                    "parts": parts,
                }));
            }
            ResponseItem::Reasoning { raw_wire_block: Some(block), .. } => {
                // Replay raw wire block as a model-role part. This carries
                // thoughtSignature for round-trip verification.
                //
                // When the block contains a functionCall (i.e. a tool call
                // with thoughtSignature), the next ResponseItem::FunctionCall
                // in the sequence is redundant — skip it so we don't emit
                // duplicate function call parts.
                let has_function_call = block.get("functionCall").is_some();
                contents.push(json!({
                    "role": "model",
                    "parts": [block],
                }));
                i += 1;
                if has_function_call {
                    // Skip the trailing FunctionCall item that duplicates
                    // the function call already in raw_wire_block.
                    if i < input.len() && matches!(&input[i], ResponseItem::FunctionCall { .. }) {
                        i += 1;
                    }
                }
            }
            _ => {
                i += 1;
            }
        }
    }

    merge_consecutive_same_role(&mut contents);
    ensure_active_loop_has_thought_signatures(&mut contents);
    contents
}

/// Merges consecutive entries with the same role into a single turn.
///
/// The Gemini API requires strict user/model role alternation. If the
/// ResponseItem sequence produces consecutive same-role turns (e.g. two
/// model turns from a Reasoning block followed by a text Message), this
/// pass merges their parts into a single turn.
fn merge_consecutive_same_role(contents: &mut Vec<Value>) {
    let mut i = 0;
    while i + 1 < contents.len() {
        let same_role = contents[i].get("role") == contents[i + 1].get("role");
        if same_role {
            // Move parts from contents[i+1] into contents[i].
            if let Some(next_parts) = contents[i + 1]["parts"].as_array().cloned() {
                if let Some(cur_parts) = contents[i]["parts"].as_array_mut() {
                    cur_parts.extend(next_parts);
                }
            }
            contents.remove(i + 1);
        } else {
            i += 1;
        }
    }
}

/// Inject `SYNTHETIC_THOUGHT_SIGNATURE` on the first `functionCall` part of
/// every model-role turn in the active loop that lacks a real signature.
///
/// Ports `geminiChat.ts:777` (`ensureActiveLoopHasThoughtSignatures`).
/// Closes the BLOCKS_PROD MISSING row in
/// `refs/api-references/harness-invariants-gemini-reference.md`
/// (`lifecycle:ensureActiveLoopHasThoughtSignatures:777`).
///
/// Active-loop start = the LAST `user`-role turn that contains a `text` part
/// (NOT a functionResponse-only user turn). If no such turn exists, no
/// injection happens (we have nothing to anchor on).
///
/// Within the active loop, every `model`-role turn's FIRST `functionCall`
/// part gets `thoughtSignature: SYNTHETIC_THOUGHT_SIGNATURE` IFF it does
/// not already have a `thoughtSignature` field. The synthetic value is
/// accepted by Gemini's verifier as a "skip validation" sentinel.
///
/// This guards every code path that produces a `FunctionCall` without
/// `raw_wire_block` (Reasoning items with `raw_wire_block: None` followed
/// by FunctionCall, subagent fold-in, compaction rebuild, etc).
/// Without this guard, Gemini returns HTTP 400
/// "Function call must have thoughtSignature".
fn ensure_active_loop_has_thought_signatures(contents: &mut [Value]) {
    // Phase 1: locate the active-loop start by scanning backward for the
    // LAST user turn that has a text part. functionResponse-only user
    // turns do NOT count (they're tool-result returns, not user input).
    let mut active_loop_start: Option<usize> = None;
    for (i, content) in contents.iter().enumerate().rev() {
        if content.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let has_text_part = content
            .get("parts")
            .and_then(Value::as_array)
            .map(|parts| parts.iter().any(|p| p.get("text").is_some()))
            .unwrap_or(false);
        if has_text_part {
            active_loop_start = Some(i);
            break;
        }
    }

    let Some(start) = active_loop_start else {
        // No user-text turn found -- nothing to anchor the active loop on.
        return;
    };

    // Phase 2: for every model-role turn in the active loop, inject the
    // synthetic signature on the FIRST functionCall part that lacks one.
    for content in contents.iter_mut().skip(start) {
        if content.get("role").and_then(Value::as_str) != Some("model") {
            continue;
        }
        let Some(parts) = content.get_mut("parts").and_then(Value::as_array_mut) else {
            continue;
        };
        for part in parts.iter_mut() {
            let Some(obj) = part.as_object_mut() else { continue };
            if !obj.contains_key("functionCall") {
                continue;
            }
            if !obj.contains_key("thoughtSignature") {
                obj.insert(
                    "thoughtSignature".to_string(),
                    Value::String(SYNTHETIC_THOUGHT_SIGNATURE.to_string()),
                );
            }
            // Only the FIRST functionCall in each turn gets the marker
            // (mirroring geminiChat.ts:818 `break`).
            break;
        }
    }
}

/// Extracts text from a `FunctionCallOutputPayload`.
///
/// **Limitation (Phase 2):** non-text content items (images, MCP
/// structured content) are silently dropped. Gemini's `functionResponse`
/// only accepts text in the `response.output` field. Phase 4 may add
/// `inlineData` support for binary content.
fn extract_function_output_text(
    output: &codex_protocol::models::FunctionCallOutputPayload,
) -> String {
    match &output.body {
        FunctionCallOutputBody::Text(text) => text.clone(),
        FunctionCallOutputBody::ContentItems(items) => items
            .iter()
            .filter_map(|item| {
                use codex_protocol::models::FunctionCallOutputContentItem;
                match item {
                    FunctionCallOutputContentItem::InputText { text } => Some(text.clone()),
                    _ => None,
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Extracts developer-role text blocks from the input to be placed in
/// Gemini's `systemInstruction` field.
pub fn extract_system_instruction(
    base_instructions: &str,
    input: &[ResponseItem],
) -> Option<Value> {
    let mut parts: Vec<Value> = Vec::new();

    if !base_instructions.is_empty() {
        parts.push(json!({"text": base_instructions}));
    }

    // Extract developer-role messages (same as messages_wire.rs).
    for item in input {
        if let ResponseItem::Message { role, content, .. } = item {
            if role == "developer" || role == "system" {
                for c in content {
                    if let ContentItem::InputText { text } = c {
                        if !text.is_empty() {
                            parts.push(json!({"text": text}));
                        }
                    }
                }
            }
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(json!({"parts": parts}))
    }
}

/// Converts codex-rs `ToolSpec` definitions to Gemini `tools[]`.
///
/// Gemini wraps function declarations inside `tools[].functionDeclarations[]`.
/// Only `Function`, `Freeform`, and `Namespace` tool types are translatable.
///
/// When `web_search_enabled` is `true` AND the current model slug supports
/// grounding (checked upstream before calling this function), a
/// `googleSearch: {}` entry is appended as a **separate** Tool object.
///
/// **Wire constraint:** despite older docs/comments claiming otherwise, the
/// `gemini-3-*` generation rejects (`HTTP 500 Internal Server Error`) any
/// request that mixes `functionDeclarations` and `googleSearch` on the same
/// Tool object. Splitting them into two entries in `tools[]` is accepted by
/// both `gemini-2.5-*` and `gemini-3-*` slugs and matches the canonical
/// example in the Gemini grounding documentation.
///
/// If the declarations list is empty, we still emit a single Tool object with
/// `googleSearch` so the model can use grounding.
pub fn tools_to_gemini_format(
    tools: &[ToolSpec],
    web_search_enabled: bool,
) -> Option<Vec<Value>> {
    let declarations: Vec<Value> = tools
        .iter()
        .flat_map(|tool| -> Vec<Value> { match tool {
            ToolSpec::Function(f) => {
                let params = sanitize_for_gemini(
                    &serde_json::to_value(&f.parameters).unwrap_or(json!({})),
                );
                Some(json!({
                    "name": truncate_tool_name(&f.name),
                    "description": f.description,
                    "parameters": params,
                })).into_iter().collect()
            }
            ToolSpec::Freeform(f) => {
                // Gemini's `functionDeclarations` schema cannot express
                // freeform/custom-format tools natively. Mirror the
                // Anthropic adapter's strategy: collapse to a single
                // `input` string parameter and embed the grammar
                // (type/syntax/definition) into the description so the
                // model knows the expected output shape. Without this,
                // models fall back to inventing their own format
                // (e.g. `cat <<'EOF' > file.py ...` heredocs) instead
                // of emitting the apply_patch grammar.
                let description = format!(
                    "{}\n\nThis is a FREEFORM tool. Put your raw freeform content \
                     into the \"input\" parameter as a single string — do NOT \
                     wrap it in any other structure.\n\n\
                     Format ({} / {}): {}",
                    f.description,
                    f.format.r#type,
                    f.format.syntax,
                    f.format.definition,
                );
                let params = sanitize_for_gemini(&json!({
                    "type": "object",
                    "properties": {
                        "input": {
                            "type": "string",
                            "description": "The freeform tool content."
                        }
                    },
                    "required": ["input"]
                }));
                Some(json!({
                    "name": truncate_tool_name(&f.name),
                    "description": description,
                    "parameters": params,
                })).into_iter().collect()
            }
            ToolSpec::Namespace(ns) => {
                // Gemini's `functionDeclarations` is a flat list. Mirror
                // the Anthropic-side flattening: emit one declaration
                // per namespaced tool with the canonical inbound
                // `<namespace>.<name>` convention so the dispatcher
                // routes calls back to the right MCP server.
                ns.tools
                    .iter()
                    .map(|nt| match nt {
                        codex_tools::ResponsesApiNamespaceTool::Function(f) => {
                            let params = sanitize_for_gemini(
                                &serde_json::to_value(&f.parameters).unwrap_or(json!({})),
                            );
                            let wire_name = format!("{}{}", ns.name, f.name);
                            json!({
                                "name": truncate_tool_name(&wire_name),
                                "description": f.description,
                                "parameters": params,
                            })
                        }
                    })
                    .collect()
            }
            _ => Vec::new(),
        }})
        .collect();

    // Build tools[]. `googleSearch` lives in its own Tool object — see
    // doc-comment above for the wire constraint that forces this split.
    // The caller gates `web_search_enabled` via supports_grounding(), so
    // we never send googleSearch to a model that would 400 on it.
    if declarations.is_empty() && !web_search_enabled {
        return None;
    }

    let mut tools_out: Vec<Value> = Vec::new();
    if !declarations.is_empty() {
        let mut decls_obj = serde_json::Map::new();
        decls_obj.insert(
            "functionDeclarations".to_string(),
            Value::Array(declarations),
        );
        tools_out.push(Value::Object(decls_obj));
    }
    if web_search_enabled {
        let mut search_obj = serde_json::Map::new();
        search_obj.insert("googleSearch".to_string(), json!({}));
        tools_out.push(Value::Object(search_obj));
    }

    Some(tools_out)
}

/// Gemini thinking budget configuration.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThinkingConfig {
    pub thinking_budget: u32,
}

/// Returns a `thinkingConfig` JSON value if the model supports thinking.
///
/// Maps codex-rs `ReasoningEffort` to Gemini thinking budgets:
/// - Low -> 1024 tokens
/// - Medium -> 8192 tokens
/// - High (default) -> 24576 tokens
pub fn thinking_config_for_model(
    slug: &str,
    effort: Option<&ReasoningEffort>,
) -> Option<serde_json::Value> {
    if !crate::model::supports_thinking(slug) {
        return None;
    }
    let budget = match effort {
        Some(ReasoningEffort::Low) | Some(ReasoningEffort::Minimal) => 1024,
        Some(ReasoningEffort::Medium) => 8192,
        Some(ReasoningEffort::High) | None => 24576,
        Some(ReasoningEffort::XHigh) => 65536,
        Some(ReasoningEffort::Max) => 65536,
        Some(ReasoningEffort::None) => return None,
    };
    serde_json::to_value(ThinkingConfig { thinking_budget: budget }).ok()
}

/// Maps codex-rs `ToolChoice` to Gemini's `toolConfig.functionCallingConfig`.
///
/// Returns `None` when the default `AUTO` mode suffices (the API defaults
/// to AUTO when `toolConfig` is absent). The returned value is the full
/// `toolConfig` object ready for placement on the request body.
pub fn tool_choice_to_gemini(tool_choice: Option<&ToolChoice>) -> Option<Value> {
    match tool_choice {
        None | Some(ToolChoice::Auto) => None,
        Some(ToolChoice::Required) => Some(json!({
            "functionCallingConfig": {
                "mode": "ANY"
            }
        })),
        Some(ToolChoice::None) => Some(json!({
            "functionCallingConfig": {
                "mode": "NONE"
            }
        })),
        Some(ToolChoice::Specific { name }) => Some(json!({
            "functionCallingConfig": {
                "mode": "ANY",
                "allowedFunctionNames": [name]
            }
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::FunctionCallOutputPayload;
    use codex_protocol::openai_models::ReasoningEffort;

    #[test]
    fn user_message_maps_to_user_role() {
        let input = vec![ResponseItem::Message {
            role: "user".into(),
            content: vec![ContentItem::InputText {
                text: "Hello".into(),
            }],
            id: None,
            phase: None,
        }];

        let contents = conversation_to_gemini_contents(&input);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "Hello");
    }

    #[test]
    fn assistant_maps_to_model_role() {
        let input = vec![ResponseItem::Message {
            role: "assistant".into(),
            content: vec![ContentItem::OutputText {
                text: "Hi there".into(),
            }],
            id: None,
            phase: None,
        }];

        let contents = conversation_to_gemini_contents(&input);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "model");
        assert_eq!(contents[0]["parts"][0]["text"], "Hi there");
    }

    #[test]
    fn system_and_developer_roles_are_skipped() {
        let input = vec![
            ResponseItem::Message {
                role: "system".into(),
                content: vec![ContentItem::InputText {
                    text: "system prompt".into(),
                }],
                id: None,
                phase: None,
            },
            ResponseItem::Message {
                role: "developer".into(),
                content: vec![ContentItem::InputText {
                    text: "dev prompt".into(),
                }],
                id: None,
                phase: None,
            },
            ResponseItem::Message {
                role: "user".into(),
                content: vec![ContentItem::InputText {
                    text: "query".into(),
                }],
                id: None,
                phase: None,
            },
        ];

        let contents = conversation_to_gemini_contents(&input);
        assert_eq!(contents.len(), 1, "system and developer should be skipped");
        assert_eq!(contents[0]["role"], "user");
    }

    #[test]
    fn empty_input_produces_empty_contents() {
        let contents = conversation_to_gemini_contents(&[]);
        assert!(contents.is_empty());
    }

    #[test]
    fn extract_system_instruction_with_base_and_developer() {
        let input = vec![ResponseItem::Message {
            role: "developer".into(),
            content: vec![ContentItem::InputText {
                text: "AGENTS.md content".into(),
            }],
            id: None,
            phase: None,
        }];

        let result = extract_system_instruction("You are helpful.", &input);
        assert!(result.is_some());
        let val = result.unwrap();
        let parts = val["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "You are helpful.");
        assert_eq!(parts[1]["text"], "AGENTS.md content");
    }

    #[test]
    fn extract_system_instruction_empty_returns_none() {
        let result = extract_system_instruction("", &[]);
        assert!(result.is_none());
    }

    #[test]
    fn extract_system_instruction_deduplicates_empty_blocks() {
        let input = vec![
            ResponseItem::Message {
                role: "developer".into(),
                content: vec![ContentItem::InputText {
                    text: "".into(),
                }],
                id: None,
                phase: None,
            },
            ResponseItem::Message {
                role: "system".into(),
                content: vec![ContentItem::InputText {
                    text: "real system prompt".into(),
                }],
                id: None,
                phase: None,
            },
        ];

        let result = extract_system_instruction("", &input);
        assert!(result.is_some());
        let val = result.unwrap();
        let parts = val["parts"].as_array().unwrap();
        // Empty developer text should be skipped, only real content included.
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "real system prompt");
    }

    #[test]
    fn tools_to_gemini_format_function_tool() {
        let schema: codex_tools::JsonSchema = serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "location": {"type": "string"}
            },
            "required": ["location"]
        })).unwrap();
        let tools = vec![ToolSpec::Function(codex_tools::ResponsesApiTool {
            name: "get_weather".into(),
            description: "Get weather".into(),
            parameters: schema,
            strict: false,
            defer_loading: None,
            output_schema: None,
        })];

        let result = tools_to_gemini_format(&tools, /*web_search_enabled*/ false);
        assert!(result.is_some());
        let gemini_tools = result.unwrap();
        assert_eq!(gemini_tools.len(), 1);
        let decls = &gemini_tools[0]["functionDeclarations"];
        assert_eq!(decls[0]["name"], "get_weather");
        assert_eq!(decls[0]["description"], "Get weather");
    }

    #[test]
    fn tools_to_gemini_format_empty_returns_none() {
        let result = tools_to_gemini_format(&[], /*web_search_enabled*/ false);
        assert!(result.is_none());
    }

    /// Translator-completeness invariant — see the analogous test in
    /// `provider-anthropic/src/regression_tests.rs` for the rationale.
    /// Adding a new ToolSpec variant upstream MUST come with an
    /// explicit decision (translate or drop) here so we never silently
    /// drop a class of tools again (the bug that hid every namespaced
    /// MCP tool from Gemini until commit ae52f83511).
    #[test]
    fn tool_spec_translator_completeness() {
        let function_schema: codex_tools::JsonSchema =
            serde_json::from_value(json!({"type":"object","properties":{}})).unwrap();
        let function = ToolSpec::Function(codex_tools::ResponsesApiTool {
            name: "f".into(),
            description: "d".into(),
            parameters: function_schema.clone(),
            strict: false,
            defer_loading: None,
            output_schema: None,
        });
        let freeform = ToolSpec::Freeform(codex_tools::FreeformTool {
            name: "ff".into(),
            description: "d".into(),
            format: codex_tools::FreeformToolFormat {
                r#type: "t".into(),
                syntax: "s".into(),
                definition: "g".into(),
            },
        });
        let namespace = ToolSpec::Namespace(codex_tools::ResponsesApiNamespace {
            name: "mcp__server__".into(),
            description: "d".into(),
            tools: Vec::new(),
        });
        let search = ToolSpec::ToolSearch {
            execution: "client".into(),
            description: "d".into(),
            parameters: function_schema.clone(),
        };
        let image_gen = ToolSpec::ImageGeneration { output_format: "png".into() };
        let web_search = ToolSpec::WebSearch {
            external_web_access: None,
            filters: None,
            user_location: None,
            search_content_types: None,
            search_context_size: None,
        };

        // Per-variant policy: count of declarations Gemini should emit.
        // Empty namespace.tools means 0 declarations is correct here;
        // a populated namespace round-trips through the test in
        // `tools_to_gemini_format_freeform_embeds_grammar_in_description`'s
        // sibling tests.
        let cases: &[(&str, &ToolSpec, usize)] = &[
            ("Function",        &function,    1),
            ("Freeform",        &freeform,    1),
            ("Namespace",       &namespace,   0), // empty inner list
            ("ToolSearch",      &search,      0), // not exposed natively to Gemini
            ("ImageGeneration", &image_gen,   0),
            ("WebSearch",       &web_search,  0),
        ];

        for (name, spec, expected) in cases {
            // web_search_enabled=false: we are testing functionDeclarations
            // translation policy, not googleSearch injection.
            let result = tools_to_gemini_format(std::slice::from_ref(*spec), /*web_search_enabled*/ false);
            let got = result
                .as_ref()
                .and_then(|v| v.first())
                .and_then(|t| t.get("functionDeclarations"))
                .and_then(|d| d.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            assert_eq!(
                got, *expected,
                "ToolSpec::{name} translation policy violated for Gemini: \
                 expected {expected} declarations, got {got} — \
                 update tools_to_gemini_format AND this test together"
            );
        }

        // Compile-time exhaustiveness guard.
        fn _exhaustive(spec: &ToolSpec) {
            match spec {
                ToolSpec::Function(_) => {}
                ToolSpec::Freeform(_) => {}
                ToolSpec::Namespace(_) => {}
                ToolSpec::ToolSearch { .. } => {}
                ToolSpec::ImageGeneration { .. } => {}
                ToolSpec::WebSearch { .. } => {}
            }
        }
    }

    /// Translator-completeness invariant for ResponseItem → Gemini
    /// `contents[]`. See the analogous test in provider-anthropic for
    /// rationale. Gemini handles fewer ResponseItem variants directly
    /// (Message, FunctionCall, FunctionCallOutput, Reasoning); the
    /// other 9 variants are intentionally dropped. The compile-time
    /// match below pins that policy: a new variant upstream fails to
    /// compile until handled here AND in conversation_to_gemini_contents.
    #[test]
    fn response_item_translator_completeness() {
        use codex_protocol::models::{
            ContentItem, FunctionCallOutputBody, FunctionCallOutputPayload,
        };

        let items: Vec<ResponseItem> = vec![
            ResponseItem::Message {
                id: None,
                role: "user".into(),
                content: vec![ContentItem::InputText { text: "hi".into() }],
                phase: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".into(),
                namespace: None,
                arguments: r#"{"cmd":"ls"}"#.into(),
                call_id: "call_1".into(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "call_1".into(),
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text("ok".into()),
                    success: Some(true),
                },
            },
            ResponseItem::Compaction { encrypted_content: "X".into() },
            ResponseItem::Other,
        ];

        // Smoke: must not panic on any well-formed variant.
        let _ = conversation_to_gemini_contents(&items);

        // Compile-time exhaustiveness — every ResponseItem variant must be
        // listed below. Adding a new one upstream fails to compile here.
        fn _exhaustive(item: &ResponseItem) {
            match item {
                ResponseItem::Message { .. } => {}
                ResponseItem::Reasoning { .. } => {}
                ResponseItem::LocalShellCall { .. } => {}
                ResponseItem::FunctionCall { .. } => {}
                ResponseItem::ToolSearchCall { .. } => {}
                ResponseItem::FunctionCallOutput { .. } => {}
                ResponseItem::CustomToolCall { .. } => {}
                ResponseItem::CustomToolCallOutput { .. } => {}
                ResponseItem::ToolSearchOutput { .. } => {}
                ResponseItem::WebSearchCall { .. } => {}
                ResponseItem::ImageGenerationCall { .. } => {}
                ResponseItem::Compaction { .. } => {}
                ResponseItem::CompactionTrigger => {}
                ResponseItem::ContextCompaction { .. } => {}
                ResponseItem::Other => {}
            }
        }
    }

    /// Regression: FREEFORM tools must embed `f.format.{type, syntax,
    /// definition}` in the description so the model knows the expected
    /// output grammar.
    ///
    /// Pre-fix, the description only said "Put raw content into the
    /// input parameter" — gemini had no way to learn the apply_patch
    /// envelope (`*** Begin Patch / *** End Patch / ...`) and would
    /// fall back to inventing its own format, typically a shell
    /// `cat <<'EOF' > file.py` heredoc that bypassed apply_patch
    /// validation entirely. See:
    ///   https://github.com/netbrah/xli/commit/303e151f5a
    ///
    /// Anthropic's adapter has always done this correctly
    /// (provider-anthropic/src/wire.rs:732); this test pins the
    /// behavior on the gemini side so a future refactor can't silently
    /// drop the grammar again.
    #[test]
    fn tools_to_gemini_format_freeform_embeds_grammar_in_description() {
        let tools = vec![ToolSpec::Freeform(codex_tools::FreeformTool {
            name: "apply_patch".into(),
            description: "Use the `apply_patch` tool to edit files.".into(),
            format: codex_tools::FreeformToolFormat {
                r#type: "patch_envelope".into(),
                syntax: "lark".into(),
                definition: "start: begin_patch hunk+ end_patch".into(),
            },
        })];

        let result = tools_to_gemini_format(&tools, /*web_search_enabled*/ false).expect("tools should serialize");
        let decls = &result[0]["functionDeclarations"];
        let desc = decls[0]["description"]
            .as_str()
            .expect("description must be a string");

        // Original description preserved.
        assert!(
            desc.contains("Use the `apply_patch` tool to edit files."),
            "description must preserve the original tool description, got: {desc}"
        );
        // Grammar metadata embedded so gemini knows the expected shape.
        assert!(
            desc.contains("patch_envelope"),
            "description must include format.type, got: {desc}"
        );
        assert!(
            desc.contains("lark"),
            "description must include format.syntax, got: {desc}"
        );
        assert!(
            desc.contains("start: begin_patch hunk+ end_patch"),
            "description must include format.definition (the actual grammar), got: {desc}"
        );
        // Schema collapses to {input: string} — Gemini can't express
        // freeform/custom-format tools natively.
        let params = &decls[0]["parameters"];
        assert_eq!(params["properties"]["input"]["type"], "string");
        assert_eq!(params["required"][0], "input");
    }

    #[test]
    fn function_call_maps_to_model_role() {
        let input = vec![ResponseItem::FunctionCall {
            call_id: "call_123".into(),
            name: "get_weather".into(),
            namespace: None,
            arguments: r#"{"location":"NYC"}"#.into(),
            id: None,
        }];

        let contents = conversation_to_gemini_contents(&input);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "model");
        assert_eq!(contents[0]["parts"][0]["functionCall"]["name"], "get_weather");
        assert_eq!(
            contents[0]["parts"][0]["functionCall"]["args"]["location"],
            "NYC"
        );
    }

    #[test]
    fn function_call_output_maps_to_user_role() {
        let input = vec![
            // Preceding FunctionCall provides the name mapping.
            ResponseItem::FunctionCall {
                call_id: "call_123".into(),
                name: "get_weather".into(),
                namespace: None,
                arguments: "{}".into(),
                id: None,
            },
            ResponseItem::FunctionCallOutput {
                call_id: "call_123".into(),
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text("Sunny, 72F".into()),
                    success: None,
                },
            },
        ];

        let contents = conversation_to_gemini_contents(&input);
        // First entry is the FunctionCall (model role), second is FunctionCallOutput (user role).
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[1]["role"], "user");
        assert_eq!(
            contents[1]["parts"][0]["functionResponse"]["name"],
            "get_weather"
        );
        assert_eq!(
            contents[1]["parts"][0]["functionResponse"]["response"]["output"],
            "Sunny, 72F"
        );
    }

    #[test]
    fn function_call_output_without_matching_call_uses_fallback() {
        let input = vec![ResponseItem::FunctionCallOutput {
            call_id: "orphan_call".into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("result".into()),
                success: None,
            },
        }];

        let contents = conversation_to_gemini_contents(&input);
        assert_eq!(contents.len(), 1);
        assert_eq!(
            contents[0]["parts"][0]["functionResponse"]["name"],
            "unknown_function"
        );
    }

    #[test]
    fn tool_choice_none_and_auto_produce_none() {
        assert!(tool_choice_to_gemini(None).is_none());
        assert!(tool_choice_to_gemini(Some(&ToolChoice::Auto)).is_none());
    }

    #[test]
    fn tool_choice_required_maps_to_any() {
        let val = tool_choice_to_gemini(Some(&ToolChoice::Required)).unwrap();
        assert_eq!(val["functionCallingConfig"]["mode"], "ANY");
        assert!(val["functionCallingConfig"].get("allowedFunctionNames").is_none());
    }

    #[test]
    fn tool_choice_none_variant_maps_to_none_mode() {
        let val = tool_choice_to_gemini(Some(&ToolChoice::None)).unwrap();
        assert_eq!(val["functionCallingConfig"]["mode"], "NONE");
    }

    #[test]
    fn tool_choice_specific_maps_to_any_with_allowed_names() {
        let val = tool_choice_to_gemini(Some(&ToolChoice::Specific {
            name: "shell".into(),
        }))
        .unwrap();
        assert_eq!(val["functionCallingConfig"]["mode"], "ANY");
        let names = val["functionCallingConfig"]["allowedFunctionNames"]
            .as_array()
            .unwrap();
        assert_eq!(names.len(), 1);
        assert_eq!(names[0], "shell");
    }

    #[test]
    fn parallel_function_calls_merge_into_single_model_turn() {
        let input = vec![
            ResponseItem::FunctionCall {
                call_id: "c1".into(),
                name: "shell".into(),
                namespace: None,
                arguments: r#"{"command":["ls"]}"#.into(),
                id: None,
            },
            ResponseItem::FunctionCall {
                call_id: "c2".into(),
                name: "shell".into(),
                namespace: None,
                arguments: r#"{"command":["pwd"]}"#.into(),
                id: None,
            },
        ];

        let contents = conversation_to_gemini_contents(&input);
        assert_eq!(contents.len(), 1, "two FunctionCalls should merge into one model turn");
        assert_eq!(contents[0]["role"], "model");
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["functionCall"]["name"], "shell");
        assert_eq!(parts[1]["functionCall"]["name"], "shell");
    }

    #[test]
    fn parallel_function_call_outputs_merge_into_single_user_turn() {
        let input = vec![
            ResponseItem::FunctionCall {
                call_id: "c1".into(),
                name: "shell".into(),
                namespace: None,
                arguments: "{}".into(),
                id: None,
            },
            ResponseItem::FunctionCall {
                call_id: "c2".into(),
                name: "shell".into(),
                namespace: None,
                arguments: "{}".into(),
                id: None,
            },
            ResponseItem::FunctionCallOutput {
                call_id: "c1".into(),
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text("file1.txt".into()),
                    success: None,
                },
            },
            ResponseItem::FunctionCallOutput {
                call_id: "c2".into(),
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text("/home/user".into()),
                    success: None,
                },
            },
        ];

        let contents = conversation_to_gemini_contents(&input);
        // model turn (2 functionCalls) + user turn (2 functionResponses)
        assert_eq!(contents.len(), 2);
        let model_parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(model_parts.len(), 2);
        let user_parts = contents[1]["parts"].as_array().unwrap();
        assert_eq!(user_parts.len(), 2);
        assert_eq!(user_parts[0]["functionResponse"]["name"], "shell");
        assert_eq!(user_parts[1]["functionResponse"]["name"], "shell");
    }

    #[test]
    fn thinking_config_for_supported_model() {
        let config = super::thinking_config_for_model("gemini-2.5-flash", None);
        assert!(config.is_some());
        let val = config.unwrap();
        assert_eq!(val["thinkingBudget"], 24576);
    }

    #[test]
    fn thinking_config_low_effort() {
        let config = super::thinking_config_for_model(
            "gemini-2.5-pro",
            Some(&ReasoningEffort::Low),
        );
        assert!(config.is_some());
        assert_eq!(config.unwrap()["thinkingBudget"], 1024);
    }

    #[test]
    fn thinking_config_medium_effort() {
        let config = super::thinking_config_for_model(
            "gemini-2.5-pro",
            Some(&ReasoningEffort::Medium),
        );
        assert!(config.is_some());
        assert_eq!(config.unwrap()["thinkingBudget"], 8192);
    }

    #[test]
    fn thinking_config_none_effort_disables() {
        let config = super::thinking_config_for_model(
            "gemini-2.5-pro",
            Some(&ReasoningEffort::None),
        );
        assert!(config.is_none());
    }

    #[test]
    fn thinking_config_unsupported_model() {
        let config = super::thinking_config_for_model("gemini-2.0-flash", None);
        assert!(config.is_none());
    }

    #[test]
    fn reasoning_item_with_raw_wire_block_round_trips() {
        let block = serde_json::json!({
            "text": "Let me think about this...",
            "thought": true
        });
        let input = vec![ResponseItem::Reasoning {
            id: Some("r1".to_string()).into(),
            summary: vec![],
            content: None,
            encrypted_content: None,
            internal_chat_message_metadata_passthrough: None,
            raw_wire_block: Some(block.clone()),
        }];
        let contents = conversation_to_gemini_contents(&input);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "model");
        assert_eq!(contents[0]["parts"][0], block);
    }

    #[test]
    fn reasoning_item_without_raw_wire_block_is_skipped() {
        let input = vec![ResponseItem::Reasoning {
            id: Some("r1".to_string()).into(),
            summary: vec![],
            content: None,
            encrypted_content: None,
            internal_chat_message_metadata_passthrough: None,
            raw_wire_block: None,
        }];
        let contents = conversation_to_gemini_contents(&input);
        assert!(contents.is_empty());
    }

    #[test]
    fn reasoning_with_function_call_skips_trailing_function_call() {
        // Simulates the SSE parser emitting Reasoning(raw_wire_block with
        // functionCall+thoughtSignature) followed by FunctionCall. The egress
        // should use the raw block and skip the redundant FunctionCall.
        let raw_block = serde_json::json!({
            "functionCall": {"name": "shell", "args": {"command": ["echo", "hi"]}},
            "thoughtSignature": "SIG_ABC",
        });
        let input = vec![
            ResponseItem::Reasoning {
                id: Some("sig_0".to_string()).into(),
                summary: vec![],
                content: None,
                encrypted_content: None,
                internal_chat_message_metadata_passthrough: None,
                raw_wire_block: Some(raw_block.clone()),
            },
            ResponseItem::FunctionCall {
                id: None,
                call_id: "gemini_call_0".into(),
                name: "shell".into(),
                arguments: r#"{"command":["echo","hi"]}"#.into(),
                namespace: None,
            },
        ];
        let contents = conversation_to_gemini_contents(&input);
        // Should produce ONE model turn with the raw block (not two turns).
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "model");
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["thoughtSignature"], "SIG_ABC");
        assert_eq!(parts[0]["functionCall"]["name"], "shell");
    }

    #[test]
    fn function_call_without_reasoning_reconstructs_normally() {
        // Legacy path: FunctionCall without a preceding Reasoning block.
        let input = vec![ResponseItem::FunctionCall {
            id: None,
            call_id: "call_1".into(),
            name: "shell".into(),
            arguments: r#"{"command":["ls"]}"#.into(),
            namespace: None,
        }];
        let contents = conversation_to_gemini_contents(&input);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "model");
        assert_eq!(contents[0]["parts"][0]["functionCall"]["name"], "shell");
        // No thoughtSignature — legacy format.
        assert!(contents[0]["parts"][0].get("thoughtSignature").is_none());
    }

    #[test]
    fn thought_signature_preserved_verbatim_on_egress() {
        // End-to-end egress: the exact JSON captured by the SSE parser
        // in raw_wire_block must appear verbatim in the model turn.
        // This proves XLI's provider code does the round-trip, not
        // just the proxy.
        let sig = "AY89a18ktvxfxe22gc8WH7UK85ilXzWhgYH39ayMakBcsje75tEzSkXUSx9aZNUYYw5TTJeKEK7+P1l0RUOgf3di4K1pkUIXeACPapMHB8Z+27StbMekObUbVavZs/Ve6xCgHk0ZEWd7GWtjHzCtouobbWmaWLF0NJyYIVDW/OJ0R4szPV8p2D2K9TFdOrp37UB5OHRs4PQRKRmkbzZri/RazaNHgODgosRwTr5tpJgD/7Oqln5Qa+OXfK4/f/7B8r0eYEE=";
        let raw_block = serde_json::json!({
            "functionCall": {
                "name": "shell",
                "args": {"command": ["echo", "hello"]}
            },
            "thoughtSignature": sig,
        });
        let input = vec![
            // User prompt
            ResponseItem::Message {
                id: None,
                role: "user".into(),
                content: vec![ContentItem::InputText {
                    text: "Run echo hello".into(),
                }],
                phase: None,
            },
            // Model response with thoughtSignature (captured by SSE parser)
            ResponseItem::Reasoning {
                id: Some("gemini_sig_0".to_string()).into(),
                summary: vec![],
                content: None,
                encrypted_content: None,
                internal_chat_message_metadata_passthrough: None,
                raw_wire_block: Some(raw_block),
            },
            ResponseItem::FunctionCall {
                id: None,
                call_id: "gemini_call_0".into(),
                name: "shell".into(),
                arguments: r#"{"command":["echo","hello"]}"#.into(),
                namespace: None,
            },
            // Tool result
            ResponseItem::FunctionCallOutput {
                call_id: "gemini_call_0".into(),
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text("hello\n".into()),
                    success: None,
                },
            },
        ];

        let contents = conversation_to_gemini_contents(&input);

        // Should produce: user turn, model turn (with sig), user turn (functionResponse)
        assert_eq!(contents.len(), 3, "user + model(sig) + user(functionResponse)");

        // Model turn must carry the exact thoughtSignature
        assert_eq!(contents[1]["role"], "model");
        let model_part = &contents[1]["parts"][0];
        assert_eq!(
            model_part["thoughtSignature"].as_str().unwrap(),
            sig,
            "thoughtSignature must be preserved verbatim"
        );
        assert_eq!(model_part["functionCall"]["name"], "shell");

        // The FunctionCall item should have been skipped (deduped)
        // — no second model turn
        assert_eq!(contents[2]["role"], "user");
        assert!(
            contents[2]["parts"][0].get("functionResponse").is_some(),
            "third turn should be the functionResponse"
        );
    }

    #[test]
    fn consecutive_same_role_turns_merged() {
        // If a Reasoning(thinking text) followed by a FunctionCall
        // produces two model turns, they should be merged.
        let input = vec![
            ResponseItem::Reasoning {
                id: Some("r1".to_string()).into(),
                summary: vec![],
                content: None,
                encrypted_content: None,
                internal_chat_message_metadata_passthrough: None,
                raw_wire_block: Some(serde_json::json!({
                    "text": "thinking...",
                    "thought": true,
                    "thoughtSignature": "SIG_THINK",
                })),
            },
            // No FunctionCall after this — just another model message
            ResponseItem::Message {
                id: None,
                role: "assistant".into(),
                content: vec![ContentItem::OutputText {
                    text: "The answer is 42.".into(),
                }],
                phase: None,
            },
        ];

        let contents = conversation_to_gemini_contents(&input);

        // Both are model-role turns — they should be merged into one
        assert_eq!(contents.len(), 1, "same-role turns should merge");
        assert_eq!(contents[0]["role"], "model");
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2, "merged turn should have 2 parts");
        assert_eq!(parts[0]["thoughtSignature"], "SIG_THINK");
        assert_eq!(parts[1]["text"], "The answer is 42.");
    }

    // -----------------------------------------------------------------------
    // Google Search grounding tool emission tests (GS-3)
    // -----------------------------------------------------------------------

    #[test]
    fn google_search_included_when_web_search_enabled() {
        let result = tools_to_gemini_format(&[], /*web_search_enabled*/ true);
        assert!(result.is_some(), "googleSearch should produce a non-None result");
        let tools = result.unwrap();
        assert_eq!(tools.len(), 1);
        assert!(
            tools[0].get("googleSearch").is_some(),
            "tool object must contain googleSearch key, got: {}",
            tools[0]
        );
        assert_eq!(tools[0]["googleSearch"], json!({}));
        // No functionDeclarations when there are no ToolSpec tools.
        assert!(tools[0].get("functionDeclarations").is_none());
    }

    #[test]
    fn google_search_absent_when_web_search_disabled() {
        let result = tools_to_gemini_format(&[], /*web_search_enabled*/ false);
        assert!(result.is_none(), "empty tools + no web_search should return None");
    }

    #[test]
    fn google_search_coexists_with_function_declarations() {
        let schema: codex_tools::JsonSchema = serde_json::from_value(json!({
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"]
        }))
        .unwrap();
        let tools = vec![ToolSpec::Function(codex_tools::ResponsesApiTool {
            name: "shell".into(),
            description: "Execute a command.".into(),
            parameters: schema,
            strict: false,
            defer_loading: None,
            output_schema: None,
        })];

        let result = tools_to_gemini_format(&tools, /*web_search_enabled*/ true);
        assert!(result.is_some());
        let gemini_tools = result.unwrap();
        // Gemini 3 rejects requests that mix functionDeclarations and
        // googleSearch on the same Tool object (HTTP 500). They must be
        // emitted as two separate entries in tools[].
        assert_eq!(
            gemini_tools.len(),
            2,
            "functionDeclarations and googleSearch must be separate Tool objects"
        );

        let decls_tool = gemini_tools
            .iter()
            .find(|t| t.get("functionDeclarations").is_some())
            .expect("a Tool object with functionDeclarations must be present");
        let search_tool = gemini_tools
            .iter()
            .find(|t| t.get("googleSearch").is_some())
            .expect("a Tool object with googleSearch must be present");

        // Neither Tool object may mix the two keys — that's what causes
        // the 500 on gemini-3-*.
        assert!(
            decls_tool.get("googleSearch").is_none(),
            "functionDeclarations Tool must NOT also carry googleSearch"
        );
        assert!(
            search_tool.get("functionDeclarations").is_none(),
            "googleSearch Tool must NOT also carry functionDeclarations"
        );

        let decls = decls_tool["functionDeclarations"].as_array().unwrap();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0]["name"], "shell");
        assert_eq!(search_tool["googleSearch"], json!({}));
    }

    #[test]
    fn google_search_key_is_empty_object() {
        // The Gemini API requires exactly `"googleSearch": {}` -- any fields
        // in the object cause a 400 on some API versions. Verify the value.
        let result = tools_to_gemini_format(&[], /*web_search_enabled*/ true).unwrap();
        let gs = &result[0]["googleSearch"];
        assert!(gs.is_object(), "googleSearch value must be an object");
        assert!(
            gs.as_object().unwrap().is_empty(),
            "googleSearch value must be empty object {{}}, got: {gs}"
        );
    }

    // ===============================================================
    // ensure_active_loop_has_thought_signatures (Gemini Stage A gap
    // closure: refs/api-references/harness-invariants-gemini-reference.md
    // row `lifecycle:ensureActiveLoopHasThoughtSignatures:777`)
    // ===============================================================

    fn user_text(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText { text: text.into() }],
            phase: None,
        }
    }

    fn assistant_text(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "assistant".into(),
            content: vec![ContentItem::OutputText { text: text.into() }],
            phase: None,
        }
    }

    fn fn_call(name: &str, call_id: &str, args_json: &str) -> ResponseItem {
        ResponseItem::FunctionCall {
            id: None,
            name: name.into(),
            namespace: None,
            arguments: args_json.into(),
            call_id: call_id.into(),
        }
    }

    fn fn_call_output(call_id: &str, output: &str) -> ResponseItem {
        use codex_protocol::models::FunctionCallOutputBody;
        use codex_protocol::models::FunctionCallOutputPayload;
        ResponseItem::FunctionCallOutput {
            call_id: call_id.into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text(output.into()),
                success: Some(true),
            },
        }
    }

    fn assert_function_call_has_signature(
        contents: &[Value],
        i: usize,
        j: usize,
        expected_sig: &str,
    ) {
        let content = contents
            .get(i)
            .unwrap_or_else(|| panic!("contents has no index {i}; got len={}", contents.len()));
        assert_eq!(
            content.get("role").and_then(Value::as_str),
            Some("model"),
            "contents[{i}] should be a model-role turn"
        );
        let parts = content
            .get("parts")
            .and_then(Value::as_array)
            .expect("model turn has parts");
        let part = parts
            .get(j)
            .unwrap_or_else(|| panic!("model turn[{i}] has no part {j}"));
        assert!(
            part.get("functionCall").is_some(),
            "part[{j}] should be a functionCall, got: {part}"
        );
        assert_eq!(
            part.get("thoughtSignature").and_then(Value::as_str),
            Some(expected_sig),
            "part[{j}] thoughtSignature mismatch; got: {part}"
        );
    }

    #[test]
    fn synthetic_signature_injected_on_bare_function_call_after_user_text() {
        let history = vec![
            user_text("call the shell tool"),
            fn_call("shell", "call_1", "{\"cmd\":\"ls\"}"),
        ];
        let contents = conversation_to_gemini_contents(&history);
        assert_function_call_has_signature(&contents, 1, 0, SYNTHETIC_THOUGHT_SIGNATURE);
    }

    #[test]
    fn real_signature_from_raw_wire_block_is_preserved_not_overwritten() {
        let raw_with_call = json!({
            "functionCall": {"name": "shell", "args": {"cmd": "ls"}},
            "thoughtSignature": "REAL_SIG_FROM_WIRE",
        });
        let history = vec![
            user_text("call shell"),
            ResponseItem::Reasoning {
                id: None,
                summary: vec![],
                content: None,
                encrypted_content: None,
                internal_chat_message_metadata_passthrough: None,
                raw_wire_block: Some(raw_with_call),
            },
            fn_call("shell", "call_real", "{\"cmd\":\"ls\"}"),
        ];
        let contents = conversation_to_gemini_contents(&history);
        assert_function_call_has_signature(&contents, 1, 0, "REAL_SIG_FROM_WIRE");
    }

    #[test]
    fn no_injection_when_history_has_no_user_text_turn() {
        let history = vec![fn_call("shell", "call_x", "{}")];
        let contents = conversation_to_gemini_contents(&history);
        let parts = contents[0]["parts"].as_array().expect("parts");
        assert!(
            parts[0].get("thoughtSignature").is_none(),
            "no user-text turn means no active loop; got: {}",
            parts[0]
        );
    }

    #[test]
    fn function_response_user_turn_does_not_anchor_active_loop() {
        let history = vec![
            user_text("start a task"),
            fn_call("shell", "call_1", "{}"),
            fn_call_output("call_1", "output1"),
            fn_call("shell", "call_2", "{}"),
        ];
        let contents = conversation_to_gemini_contents(&history);
        assert_function_call_has_signature(&contents, 1, 0, SYNTHETIC_THOUGHT_SIGNATURE);
        assert_function_call_has_signature(&contents, 3, 0, SYNTHETIC_THOUGHT_SIGNATURE);
    }

    #[test]
    fn second_user_text_turn_resets_active_loop_start() {
        let history = vec![
            user_text("first task"),
            fn_call("shell", "call_a", "{}"),
            fn_call_output("call_a", "done"),
            user_text("second task"),
            fn_call("shell", "call_b", "{}"),
        ];
        let contents = conversation_to_gemini_contents(&history);
        let fn_a_part = &contents[1]["parts"][0];
        assert!(
            fn_a_part.get("thoughtSignature").is_none(),
            "model-fn-A precedes the active loop start; must NOT get synthetic sig. Part: {fn_a_part}"
        );
        let last_model_fn_turn_idx = contents
            .iter()
            .enumerate()
            .rfind(|(_, c)| {
                c.get("role").and_then(Value::as_str) == Some("model")
                    && c.get("parts")
                        .and_then(Value::as_array)
                        .map(|p| p.iter().any(|pp| pp.get("functionCall").is_some()))
                        .unwrap_or(false)
            })
            .map(|(i, _)| i)
            .expect("model-fn turn exists for call_b");
        assert_function_call_has_signature(
            &contents,
            last_model_fn_turn_idx,
            0,
            SYNTHETIC_THOUGHT_SIGNATURE,
        );
    }

    #[test]
    fn only_first_function_call_in_turn_gets_signature() {
        let history = vec![
            user_text("call three tools"),
            fn_call("shell", "c1", "{}"),
            fn_call("ping", "c2", "{}"),
            fn_call("grep", "c3", "{}"),
        ];
        let contents = conversation_to_gemini_contents(&history);
        let parts = contents[1]["parts"].as_array().expect("3 parts");
        assert_eq!(parts.len(), 3);
        assert_eq!(
            parts[0].get("thoughtSignature").and_then(Value::as_str),
            Some(SYNTHETIC_THOUGHT_SIGNATURE),
            "first functionCall part must have synthetic sig"
        );
        assert!(
            parts[1].get("thoughtSignature").is_none(),
            "second functionCall part must NOT get a signature"
        );
        assert!(
            parts[2].get("thoughtSignature").is_none(),
            "third functionCall part must NOT get a signature"
        );
    }

    #[test]
    fn assistant_text_only_turn_in_active_loop_is_untouched() {
        let history = vec![
            user_text("hi"),
            assistant_text("hello! how can I help?"),
        ];
        let contents = conversation_to_gemini_contents(&history);
        assert_eq!(
            contents[1].get("role").and_then(Value::as_str),
            Some("model")
        );
        let parts = contents[1]["parts"].as_array().expect("parts");
        for part in parts {
            assert!(
                part.get("thoughtSignature").is_none(),
                "text-only model turn must not carry thoughtSignature; got: {part}"
            );
        }
    }

    #[test]
    fn synthetic_signature_constant_matches_gemini_chat_value() {
        assert_eq!(SYNTHETIC_THOUGHT_SIGNATURE, "skip_thought_signature_validator");
    }

    #[test]
    fn injection_is_idempotent_across_multiple_translation_runs() {
        let history = vec![
            user_text("task"),
            fn_call("shell", "call_1", "{}"),
        ];
        let first = conversation_to_gemini_contents(&history);
        let second = conversation_to_gemini_contents(&history);
        assert_eq!(first, second, "translation must be idempotent");
    }

    #[test]
    fn multi_turn_active_loop_injects_on_every_model_function_call_turn() {
        let history = vec![
            user_text("kick off"),
            fn_call("a", "call_a", "{}"),
            fn_call_output("call_a", "ra"),
            fn_call("b", "call_b", "{}"),
            fn_call_output("call_b", "rb"),
            fn_call("c", "call_c", "{}"),
        ];
        let contents = conversation_to_gemini_contents(&history);
        let model_fn_turns: Vec<&Value> = contents
            .iter()
            .filter(|c| {
                c.get("role").and_then(Value::as_str) == Some("model")
                    && c.get("parts")
                        .and_then(Value::as_array)
                        .map(|p| p.iter().any(|pp| pp.get("functionCall").is_some()))
                        .unwrap_or(false)
            })
            .collect();
        assert_eq!(
            model_fn_turns.len(),
            3,
            "expected 3 model-fn turns, got {}",
            model_fn_turns.len()
        );
        for (idx, turn) in model_fn_turns.iter().enumerate() {
            let parts = turn["parts"].as_array().unwrap();
            let first_fn_part = parts
                .iter()
                .find(|p| p.get("functionCall").is_some())
                .unwrap_or_else(|| panic!("turn {idx} has no functionCall part"));
            assert_eq!(
                first_fn_part.get("thoughtSignature").and_then(Value::as_str),
                Some(SYNTHETIC_THOUGHT_SIGNATURE),
                "model-fn turn {idx} first functionCall missing synthetic sig"
            );
        }
    }
}
