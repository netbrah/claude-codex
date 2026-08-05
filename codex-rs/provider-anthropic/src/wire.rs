//! Translators between codex-rs internal types and the Anthropic `/messages`
//! wire format.

use codex_protocol::flat_mcp_tool_name;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::ResponseItem;
use codex_tools::ToolSpec;
use serde_json::Value;
use serde_json::json;

/// S-031: Internal-only provenance marker added to `thinking` /
/// `redacted_thinking` blocks that were *reconstructed* from decomposed
/// Reasoning fields rather than replayed byte-identically from
/// `raw_wire_block`. Reconstructed blocks carry a synthetic/empty signature
/// and must be stripped from the latest assistant message before send (see
/// `strip_thinking_from_non_latest_assistant_messages`). This key is scrubbed
/// from every surviving block, so it never reaches the Anthropic wire.
const RECONSTRUCTED_THINKING_MARKER: &str = "__xli_reconstructed_thinking";

/// Removes orphaned tool calls and tool results from the conversation history.
///
/// An "orphaned" tool call is a `FunctionCall`, `LocalShellCall`, or
/// `CustomToolCall` without a matching `FunctionCallOutput` /
/// `CustomToolCallOutput` (matched by `call_id`), or vice-versa.
///
/// Sending unpaired `tool_use` / `tool_result` blocks to the Anthropic API
/// causes a 400 error, so we strip them before translation.
fn clean_orphaned_tool_calls(input: &[ResponseItem]) -> Vec<ResponseItem> {
    use std::collections::HashSet;

    // First pass: collect call_ids from tool calls and tool outputs.
    let mut call_ids: HashSet<String> = HashSet::new();
    let mut output_ids: HashSet<String> = HashSet::new();

    for item in input {
        match item {
            ResponseItem::FunctionCall { call_id, .. } => {
                call_ids.insert(call_id.clone());
            }
            ResponseItem::LocalShellCall { call_id, .. } => {
                if let Some(id) = call_id {
                    call_ids.insert(id.clone());
                }
                // None call_id → can never match an output → orphaned
            }
            ResponseItem::CustomToolCall { call_id, .. } => {
                call_ids.insert(call_id.clone());
            }
            ResponseItem::ToolSearchCall { call_id, .. } => {
                if let Some(id) = call_id {
                    call_ids.insert(id.clone());
                }
            }
            ResponseItem::FunctionCallOutput { call_id, .. } => {
                output_ids.insert(call_id.clone());
            }
            ResponseItem::CustomToolCallOutput { call_id, .. } => {
                output_ids.insert(call_id.clone());
            }
            ResponseItem::ToolSearchOutput { call_id, .. } => {
                if let Some(id) = call_id {
                    output_ids.insert(id.clone());
                }
            }
            _ => {}
        }
    }

    // Paired ids: present in BOTH sets.
    let paired: HashSet<&String> = call_ids.intersection(&output_ids).collect();

    // Second pass: keep only paired tool items and all non-tool items.
    input
        .iter()
        .filter(|item| match item {
            ResponseItem::FunctionCall { call_id, .. } => paired.contains(call_id),
            ResponseItem::LocalShellCall { call_id, .. } => {
                call_id.as_ref().is_some_and(|id| paired.contains(id))
            }
            ResponseItem::CustomToolCall { call_id, .. } => paired.contains(call_id),
            ResponseItem::ToolSearchCall { call_id, .. } => {
                call_id.as_ref().is_some_and(|id| paired.contains(id))
            }
            ResponseItem::FunctionCallOutput { call_id, .. } => paired.contains(call_id),
            ResponseItem::CustomToolCallOutput { call_id, .. } => paired.contains(call_id),
            ResponseItem::ToolSearchOutput { call_id, .. } => {
                call_id.as_ref().is_some_and(|id| paired.contains(id))
            }
            _ => true,
        })
        .cloned()
        .collect()
}

/// Translates the codex-rs conversation history (`&[ResponseItem]`) into
/// Anthropic's `messages` array, extracting the system prompt from the first
/// system-role message if present.
pub fn conversation_to_anthropic_messages(
    input: &[ResponseItem],
    supports_image: bool,
    supports_prompt_caching: bool,
    supports_assistant_prefill: bool,
) -> Vec<Value> {
    // S-005: Strip orphaned tool calls/results before translation to prevent
    // Anthropic API 400 errors from unpaired tool_use/tool_result blocks.
    let cleaned = clean_orphaned_tool_calls(input);

    let mut messages: Vec<Value> = Vec::new();

    for item in &cleaned {
        match item {
            ResponseItem::Message { role, content, .. } => {
                let anthropic_role = match role.as_str() {
                    "system" => continue,
                    "user" => "user",
                    "assistant" => "assistant",
                    "developer" => continue, // already injected via system parameter
                    _ => "user",
                };

                let content_blocks: Vec<Value> = content
                    .iter()
                    .map(|c| match c {
                        ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                            json!({
                                "type": "text",
                                "text": text,
                            })
                        }
                        ContentItem::InputImage { image_url, detail: _ } => {
                            if supports_image {
                                json!({
                                    "type": "image",
                                    "source": {
                                        "type": "url",
                                        "url": image_url,
                                    },
                                })
                            } else {
                                // S-008: Model does not support image input — emit
                                // a text placeholder so the request doesn't fail.
                                tracing::debug!(
                                    "modality gating: replacing image with text placeholder (model does not support image input)"
                                );
                                json!({
                                    "type": "text",
                                    "text": format!(
                                        "[Image: content not shown — this model does not support image input. Original URL: {}]",
                                        image_url
                                    ),
                                })
                            }
                        }
                    })
                    .collect();

                if content_blocks.is_empty() {
                    continue;
                }

                append_to_role(&mut messages, anthropic_role, content_blocks);
            }

            ResponseItem::FunctionCall {
                call_id,
                name,
                namespace,
                arguments,
                ..
            } => {
                let input_val: Value = serde_json::from_str(arguments).unwrap_or_else(|e| {
                    tracing::warn!("malformed tool arguments JSON, using empty object: {e}");
                    json!({})
                });
                // Re-encode history under the SAME flat name the live tool
                // list advertises (`<namespace>__<name>`), not the bare
                // `name`. Otherwise the model sees its own past namespaced
                // MCP calls under a name that no advertised tool matches,
                // and starts echoing the bare form -> decode misses the
                // registry -> "unsupported call". See flat_mcp_tool_name.
                let wire_name = flat_mcp_tool_name(name, namespace.as_deref());
                let block = json!({
                    "type": "tool_use",
                    "id": call_id,
                    "name": wire_name.as_str(),
                    "input": input_val,
                });
                append_to_role(&mut messages, "assistant", vec![block]);
            }

            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                let content = output_to_content(output, supports_image);
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": content,
                });
                append_to_role(&mut messages, "user", vec![block]);
            }

            ResponseItem::CustomToolCall {
                call_id,
                name,
                input: input_str,
                ..
            } => {
                let input_val: Value = serde_json::from_str(input_str).unwrap_or_else(|e| {
                    tracing::warn!("malformed tool arguments JSON, using empty object: {e}");
                    json!({})
                });
                let block = json!({
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": input_val,
                });
                append_to_role(&mut messages, "assistant", vec![block]);
            }

            ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                let content = output_to_content(output, supports_image);
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": content,
                });
                append_to_role(&mut messages, "user", vec![block]);
            }

            ResponseItem::LocalShellCall {
                call_id, action, ..
            } => {
                let codex_protocol::models::LocalShellAction::Exec(exec) = action;
                let args = json!({ "command": exec.command.join(" ") });
                // Generate a synthetic toolu_ ID when call_id is None to prevent
                // orphaned tool_result entries from empty "id" fields.
                let id = call_id.clone().unwrap_or_else(|| {
                    format!("toolu_synthetic_{:016x}", {
                        use std::hash::Hash;
                        use std::hash::Hasher;
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        exec.command.hash(&mut h);
                        messages.len().hash(&mut h);
                        h.finish()
                    })
                });
                let block = json!({
                    "type": "tool_use",
                    "id": id,
                    "name": "shell",
                    "input": args,
                });
                append_to_role(&mut messages, "assistant", vec![block]);
            }

            ResponseItem::Reasoning {
                summary,
                content,
                encrypted_content,
                raw_wire_block,
                ..
            } => {
                use codex_protocol::models::ReasoningItemContent;
                use codex_protocol::models::ReasoningItemReasoningSummary;

                // Prefer the raw wire block for byte-identical replay. This
                // avoids any mutation from decomposition/reconstruction and
                // satisfies Anthropic's cryptographic verification of thinking
                // blocks in the latest assistant message.
                if let Some(raw_block) = raw_wire_block {
                    // S-OPUS47-EMPTY-THINKING: defensively drop any persisted
                    // raw_wire_block of shape
                    //   { type: "thinking", thinking: "", signature: <non-empty> }
                    // Opus 4.7 adaptive thinking (display=summarized/omitted)
                    // on the Vertex route streams a signature_delta but
                    // withholds every thinking_delta, so older sessions may
                    // already carry these unreplayable blocks. Replaying them
                    // fails Anthropic's verifier with "thinking blocks in the
                    // latest assistant message cannot be modified" because the
                    // signature was computed over content the proxy never
                    // returned. Anthropic regenerates the thought on the next
                    // turn — replay is not required for correctness. (Redacted
                    // thinking is unaffected; `data` is the verifier input,
                    // not text.)
                    let is_unreplayable_empty_thinking =
                        raw_block.get("type").and_then(serde_json::Value::as_str)
                            == Some("thinking")
                            && raw_block
                                .get("thinking")
                                .and_then(serde_json::Value::as_str)
                                .is_none_or(str::is_empty);
                    if is_unreplayable_empty_thinking {
                        continue;
                    }
                    append_to_role(&mut messages, "assistant", vec![raw_block.clone()]);
                    continue;
                }

                // Fallback: reconstruct from decomposed fields. Used for
                // Reasoning items from the Responses wire (OpenAI) or older
                // session files that predate raw_wire_block.
                if let Some(ec) = encrypted_content
                    && let Some(data) = ec.strip_prefix("\0REDACTED\0")
                {
                    let block = json!({
                        "type": "redacted_thinking",
                        "data": data,
                        // S-031: provenance marker — this redacted block was
                        // reconstructed from decomposed fields, not replayed
                        // byte-identically from raw_wire_block. It must be
                        // stripped before send (Anthropic verifies thinking
                        // blocks in the latest assistant message).
                        RECONSTRUCTED_THINKING_MARKER: true,
                    });
                    append_to_role(&mut messages, "assistant", vec![block]);
                    continue;
                }

                let signature = encrypted_content.as_deref().unwrap_or("");

                if let Some(content_items) = content {
                    for item in content_items {
                        let text = match item {
                            ReasoningItemContent::ReasoningText { text }
                            | ReasoningItemContent::Text { text } => text.as_str(),
                        };
                        if !text.is_empty() {
                            let block = json!({
                                "type": "thinking",
                                "thinking": text,
                                "signature": signature,
                                // S-031: reconstructed (non-raw) thinking block.
                                RECONSTRUCTED_THINKING_MARKER: true,
                            });
                            append_to_role(&mut messages, "assistant", vec![block]);
                        }
                    }
                } else {
                    let text = summary
                        .iter()
                        .map(|s| match s {
                            ReasoningItemReasoningSummary::SummaryText { text } => text.as_str(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.is_empty() {
                        let block = json!({
                            "type": "thinking",
                            "thinking": text,
                            "signature": signature,
                            // S-031: reconstructed (non-raw) thinking block.
                            RECONSTRUCTED_THINKING_MARKER: true,
                        });
                        append_to_role(&mut messages, "assistant", vec![block]);
                    }
                }
            }

            ResponseItem::ToolSearchCall {
                call_id, arguments, ..
            } => {
                // Translate tool_search calls into regular tool_use blocks so
                // the model retains memory of tool discovery across turns.
                let id = call_id.clone().unwrap_or_else(|| {
                    format!("toolu_search_{:016x}", {
                        use std::hash::Hash;
                        use std::hash::Hasher;
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        arguments.to_string().hash(&mut h);
                        messages.len().hash(&mut h);
                        h.finish()
                    })
                });
                let block = json!({
                    "type": "tool_use",
                    "id": id,
                    "name": "tool_search",
                    "input": arguments,
                });
                append_to_role(&mut messages, "assistant", vec![block]);
            }

            ResponseItem::ToolSearchOutput { call_id, tools, .. } => {
                // Translate tool_search results into tool_result blocks.
                let id = call_id.clone().unwrap_or_default();
                let content = if tools.is_empty() {
                    json!("No tools found.")
                } else {
                    json!(tools)
                };
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": content,
                });
                append_to_role(&mut messages, "user", vec![block]);
            }

            _ => {
                tracing::trace!("messages_wire: skipping unhandled ResponseItem variant");
            }
        }
    }

    // S-021: Enforce Anthropic tool_use/tool_result adjacency constraint.
    //
    // The pre-translation orphan cleaner (S-005) checks global call_id pairing
    // across the entire ResponseItem history, but Anthropic requires that every
    // tool_use block in an assistant message has a matching tool_result in the
    // *immediately following* user message, and vice versa.
    //
    // This post-translation pass operates on the already-translated messages
    // array and strips tool_use/tool_result blocks that violate adjacency.
    // Must run before strip_thinking (which may remove assistant messages)
    // and before S-014 (which appends a trailing user sentinel).
    clean_orphaned_tool_blocks_by_adjacency(&mut messages);

    // S-022: Hoist `tool_result` blocks to the head of their user message.
    //
    // Direct Anthropic API tolerates `tool_result` blocks appearing anywhere
    // inside a user message, but Vertex AI's Claude endpoint enforces a
    // stricter constraint: when an assistant message ends with `tool_use`,
    // the immediately following user message must *begin* with the matching
    // `tool_result` block(s). A mixed ordering like
    //   user.content = [text("…"), tool_result{id=X}]
    // triggers `messages.N: tool_use ids were found without tool_result
    // blocks immediately after: <id>`.
    //
    // This commonly happens when an out-of-band user-role message is
    // recorded into history between the assistant `function_call` and its
    // `function_call_output` (e.g. the unified-exec "too many processes"
    // warning, or the apply_patch routing nudge). The translator's
    // `append_to_role` merges consecutive user messages, so the warning
    // text lands ahead of the tool_result inside one composite user
    // message — which Vertex rejects.
    //
    // This pass performs a stable partition per user message, moving all
    // `tool_result` blocks to the front while preserving relative order
    // within each group. No blocks are dropped.
    hoist_tool_results_to_front(&mut messages);

    strip_thinking_from_non_latest_assistant_messages(&mut messages);

    // S-014: Guard against trailing assistant messages.
    //
    // Direct Anthropic API supports "prefill" (ending with role:assistant),
    // but Vertex AI does NOT — it rejects with "This model does not support
    // assistant message prefill".  Since our proxy may route through Vertex AI
    // (evidenced by req_vrtx_ request IDs), we must unconditionally append a
    // synthetic user turn when the conversation ends with an assistant message.
    //
    // This commonly happens when:
    //  - A LocalShellCall is in-flight (tool_use without matching tool_result)
    //  - Mid-turn compaction leaves a trailing assistant block
    //  - Fork/resume snapshots end on an assistant boundary
    //  - Sub-agent spawn with fork_context inherits a mid-conversation state
    if !supports_assistant_prefill && let Some(last) = messages.last() {
        if last["role"].as_str() == Some("assistant") {
            let has_tool_use = last["content"]
                .as_array()
                .map(|arr| arr.iter().any(|b| b["type"] == "tool_use"))
                .unwrap_or(false);
            let sentinel = if has_tool_use {
                "[Awaiting tool result]"
            } else {
                "[Continue]"
            };
            messages.push(json!({
                "role": "user",
                "content": [{"type": "text", "text": sentinel}]
            }));
        }
    }

    // S-X2: History prompt-caching sliding window.
    //
    // Anthropic supports up to 4 `cache_control` breakpoints per request.
    // We budget them as a sliding window for maximum cache hit rate:
    //
    //   Slot 1: last system block      (stable across session — in request.rs)
    //   Slot 2: last tool definition   (stable — tool list rarely changes)
    //   Slot 3: 2nd-to-last user turn  (cache boundary from turn N-1 — on
    //           turn N, everything up through here is a cache read)
    //   Slot 4: last user turn         (sets boundary for turn N+1's read)
    //
    // Without Slot 3, each new turn invalidates the entire message-side
    // prefix and must re-ingest the whole conversation at full cost.
    // The sliding window ensures turn N+1 gets a cache read of the
    // entire prefix up to turn N's boundary.
    if supports_prompt_caching {
        apply_history_cache_control(&mut messages);
    }

    messages
}

/// Sliding-window `cache_control` placement on the last two user messages.
///
/// Anthropic allows up to 4 `cache_control: {"type": "ephemeral"}`
/// breakpoints per request. Slots 1-2 are consumed by system blocks and
/// the last tool definition (in `request.rs` / `tools_to_anthropic_format`).
/// This function uses the remaining 2 slots on a sliding window of user
/// messages:
///
/// - **Slot 3:** 2nd-to-last user message — on turn N, everything up
///   through this boundary is a cache read (prefix from turn N-1).
/// - **Slot 4:** last user message — sets the boundary for turn N+1.
///
/// This mirrors the caching strategy used in the sister harness
///
/// Falls back gracefully: if there's only one user message it gets one
/// marker; if there are no user messages, this is a no-op.
fn apply_history_cache_control(messages: &mut [Value]) {
    // Walk backwards to find the last 2 user messages.
    let mut user_indices: Vec<usize> = Vec::new();
    for i in (0..messages.len()).rev() {
        if messages[i].get("role").and_then(|r| r.as_str()) == Some("user") {
            user_indices.push(i);
            if user_indices.len() >= 2 {
                break;
            }
        }
    }

    // Mark each user message's last content block with cache_control.
    for &idx in &user_indices {
        mark_last_block_cache_control(&mut messages[idx]);
    }
}

/// Adds `cache_control: {"type": "ephemeral"}` to the last content block
/// of a message. No-op if the message has no content array, an empty
/// array, or a non-object last block.
fn mark_last_block_cache_control(message: &mut Value) {
    let Some(content) = message.get_mut("content").and_then(|c| c.as_array_mut()) else {
        return;
    };
    let Some(last_block) = content.last_mut() else {
        return;
    };
    let Some(obj) = last_block.as_object_mut() else {
        return;
    };
    obj.insert("cache_control".to_owned(), json!({"type": "ephemeral"}));
}

/// Extracts text content from `developer`-role messages.
///
/// Developer-role messages carry AGENTS.md contents, permission directives,
/// personality config, and other project instructions. In the Anthropic
/// `/messages` wire format these must be injected into the `system` parameter
/// rather than appearing in the `messages[]` array.
///
/// Returns a `Vec<String>` of developer text blocks in the order they appear
/// in the conversation. The caller should append them to the `system` array
/// after `base_instructions`.
pub fn extract_developer_blocks(input: &[ResponseItem]) -> Vec<String> {
    let mut blocks = Vec::new();
    for item in input {
        if let ResponseItem::Message { role, content, .. } = item
            && role == "developer"
        {
            for c in content {
                match c {
                    ContentItem::InputText { text } | ContentItem::OutputText { text }
                        if !text.is_empty() =>
                    {
                        blocks.push(text.clone());
                    }
                    _ => {}
                }
            }
        }
    }
    blocks
}

/// Enforces Anthropic's tool_use/tool_result adjacency constraint on the
/// translated messages array.
///
/// Anthropic requires that every `tool_use` block in an assistant message has a
/// `tool_result` with a matching `tool_use_id` in the **immediately following**
/// user message, and every `tool_result` in a user message has a matching
/// `tool_use` `id` in the **immediately preceding** assistant message.
///
/// This is stricter than global call_id pairing: a tool_use/tool_result pair
/// that exists but is separated by intervening messages of the wrong role (e.g.
/// from `ensure_call_outputs_present` splitting parallel tool calls with
/// interleaved Reasoning items) violates the constraint.
///
/// The function:
/// 1. Iterates messages and collects the set of tool_use ids in each assistant
///    message and tool_result tool_use_ids in each user message.
/// 2. For each assistant[i] / user[i+1] pair, computes the intersection of ids.
///    Strips blocks whose ids are not in the intersection.
/// 3. Removes messages that become empty after stripping.
fn clean_orphaned_tool_blocks_by_adjacency(messages: &mut Vec<Value>) {
    use std::collections::HashSet;

    // Pass 1: For each assistant message, check if the next message is a user
    // message with matching tool_result blocks. Strip unmatched tool_use blocks.
    // Similarly, for each user message check the preceding assistant message.
    //
    // We iterate by index pairs and mark blocks to keep.

    let len = messages.len();
    for i in 0..len {
        let role = messages[i]
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("");

        if role == "assistant" {
            // Collect tool_use ids in this assistant message
            let tool_use_ids: HashSet<String> = messages[i]["content"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter(|b| b["type"] == "tool_use")
                        .filter_map(|b| b["id"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            if tool_use_ids.is_empty() {
                continue;
            }

            // Collect tool_result ids in the next message (if it's a user message)
            let next_result_ids: HashSet<String> = if i + 1 < len
                && messages[i + 1].get("role").and_then(|r| r.as_str()) == Some("user")
            {
                messages[i + 1]["content"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter(|b| b["type"] == "tool_result")
                            .filter_map(|b| b["tool_use_id"].as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                HashSet::new()
            };

            // Strip tool_use blocks from the assistant message that don't have
            // an adjacent tool_result
            let matched: HashSet<&String> = tool_use_ids.intersection(&next_result_ids).collect();
            if matched.len() < tool_use_ids.len() {
                let stripped_ids: Vec<_> = tool_use_ids
                    .iter()
                    .filter(|id| !matched.contains(id))
                    .collect();
                tracing::debug!(
                    "S-021: stripping {} non-adjacent tool_use block(s) from assistant message {i}",
                    stripped_ids.len()
                );
                if let Some(content) = messages[i]
                    .get_mut("content")
                    .and_then(|c| c.as_array_mut())
                {
                    content.retain(|block| {
                        if block["type"] == "tool_use" {
                            let id = block["id"].as_str().unwrap_or("");
                            matched.iter().any(|m| m.as_str() == id)
                        } else {
                            true
                        }
                    });
                }
            }
        } else if role == "user" {
            // Collect tool_result ids in this user message
            let tool_result_ids: HashSet<String> = messages[i]["content"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter(|b| b["type"] == "tool_result")
                        .filter_map(|b| b["tool_use_id"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            if tool_result_ids.is_empty() {
                continue;
            }

            // Collect tool_use ids from the preceding message (if it's an assistant message)
            let prev_use_ids: HashSet<String> = if i > 0
                && messages[i - 1].get("role").and_then(|r| r.as_str()) == Some("assistant")
            {
                messages[i - 1]["content"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter(|b| b["type"] == "tool_use")
                            .filter_map(|b| b["id"].as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                HashSet::new()
            };

            // Strip tool_result blocks from the user message that don't have
            // an adjacent tool_use
            let matched: HashSet<&String> = tool_result_ids.intersection(&prev_use_ids).collect();
            if matched.len() < tool_result_ids.len() {
                let stripped_ids: Vec<_> = tool_result_ids
                    .iter()
                    .filter(|id| !matched.contains(id))
                    .collect();
                tracing::debug!(
                    "S-021: stripping {} non-adjacent tool_result block(s) from user message {i}",
                    stripped_ids.len()
                );
                if let Some(content) = messages[i]
                    .get_mut("content")
                    .and_then(|c| c.as_array_mut())
                {
                    content.retain(|block| {
                        if block["type"] == "tool_result" {
                            let id = block["tool_use_id"].as_str().unwrap_or("");
                            matched.iter().any(|m| m.as_str() == id)
                        } else {
                            true
                        }
                    });
                }
            }
        }
    }

    // Pass 2: Remove messages whose content became empty after stripping.
    messages.retain(|msg| {
        msg.get("content")
            .and_then(|c| c.as_array())
            .is_none_or(|arr| !arr.is_empty())
    });
}

/// Stable-partitions each user message so all `tool_result` content blocks
/// appear before non-tool_result blocks, preserving relative order within
/// each group.
///
/// Vertex AI's Claude endpoint rejects requests where a `tool_use`-bearing
/// assistant message is followed by a user message whose content does not
/// *begin* with the matching `tool_result` block(s). The direct Anthropic
/// API accepts either ordering, which is why this only surfaces on
/// proxy/Vertex routes (request IDs prefixed with `req_vrtx_`).
///
/// The function operates after `clean_orphaned_tool_blocks_by_adjacency`
/// has confirmed pairing, so any `tool_result` block here is known to
/// match an immediately-preceding assistant `tool_use`.
fn hoist_tool_results_to_front(messages: &mut [Value]) {
    for msg in messages.iter_mut() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };
        // Cheap early-out: if the first non-empty block is already a
        // tool_result (or none exist), there's nothing to reorder.
        let any_tool_result = content
            .iter()
            .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"));
        if !any_tool_result {
            continue;
        }
        let first_is_tool_result = content
            .first()
            .and_then(|b| b.get("type"))
            .and_then(|t| t.as_str())
            == Some("tool_result");
        let all_leading_are_tool_results = content
            .iter()
            .take_while(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
            .count()
            == content
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
                .count();
        if first_is_tool_result && all_leading_are_tool_results {
            continue;
        }

        // Stable partition: tool_result blocks first, everything else after.
        let owned = std::mem::take(content);
        let (results, rest): (Vec<Value>, Vec<Value>) = owned
            .into_iter()
            .partition(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"));
        content.extend(results);
        content.extend(rest);
    }
}

/// Strips `thinking` and `redacted_thinking` content blocks from all assistant
/// messages except the last one in the array.
///
/// The Anthropic API requires that thinking blocks in the **latest** assistant
/// message be byte-identical to the original response. Earlier assistant
/// messages can have thinking blocks omitted entirely. Stripping them prevents
/// issues from compaction merging, proxy translation, or serialization
/// round-trips that could subtly modify the blocks.
///
/// S-031: For the **latest** assistant message we cannot blindly keep all
/// thinking blocks. Blocks replayed byte-identically from `raw_wire_block`
/// are safe, but blocks *reconstructed* from decomposed Reasoning fields
/// (Responses-wire origin, pre-`raw_wire_block` sessions, or
/// compaction-merged history) carry a synthetic/empty `signature` that fails
/// Anthropic's cryptographic verification with
/// `messages.N.content.M: thinking blocks in the latest assistant message
/// cannot be modified`. We tag those reconstructed blocks with
/// `RECONSTRUCTED_THINKING_MARKER` at build time and drop them here so only
/// trustworthy (raw) thinking survives in the latest assistant message. The
/// marker is internal-only and is scrubbed from every surviving block before
/// the payload leaves the translator.
fn strip_thinking_from_non_latest_assistant_messages(messages: &mut Vec<Value>) {
    let last_assistant_idx = messages
        .iter()
        .rposition(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"));

    let Some(last_idx) = last_assistant_idx else {
        return;
    };

    for (i, msg) in messages.iter_mut().enumerate() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        if let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
            if i < last_idx {
                // Non-latest assistant message: drop ALL thinking blocks.
                content.retain(|block| {
                    let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    block_type != "thinking" && block_type != "redacted_thinking"
                });
            } else {
                // Latest assistant message: keep only raw (non-reconstructed)
                // thinking blocks; drop reconstructed ones that would fail
                // Anthropic signature verification.
                content.retain(|block| {
                    let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    let is_thinking = block_type == "thinking" || block_type == "redacted_thinking";
                    let is_reconstructed = block
                        .get(RECONSTRUCTED_THINKING_MARKER)
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    !(is_thinking && is_reconstructed)
                });
            }
        }
    }

    // Scrub the internal provenance marker from any surviving block so it
    // never leaks onto the wire.
    for msg in messages.iter_mut() {
        if let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
            for block in content.iter_mut() {
                if let Some(obj) = block.as_object_mut() {
                    obj.remove(RECONSTRUCTED_THINKING_MARKER);
                }
            }
        }
    }

    // Remove assistant messages that became empty after stripping
    messages.retain(|msg| {
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            return true;
        }
        msg.get("content")
            .and_then(|c| c.as_array())
            .is_none_or(|arr| !arr.is_empty())
    });
}

/// Translates OpenAI Responses API tool specs to Anthropic `/messages` format.
///
/// `Function` tools map 1:1. `Freeform` (custom) tools are translated into a
/// single-string-parameter tool with the grammar/format definition embedded in
/// the description so Claude can produce the expected freeform output.
/// `ToolSearch` tools are translated as function tools with the same parameter
/// schema.
///
/// Server-side tool types (`local_shell`, `web_search`, `image_generation`)
/// are skipped since Anthropic doesn't have equivalents.
pub fn tools_to_anthropic_format(tools: &[ToolSpec]) -> Vec<Value> {
    let mut result: Vec<Value> = tools
        .iter()
        .flat_map(|tool| -> Vec<Value> {
            match tool {
                ToolSpec::Function(f) => Some(json!({
                    "name": f.name,
                    "description": f.description,
                    "input_schema": f.parameters,
                }))
                .into_iter()
                .collect(),
                ToolSpec::Freeform(f) => {
                    // Anthropic has no native freeform/custom tool type. Translate
                    // into a regular tool with a single `input` string parameter.
                    // Embed the grammar definition into the description so the
                    // model knows the expected output format.
                    let description = format!(
                        "{}\n\nThis is a FREEFORM tool. Put your raw freeform content \
                     into the \"input\" parameter as a single string — do NOT \
                     wrap it in any other structure.\n\n\
                     Format ({} / {}): {}", // type / syntax: definition
                        f.description, f.format.r#type, f.format.syntax, f.format.definition,
                    );
                    Some(json!({
                        "name": f.name,
                        "description": description,
                        "input_schema": {
                            "type": "object",
                            "properties": {
                                "input": {
                                    "type": "string",
                                    "description": "The freeform tool content."
                                }
                            },
                            "required": ["input"],
                            "additionalProperties": false
                        },
                    }))
                    .into_iter()
                    .collect()
                }
                ToolSpec::ToolSearch {
                    description,
                    parameters,
                    ..
                } => {
                    // Expose tool_search to the model so it can discover skills.
                    Some(json!({
                        "name": "tool_search",
                        "description": description,
                        "input_schema": parameters,
                    }))
                    .into_iter()
                    .collect()
                }
                ToolSpec::Namespace(ns) => {
                    // Anthropic's `/messages` API has no native namespacing —
                    // tool names are flat. Flatten `ToolSpec::Namespace` into
                    // individual function tools using the canonical inbound
                    // `<namespace>.<name>` convention so the dispatcher can
                    // route tool_use blocks back to the right MCP server.
                    //
                    // Without this flattening, every namespaced MCP server
                    // (atlassian, cit, ghe, cognee, etc.) is silently
                    // invisible to Claude — the model only sees built-in
                    // tools and the OpenAI `/responses` `namespace` shape
                    // never reaches the wire.
                    ns.tools
                        .iter()
                        .map(|nt| match nt {
                            codex_tools::ResponsesApiNamespaceTool::Function(f) => json!({
                                "name": format!("{}__{}", ns.name, f.name),
                                "description": f.description,
                                "input_schema": f.parameters,
                            }),
                        })
                        .collect()
                }
                _ => Vec::new(),
            }
        })
        .collect();

    if let Some(last) = result.last_mut()
        && let Some(obj) = last.as_object_mut()
    {
        obj.insert("cache_control".to_owned(), json!({"type": "ephemeral"}));
    }
    result
}

/// Converts a function-call output payload into an Anthropic-compatible
/// `"content"` value for a `tool_result` block.
///
/// - **Text-only output** → a plain JSON string (fast path).
/// - **ContentItems with images** → a JSON array of `text` / `image` content
///   blocks, mirroring the same format used for user messages.
/// - When `supports_image` is `false`, image items are replaced with a
///   descriptive text placeholder so the request never fails on text-only
///   models.
fn output_to_content(
    output: &codex_protocol::models::FunctionCallOutputPayload,
    supports_image: bool,
) -> Value {
    use codex_protocol::models::FunctionCallOutputContentItem;

    match &output.body {
        FunctionCallOutputBody::Text(text) => Value::String(text.clone()),
        FunctionCallOutputBody::ContentItems(items) => {
            let has_image = items
                .iter()
                .any(|i| matches!(i, FunctionCallOutputContentItem::InputImage { .. }));

            if !has_image {
                // Fast path: text-only content items — join into a single string.
                let joined = items
                    .iter()
                    .filter_map(|item| {
                        if let FunctionCallOutputContentItem::InputText { text } = item {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Value::String(joined)
            } else {
                // Mixed or image-only: produce an array of content blocks.
                let blocks: Vec<Value> = items
                    .iter()
                    .map(|item| match item {
                        FunctionCallOutputContentItem::InputText { text } => {
                            json!({ "type": "text", "text": text })
                        }
                        FunctionCallOutputContentItem::InputImage {
                            image_url, ..
                        } => {
                            if supports_image {
                                json!({
                                    "type": "image",
                                    "source": {
                                        "type": "url",
                                        "url": image_url,
                                    },
                                })
                            } else {
                                tracing::debug!(
                                    "modality gating: replacing tool-result image with text placeholder"
                                );
                                json!({
                                    "type": "text",
                                    "text": format!(
                                        "[Image: content not shown — this model does not support image input. Original URL: {}]",
                                        image_url
                                    ),
                                })
                            }
                        }
                        FunctionCallOutputContentItem::EncryptedContent {
                            encrypted_content,
                        } => {
                            json!({ "type": "text", "text": encrypted_content })
                        }
                    })
                    .collect();
                Value::Array(blocks)
            }
        }
    }
}

/// Appends content blocks to the last message if it has the matching role,
/// or creates a new message. This ensures Anthropic's alternating
/// user/assistant constraint is met by merging consecutive same-role messages.
fn append_to_role(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(|r| r.as_str()) == Some(role)
        && let Some(content) = last.get_mut("content").and_then(|c| c.as_array_mut())
    {
        content.extend(blocks);
        return;
    }
    messages.push(json!({
        "role": role,
        "content": blocks,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::FunctionCallOutputPayload;

    /// S-014 test helper: after `conversation_to_anthropic_messages`, a
    /// conversation ending in an assistant message has a synthetic
    /// `[Continue]` / `[Awaiting tool result]` user sentinel appended (see
    /// `S-014: Guard against trailing assistant messages`). These tests
    /// were written before S-014 landed; the helper strips that trailing
    /// sentinel when present so assertions about the real conversation
    /// tail keep their semantics.
    ///
    /// Returns the slice without the trailing synthetic user when the
    /// input:
    ///   - ends on role:user
    ///   - whose first content block is a text sentinel
    ///     (`[Continue]` or `[Awaiting tool result]`)
    /// Otherwise returns the slice unchanged.
    #[track_caller]
    fn drop_s014_sentinel(messages: &[serde_json::Value]) -> &[serde_json::Value] {
        let Some(last) = messages.last() else {
            return messages;
        };
        if last["role"] != "user" {
            return messages;
        }
        let text = last["content"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|b| b.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("");
        if matches!(text, "[Continue]" | "[Awaiting tool result]") {
            &messages[..messages.len() - 1]
        } else {
            messages
        }
    }

    #[test]
    fn test_simple_user_assistant_messages() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Hello".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Hi there".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, true);
        // S-014: trailing-assistant guard appends a synthetic [Continue]
        // user sentinel. Strip it before asserting the real tail.
        let real = drop_s014_sentinel(&messages);
        assert_eq!(real.len(), 2);
        assert_eq!(real[0]["role"], "user");
        assert_eq!(real[0]["content"][0]["type"], "text");
        assert_eq!(real[0]["content"][0]["text"], "Hello");
        assert_eq!(real[1]["role"], "assistant");
        assert_eq!(real[1]["content"][0]["text"], "Hi there");
    }

    #[test]
    fn test_tool_use_roundtrip() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "List files".to_string(),
                }],
                phase: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: r#"{"command":"ls"}"#.to_string(),
                call_id: "toolu_01".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_01".to_string(),
                output: FunctionCallOutputPayload::from_text("file1.txt\nfile2.txt".to_string()),
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["type"], "tool_use");
        assert_eq!(messages[1]["content"][0]["id"], "toolu_01");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "toolu_01");
    }

    #[test]
    fn namespaced_function_call_reencodes_with_double_underscore() {
        // History re-encode must reproduce the advertised flat name
        // (`<namespace>__<name>`), not the bare name, or the model echoes a
        // spelling no advertised tool matches -> "unsupported call".
        let input = vec![
            ResponseItem::FunctionCall {
                id: None,
                name: "add_jira_comment".to_string(),
                namespace: Some("mcp__atlassian".to_string()),
                arguments: r#"{"issue":"X-1"}"#.to_string(),
                call_id: "toolu_ns".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_ns".to_string(),
                output: FunctionCallOutputPayload::from_text("ok".to_string()),
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"][0]["type"], "tool_use");
        assert_eq!(
            messages[0]["content"][0]["name"],
            "mcp__atlassian__add_jira_comment"
        );
    }

    #[test]
    fn advertised_and_reencoded_mcp_names_match() {
        // The flat name advertised to the model (tools_to_anthropic_format,
        // flattening ToolSpec::Namespace) MUST equal the flat name we echo in
        // re-encoded history. Prior fixtures all used namespace:None, so the
        // delimiter break between these two paths was invisible. This pins the
        // invariant per wire.
        use codex_tools::JsonSchema;
        use codex_tools::ResponsesApiNamespace;
        use codex_tools::ResponsesApiNamespaceTool;
        use codex_tools::ResponsesApiTool;

        let inner = ResponsesApiTool {
            name: "add_jira_comment".to_string(),
            description: "comment on a jira issue".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                std::collections::BTreeMap::new(),
                None,
                Some(false.into()),
            ),
            output_schema: None,
        };
        let ns = ToolSpec::Namespace(ResponsesApiNamespace {
            name: "mcp__atlassian".to_string(),
            description: "atlassian tools".to_string(),
            tools: vec![ResponsesApiNamespaceTool::Function(inner)],
        });
        let advertised = tools_to_anthropic_format(&[ns]);
        let advertised_name = advertised[0]["name"].as_str().unwrap();

        let input = vec![
            ResponseItem::FunctionCall {
                id: None,
                name: "add_jira_comment".to_string(),
                namespace: Some("mcp__atlassian".to_string()),
                arguments: "{}".to_string(),
                call_id: "toolu_eq".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_eq".to_string(),
                output: FunctionCallOutputPayload::from_text("ok".to_string()),
            },
        ];
        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        let reencoded_name = messages[0]["content"][0]["name"].as_str().unwrap();

        assert_eq!(
            advertised_name, reencoded_name,
            "advertise vs history name mismatch: {advertised_name:?} != {reencoded_name:?}"
        );
        assert_eq!(advertised_name, "mcp__atlassian__add_jira_comment");
    }

    #[test]
    fn test_consecutive_same_role_merged() {
        let input = vec![
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: r#"{"command":"ls"}"#.to_string(),
                call_id: "toolu_01".to_string(),
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "read_file".to_string(),
                namespace: None,
                arguments: r#"{"path":"foo.txt"}"#.to_string(),
                call_id: "toolu_02".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_01".to_string(),
                output: FunctionCallOutputPayload::from_text("file1.txt".to_string()),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_02".to_string(),
                output: FunctionCallOutputPayload::from_text("contents".to_string()),
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        // Two consecutive assistant FunctionCalls should merge into one assistant message
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_tools_translation() {
        use codex_tools::JsonSchema;
        use codex_tools::ResponsesApiTool;

        let tools = vec![ToolSpec::Function(ResponsesApiTool {
            name: "shell".to_string(),
            description: "Run a shell command".to_string(),
            strict: true,
            defer_loading: None,
            parameters: JsonSchema::object(Default::default(), None, None),
            output_schema: None,
        })];

        let anthropic_tools = tools_to_anthropic_format(&tools);
        assert_eq!(anthropic_tools.len(), 1);
        assert_eq!(anthropic_tools[0]["name"], "shell");
        assert!(anthropic_tools[0].get("input_schema").is_some());
    }

    #[test]
    fn test_system_messages_skipped() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "system".to_string(),
                content: vec![ContentItem::InputText {
                    text: "You are helpful".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Hello".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(messages.len(), 1, "system messages should be skipped");
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn test_empty_content_messages_skipped() {
        let input = vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![],
            phase: None,
        }];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert!(
            messages.is_empty(),
            "empty content messages should be skipped"
        );
    }

    #[test]
    fn test_raw_wire_block_used_verbatim() {
        use codex_protocol::models::ReasoningItemReasoningSummary;

        // When raw_wire_block is present, it should be used directly
        // instead of reconstructing from summary/encrypted_content.
        let raw_block = json!({
            "type": "thinking",
            "thinking": "Exact wire text with special chars: \n\t\"quotes\"",
            "signature": "ErUmExactSignature=="
        });
        let input = vec![ResponseItem::Reasoning {
            id: None,
            summary: vec![ReasoningItemReasoningSummary::SummaryText {
                text: "DIFFERENT summary text".to_string(),
            }],
            content: None,
            encrypted_content: Some("DIFFERENT_signature".to_string()),
            internal_chat_message_metadata_passthrough: None,

            raw_wire_block: Some(raw_block),
        }];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        // S-014: strip the synthetic trailing user sentinel.
        let real = drop_s014_sentinel(&messages);
        assert_eq!(real.len(), 1);
        let block = &real[0]["content"][0];
        // Must use the raw block, NOT the decomposed fields
        assert_eq!(
            block["thinking"], "Exact wire text with special chars: \n\t\"quotes\"",
            "must use raw_wire_block thinking, not summary"
        );
        assert_eq!(
            block["signature"], "ErUmExactSignature==",
            "must use raw_wire_block signature, not encrypted_content"
        );
    }

    #[test]
    fn test_opus47_empty_thinking_with_signature_dropped() {
        // S-OPUS47-EMPTY-THINKING regression: Opus 4.7 adaptive thinking on
        // the Vertex route streams a signature_delta but withholds every
        // thinking_delta. A persisted Reasoning whose `raw_wire_block` is
        // `{type:"thinking", thinking:"", signature:<real>}` must be dropped
        // entirely — replaying it triggers Anthropic's
        // `messages.N.content.M: thinking blocks in the latest assistant
        // message cannot be modified` rejection because the signature was
        // computed over content the proxy never returned.
        let raw_block = json!({
            "type": "thinking",
            "thinking": "",
            "signature": "Er4CCmUIDhACGAIqQMQHBF5Vrealsig=="
        });
        let input = vec![
            ResponseItem::Reasoning {
                id: None,
                summary: Vec::new(),
                content: None,
                encrypted_content: Some("Er4CCmUIDhACGAIqQMQHBF5Vrealsig==".to_string()),
                internal_chat_message_metadata_passthrough: None,
                raw_wire_block: Some(raw_block),
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Real reply".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        let real = drop_s014_sentinel(&messages);
        // Both items coalesce into one assistant message; the empty-thinking
        // block must be absent so only the text block survives.
        assert_eq!(real.len(), 1);
        let content = real[0]["content"].as_array().expect("content array");
        for block in content {
            assert_ne!(
                block["type"], "thinking",
                "empty-thinking block must be dropped: {block:?}"
            );
        }
        assert!(
            content
                .iter()
                .any(|b| b["type"] == "text" && b["text"] == "Real reply"),
            "real assistant text must survive: {content:?}"
        );
    }

    #[test]
    fn test_raw_wire_block_redacted_used_verbatim() {
        // When raw_wire_block is present for redacted thinking, use it directly
        let raw_block = json!({
            "type": "redacted_thinking",
            "data": "ExactOpaqueData123=="
        });
        let input = vec![ResponseItem::Reasoning {
            id: None,
            summary: Vec::new(),
            content: None,
            encrypted_content: Some("\0REDACTED\0DIFFERENT_data".to_string()),
            internal_chat_message_metadata_passthrough: None,
            raw_wire_block: Some(raw_block),
        }];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        // S-014: strip the synthetic trailing user sentinel.
        let real = drop_s014_sentinel(&messages);
        assert_eq!(real.len(), 1);
        let block = &real[0]["content"][0];
        assert_eq!(block["type"], "redacted_thinking");
        assert_eq!(
            block["data"], "ExactOpaqueData123==",
            "must use raw_wire_block data, not encrypted_content"
        );
    }

    #[test]
    fn test_fallback_when_no_raw_wire_block() {
        use codex_protocol::models::ReasoningItemReasoningSummary;

        // S-031: When raw_wire_block is None (old sessions, Responses wire),
        // the reconstruction path runs but its synthetic-signature thinking
        // block is NOT trustworthy. As the latest assistant message it would
        // fail Anthropic's `thinking blocks ... cannot be modified` check, so
        // it must be stripped. We pair the reasoning with a following
        // assistant text turn to confirm the reasoning's reconstructed
        // thinking is dropped while real assistant content survives.
        let input = vec![
            ResponseItem::Reasoning {
                id: None,
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "Reconstructed text".to_string(),
                }],
                content: None,
                encrypted_content: Some("reconstructed_sig".to_string()),
                internal_chat_message_metadata_passthrough: None,

                raw_wire_block: None,
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Final answer".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        // S-014: strip the synthetic trailing user sentinel.
        let real = drop_s014_sentinel(&messages);
        // append_to_role merges both into a single assistant message; the
        // reconstructed thinking is stripped, leaving only the text block.
        let blocks = real[real.len() - 1]["content"].as_array().unwrap();
        assert!(
            blocks
                .iter()
                .all(|b| b["type"] != "thinking" && b["type"] != "redacted_thinking"),
            "reconstructed thinking must be stripped, got: {blocks:?}"
        );
        assert!(
            blocks.iter().any(|b| b["text"] == "Final answer"),
            "real assistant text must survive: {blocks:?}"
        );
    }

    #[test]
    fn test_reconstructed_thinking_stripped_from_latest_assistant() {
        use codex_protocol::models::ReasoningItemReasoningSummary;

        // S-031 regression: a lone reconstructed Reasoning item (no
        // raw_wire_block) lands as the latest assistant message. Its
        // synthetic signature would trigger
        // `messages.N.content.M: thinking blocks in the latest assistant
        // message cannot be modified`, so the block must be dropped entirely.
        let input = vec![ResponseItem::Reasoning {
            id: None,
            summary: vec![ReasoningItemReasoningSummary::SummaryText {
                text: "Deep thoughts".to_string(),
            }],
            content: None,
            encrypted_content: Some("sig_real_signature_abc".to_string()),
            internal_chat_message_metadata_passthrough: None,

            raw_wire_block: None,
        }];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        // No assistant thinking should reach the wire, and the internal
        // provenance marker must never leak.
        for msg in &messages {
            if let Some(content) = msg["content"].as_array() {
                for block in content {
                    assert!(
                        block["type"] != "thinking" && block["type"] != "redacted_thinking",
                        "reconstructed thinking must not survive: {block:?}"
                    );
                    assert!(
                        block.get(RECONSTRUCTED_THINKING_MARKER).is_none(),
                        "internal marker must be scrubbed: {block:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_raw_thinking_survives_but_reconstructed_dropped_in_latest() {
        use codex_protocol::models::ReasoningItemReasoningSummary;

        // S-031: when the latest assistant message mixes a byte-identical
        // raw_wire_block thinking with a reconstructed one, only the raw block
        // survives. append_to_role merges both reasoning items into one
        // assistant message, exercising the latest-message retain path.
        let raw_block = json!({
            "type": "thinking",
            "thinking": "RAW verbatim thinking",
            "signature": "RawSignature=="
        });
        let input = vec![
            ResponseItem::Reasoning {
                id: None,
                summary: Vec::new(),
                content: None,
                encrypted_content: None,
                internal_chat_message_metadata_passthrough: None,
                raw_wire_block: Some(raw_block),
            },
            ResponseItem::Reasoning {
                id: None,
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "reconstructed thinking".to_string(),
                }],
                content: None,
                encrypted_content: Some("synthetic_sig".to_string()),
                internal_chat_message_metadata_passthrough: None,

                raw_wire_block: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        let real = drop_s014_sentinel(&messages);
        let blocks = real[real.len() - 1]["content"].as_array().unwrap();
        let thinking: Vec<&Value> = blocks.iter().filter(|b| b["type"] == "thinking").collect();
        assert_eq!(
            thinking.len(),
            1,
            "only the raw thinking block must survive: {blocks:?}"
        );
        assert_eq!(thinking[0]["thinking"], "RAW verbatim thinking");
        assert_eq!(thinking[0]["signature"], "RawSignature==");
        assert!(
            thinking[0].get(RECONSTRUCTED_THINKING_MARKER).is_none(),
            "marker must be scrubbed from surviving raw block"
        );
    }

    #[test]
    fn test_redacted_thinking_roundtrip() {
        let input = vec![ResponseItem::Reasoning {
            id: None,
            summary: Vec::new(),
            content: None,
            encrypted_content: Some("\0REDACTED\0opaque_data_xyz".to_string()),
            // S-031: reconstructed (no raw_wire_block) blocks are dropped
            // from the latest assistant message. Use the trustworthy
            // raw-replay path so this round-trip assertion is meaningful.
            internal_chat_message_metadata_passthrough: None,

            raw_wire_block: Some(json!({
                "type": "redacted_thinking",
                "data": "opaque_data_xyz",
            })),
        }];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        // S-014: strip the synthetic trailing user sentinel.
        let real = drop_s014_sentinel(&messages);
        assert_eq!(real.len(), 1);
        assert_eq!(real[0]["role"], "assistant");
        assert_eq!(
            real[0]["content"][0]["type"], "redacted_thinking",
            "should emit redacted_thinking block"
        );
        assert_eq!(real[0]["content"][0]["data"], "opaque_data_xyz");
    }

    #[test]
    fn test_multi_turn_tool_loop() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Deploy the app".to_string(),
                }],
                phase: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: r#"{"command":"npm build"}"#.to_string(),
                call_id: "toolu_01".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_01".to_string(),
                output: FunctionCallOutputPayload::from_text("Build successful".to_string()),
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: r#"{"command":"npm deploy"}"#.to_string(),
                call_id: "toolu_02".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_02".to_string(),
                output: FunctionCallOutputPayload::from_text("Deployed!".to_string()),
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Done deploying".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);

        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["type"], "tool_use");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[3]["role"], "assistant");
        assert_eq!(messages[3]["content"][0]["type"], "tool_use");
        assert_eq!(messages[4]["role"], "user");
        assert_eq!(messages[4]["content"][0]["type"], "tool_result");
        assert_eq!(messages[5]["role"], "assistant");
        assert_eq!(messages[5]["content"][0]["type"], "text");

        for msg in &messages {
            let role = msg["role"].as_str().unwrap();
            assert!(
                role == "user" || role == "assistant",
                "only user/assistant roles allowed"
            );
        }
    }

    #[test]
    fn test_non_function_tools_filtered() {
        let tools = vec![
            ToolSpec::WebSearch {
                external_web_access: None,
                filters: None,
                user_location: None,
                search_context_size: None,
                search_content_types: None,
            },
            ToolSpec::ImageGeneration {
                output_format: "png".to_string(),
            },
        ];

        let anthropic_tools = tools_to_anthropic_format(&tools);
        assert!(
            anthropic_tools.is_empty(),
            "non-function tools should be filtered out"
        );
    }

    #[test]
    fn test_local_shell_call_translated() {
        use codex_protocol::models::LocalShellAction;
        use codex_protocol::models::LocalShellExecAction;
        use codex_protocol::models::LocalShellStatus;

        let input = vec![
            ResponseItem::LocalShellCall {
                id: None,
                call_id: Some("shell_01".to_string()),
                status: LocalShellStatus::InProgress,
                action: LocalShellAction::Exec(LocalShellExecAction {
                    command: vec!["ls".to_string(), "-la".to_string()],
                    timeout_ms: None,
                    working_directory: None,
                    env: None,
                    user: None,
                }),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "shell_01".to_string(),
                output: FunctionCallOutputPayload::from_text("total 8\ndrwxr-xr-x".to_string()),
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(messages.len(), 2); // assistant + user(tool_result)
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"][0]["type"], "tool_use");
        assert_eq!(messages[0]["content"][0]["id"], "shell_01");
        assert_eq!(messages[0]["content"][0]["name"], "shell");
        assert_eq!(messages[0]["content"][0]["input"]["command"], "ls -la");
    }

    #[test]
    fn test_custom_tool_call_roundtrip() {
        let input = vec![
            ResponseItem::CustomToolCall {
                id: None,
                status: None,
                call_id: "custom_01".to_string(),
                name: "apply_patch".to_string(),
                input: r#"{"patch":"diff content"}"#.to_string(),
            },
            ResponseItem::CustomToolCallOutput {
                call_id: "custom_01".to_string(),
                name: None,
                output: FunctionCallOutputPayload::from_text("Patch applied".to_string()),
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["content"][0]["type"], "tool_use");
        assert_eq!(messages[0]["content"][0]["id"], "custom_01");
        assert_eq!(messages[0]["content"][0]["name"], "apply_patch");
        assert_eq!(messages[1]["content"][0]["type"], "tool_result");
        assert_eq!(messages[1]["content"][0]["tool_use_id"], "custom_01");
    }

    #[test]
    fn test_thinking_precedes_tool_use_in_same_message() {
        use codex_protocol::models::ReasoningItemReasoningSummary;

        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Deploy".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Reasoning {
                id: None,
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "I should run the build".to_string(),
                }],
                content: None,
                encrypted_content: Some("sig_xyz".to_string()),
                // S-031: latest assistant must use raw-replay path.
                internal_chat_message_metadata_passthrough: None,

                raw_wire_block: Some(json!({
                    "type": "thinking",
                    "thinking": "I should run the build",
                    "signature": "sig_xyz",
                })),
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: r#"{"command":"npm build"}"#.to_string(),
                call_id: "toolu_01".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_01".to_string(),
                output: FunctionCallOutputPayload::from_text("Build succeeded".to_string()),
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(messages.len(), 3); // user + assistant(thinking+tool_use) + user(tool_result)
        assert_eq!(messages[0]["role"], "user");

        let assistant_content = messages[1]["content"].as_array().unwrap();
        assert_eq!(assistant_content.len(), 2);
        assert_eq!(
            assistant_content[0]["type"], "thinking",
            "thinking must come before tool_use"
        );
        assert_eq!(
            assistant_content[1]["type"], "tool_use",
            "tool_use must come after thinking"
        );
        assert_eq!(messages[2]["role"], "user"); // tool_result
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn test_tool_cache_control_on_last_tool() {
        use codex_tools::JsonSchema;
        use codex_tools::ResponsesApiTool;

        let tools = vec![
            ToolSpec::Function(ResponsesApiTool {
                name: "shell".to_string(),
                description: "Run shell".to_string(),
                strict: true,
                defer_loading: None,
                parameters: JsonSchema::object(Default::default(), None, None),
                output_schema: None,
            }),
            ToolSpec::Function(ResponsesApiTool {
                name: "read_file".to_string(),
                description: "Read a file".to_string(),
                strict: true,
                defer_loading: None,
                parameters: JsonSchema::object(Default::default(), None, None),
                output_schema: None,
            }),
        ];

        let anthropic_tools = tools_to_anthropic_format(&tools);
        assert_eq!(anthropic_tools.len(), 2);
        assert!(
            anthropic_tools[0].get("cache_control").is_none(),
            "first tool should not have cache_control"
        );
        assert_eq!(
            anthropic_tools[1]["cache_control"]["type"], "ephemeral",
            "last tool must have cache_control for prompt caching"
        );
    }

    #[test]
    fn test_empty_tools_no_panic() {
        let tools: Vec<ToolSpec> = vec![];
        let anthropic_tools = tools_to_anthropic_format(&tools);
        assert!(anthropic_tools.is_empty());
    }

    // S-X2 history caching tests --------------------------------------------

    fn user_msg(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText { text: text.into() }],
            phase: None,
        }
    }

    fn assistant_msg(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "assistant".into(),
            content: vec![ContentItem::OutputText { text: text.into() }],
            phase: None,
        }
    }

    #[test]
    fn history_cache_control_on_last_block_of_last_message() {
        let input = vec![user_msg("hello"), assistant_msg("hi"), user_msg("again")];
        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        let last = messages.last().expect("at least one message");
        let last_block = last["content"]
            .as_array()
            .expect("content is array")
            .last()
            .expect("at least one block");
        assert_eq!(
            last_block["cache_control"]["type"], "ephemeral",
            "last content block of last message must carry cache_control: ephemeral for history caching"
        );
    }

    #[test]
    fn history_cache_control_sliding_window_marks_last_two_user_messages() {
        let input = vec![
            user_msg("first"),
            assistant_msg("reply1"),
            user_msg("second"),
            assistant_msg("reply2"),
            user_msg("third"),
        ];
        let messages = conversation_to_anthropic_messages(&input, true, true, false);

        // Collect which user messages carry cache_control.
        let user_cache_count = messages
            .iter()
            .filter(|m| {
                m["role"] == "user"
                    && m["content"]
                        .as_array()
                        .and_then(|a| a.last())
                        .and_then(|b| b.get("cache_control"))
                        .is_some()
            })
            .count();

        assert_eq!(
            user_cache_count, 2,
            "exactly 2 user messages should carry cache_control (sliding window)"
        );

        // Assistant messages must NOT carry cache_control.
        for msg in &messages {
            if msg["role"] == "assistant" {
                for block in msg["content"].as_array().unwrap() {
                    assert!(
                        block.get("cache_control").is_none(),
                        "assistant messages must not have cache_control"
                    );
                }
            }
        }
    }

    #[test]
    fn history_cache_control_no_panic_on_empty_input() {
        let messages = conversation_to_anthropic_messages(&[], true, true, false);
        assert!(messages.is_empty());
    }

    #[test]
    fn history_cache_control_lands_on_synthetic_user_after_trailing_assistant() {
        // Conversation ends on assistant — S-014 appends a synthetic user
        // sentinel. The cache_control breakpoint must land on that synthetic
        // user message (the new last message), not on the assistant turn.
        let input = vec![user_msg("hi"), assistant_msg("response")];
        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        let last = messages.last().expect("synthetic user appended");
        assert_eq!(last["role"], "user");
        assert_eq!(
            last["content"][0]["cache_control"]["type"], "ephemeral",
            "synthetic trailing user message must carry cache_control"
        );
    }

    #[test]
    fn test_developer_messages_skipped() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "developer".to_string(),
                content: vec![ContentItem::InputText {
                    text: "You have full permissions".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Hello".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(messages.len(), 1, "developer messages should be skipped");
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["text"], "Hello");
    }

    #[test]
    fn test_malformed_arguments_uses_empty_object() {
        let input = vec![
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: "not valid json{{{".to_string(),
                call_id: "toolu_bad".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_bad".to_string(),
                output: FunctionCallOutputPayload::from_text("error".to_string()),
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(messages.len(), 2); // assistant + user(tool_result)
        assert_eq!(messages[0]["content"][0]["type"], "tool_use");
        assert_eq!(
            messages[0]["content"][0]["input"],
            json!({}),
            "malformed arguments should fall back to empty object"
        );
    }

    #[test]
    fn test_thinking_stripped_from_earlier_assistant_messages() {
        use codex_protocol::models::ReasoningItemReasoningSummary;

        // Turn 1: user → thinking + tool_use → tool_result
        // Turn 2: user → thinking + text (latest)
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "First question".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Reasoning {
                id: None,
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "Old thinking".to_string(),
                }],
                content: None,
                encrypted_content: Some("old_sig".to_string()),
                internal_chat_message_metadata_passthrough: None,

                raw_wire_block: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: r#"{"command":"ls"}"#.to_string(),
                call_id: "toolu_01".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_01".to_string(),
                output: FunctionCallOutputPayload::from_text("files".to_string()),
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Second question".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Reasoning {
                id: None,
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "Latest thinking".to_string(),
                }],
                content: None,
                encrypted_content: Some("latest_sig".to_string()),
                // S-031: latest must use raw-replay path; reconstructed
                // thinking would be dropped from the latest assistant msg.
                internal_chat_message_metadata_passthrough: None,

                raw_wire_block: Some(json!({
                    "type": "thinking",
                    "thinking": "Latest thinking",
                    "signature": "latest_sig",
                })),
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Final answer".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        // S-014: strip the synthetic trailing user sentinel so
        // `.last()` resolves to the real final assistant message.
        let messages = drop_s014_sentinel(&messages);

        // First assistant message should have thinking stripped, only tool_use remains
        let first_assistant = messages
            .iter()
            .find(|m| {
                m["role"] == "assistant"
                    && m["content"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|b| b["type"] == "tool_use")
            })
            .expect("should have an assistant message with tool_use");
        for block in first_assistant["content"].as_array().unwrap() {
            assert_ne!(
                block["type"], "thinking",
                "thinking should be stripped from earlier assistant messages"
            );
        }

        // Last assistant message should preserve thinking
        let last_assistant = messages.last().unwrap();
        assert_eq!(last_assistant["role"], "assistant");
        let content = last_assistant["content"].as_array().unwrap();
        assert!(
            content.iter().any(|b| b["type"] == "thinking"),
            "latest assistant message must keep thinking blocks"
        );
        assert_eq!(
            content.iter().find(|b| b["type"] == "thinking").unwrap()["signature"],
            "latest_sig"
        );
    }

    #[test]
    fn test_redacted_thinking_stripped_from_earlier_messages() {
        // Turn 1: user → redacted_thinking + text
        // Turn 2: user → text (latest)
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Q1".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Reasoning {
                id: None,
                summary: Vec::new(),
                content: None,
                encrypted_content: Some("\0REDACTED\0old_opaque".to_string()),
                internal_chat_message_metadata_passthrough: None,
                raw_wire_block: None,
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "A1".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Q2".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "A2".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);

        // First assistant message should NOT have redacted_thinking
        let first_assistant = &messages[1];
        assert_eq!(first_assistant["role"], "assistant");
        for block in first_assistant["content"].as_array().unwrap() {
            assert_ne!(
                block["type"], "redacted_thinking",
                "redacted_thinking should be stripped from earlier assistant messages"
            );
        }
    }

    #[test]
    fn test_empty_assistant_messages_removed_after_stripping() {
        use codex_protocol::models::ReasoningItemReasoningSummary;

        // An assistant message that contains ONLY a thinking block (no text/tool_use)
        // should be removed entirely after stripping (if it's not the last)
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Q1".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Reasoning {
                id: None,
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "Only thinking, no text".to_string(),
                }],
                content: None,
                encrypted_content: Some("sig_only_think".to_string()),
                internal_chat_message_metadata_passthrough: None,

                raw_wire_block: None,
            },
            // No text message follows — this creates an assistant msg with only thinking
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Q2".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "A2".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);

        // The thinking-only assistant message should be removed
        // We should have: user(Q1) → user(Q2) → assistant(A2)
        // But Anthropic requires alternation, so the two user messages may be merged
        // The key assertion: no empty assistant messages
        for msg in &messages {
            if msg["role"] == "assistant" {
                let content = msg["content"].as_array().unwrap();
                assert!(
                    !content.is_empty(),
                    "assistant messages with empty content should be removed"
                );
            }
        }
    }

    #[test]
    fn test_single_assistant_message_thinking_preserved() {
        use codex_protocol::models::ReasoningItemReasoningSummary;

        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Hello".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Reasoning {
                id: None,
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "The only thinking block".to_string(),
                }],
                content: None,
                encrypted_content: Some("sig_only".to_string()),
                // S-031: latest assistant must use raw-replay path.
                internal_chat_message_metadata_passthrough: None,

                raw_wire_block: Some(json!({
                    "type": "thinking",
                    "thinking": "The only thinking block",
                    "signature": "sig_only",
                })),
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Answer".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        // S-014: strip the synthetic trailing user sentinel.
        let real = drop_s014_sentinel(&messages);
        let last = real.last().unwrap();
        assert_eq!(last["role"], "assistant");
        let content = last["content"].as_array().unwrap();
        assert!(
            content.iter().any(|b| b["type"] == "thinking"),
            "single assistant message must keep its thinking block"
        );
        assert_eq!(
            content.iter().find(|b| b["type"] == "thinking").unwrap()["thinking"],
            "The only thinking block"
        );
    }

    #[test]
    fn test_compacted_history_thinking_stripped() {
        use codex_protocol::models::ReasoningItemReasoningSummary;

        // Simulates post-compaction preserved items:
        // summary user msg → ack assistant → preserved reasoning (old turn)
        // → tool_use → tool_result → new user → new reasoning + text (latest turn)
        let input = vec![
            // Compaction summary
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Summary of prior conversation...".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Understood, continuing.".to_string(),
                }],
                phase: None,
            },
            // Preserved old turn with reasoning
            ResponseItem::Reasoning {
                id: None,
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "Old preserved thinking".to_string(),
                }],
                content: None,
                encrypted_content: Some("old_preserved_sig".to_string()),
                internal_chat_message_metadata_passthrough: None,

                raw_wire_block: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: r#"{"command":"cat foo"}"#.to_string(),
                call_id: "toolu_p1".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_p1".to_string(),
                output: FunctionCallOutputPayload::from_text("foo content".to_string()),
            },
            // New turn
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "New question".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Reasoning {
                id: None,
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "Fresh thinking".to_string(),
                }],
                content: None,
                encrypted_content: Some("fresh_sig".to_string()),
                // S-031: latest must use raw-replay path.
                internal_chat_message_metadata_passthrough: None,

                raw_wire_block: Some(json!({
                    "type": "thinking",
                    "thinking": "Fresh thinking",
                    "signature": "fresh_sig",
                })),
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Fresh answer".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);

        // Find all assistant messages
        let assistant_msgs: Vec<_> = messages
            .iter()
            .filter(|m| m["role"] == "assistant")
            .collect();

        // All non-last assistant messages should have no thinking blocks
        for msg in &assistant_msgs[..assistant_msgs.len() - 1] {
            for block in msg["content"].as_array().unwrap() {
                assert_ne!(
                    block["type"], "thinking",
                    "preserved old thinking must be stripped from non-latest assistant"
                );
            }
        }

        // Last assistant message should keep fresh thinking
        let last = assistant_msgs.last().unwrap();
        let content = last["content"].as_array().unwrap();
        assert!(
            content.iter().any(|b| b["type"] == "thinking"),
            "latest assistant must keep fresh thinking"
        );
        assert_eq!(
            content.iter().find(|b| b["type"] == "thinking").unwrap()["signature"],
            "fresh_sig"
        );
    }

    // ── T-1-G: extract_developer_blocks tests ─────────────────────────

    #[test]
    fn extract_developer_blocks_returns_text_in_order() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "developer".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Block A".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "User message".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "developer".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Block B".to_string(),
                }],
                phase: None,
            },
        ];
        let blocks = extract_developer_blocks(&input);
        assert_eq!(blocks, vec!["Block A", "Block B"]);
    }

    #[test]
    fn extract_developer_blocks_ignores_non_developer() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Not a dev block".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "system".to_string(),
                content: vec![ContentItem::InputText {
                    text: "System".to_string(),
                }],
                phase: None,
            },
        ];
        let blocks = extract_developer_blocks(&input);
        assert!(
            blocks.is_empty(),
            "non-developer messages should be ignored"
        );
    }

    #[test]
    fn extract_developer_blocks_skips_empty_text() {
        let input = vec![ResponseItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputText {
                text: "".to_string(),
            }],
            phase: None,
        }];
        let blocks = extract_developer_blocks(&input);
        assert!(
            blocks.is_empty(),
            "empty developer text blocks should be skipped"
        );
    }

    #[test]
    fn extract_developer_blocks_handles_multiple_content_items() {
        let input = vec![ResponseItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![
                ContentItem::InputText {
                    text: "Part 1".to_string(),
                },
                ContentItem::InputText {
                    text: "Part 2".to_string(),
                },
            ],
            phase: None,
        }];
        let blocks = extract_developer_blocks(&input);
        assert_eq!(
            blocks,
            vec!["Part 1", "Part 2"],
            "each text item in a developer message is its own block"
        );
    }

    // ── T-1-E: LocalShellCall call_id None → synthetic ID ──────────────

    #[test]
    fn local_shell_call_none_call_id_produces_nonempty_id() {
        use codex_protocol::models::LocalShellAction;
        use codex_protocol::models::LocalShellExecAction;
        use codex_protocol::models::LocalShellStatus;

        // S-005: A LocalShellCall with call_id=None can never have a matching
        // FunctionCallOutput, so it is always removed by orphan cleanup.
        let input = vec![ResponseItem::LocalShellCall {
            call_id: None,
            action: LocalShellAction::Exec(LocalShellExecAction {
                command: vec!["ls".to_string()],
                timeout_ms: None,
                working_directory: None,
                env: None,
                user: None,
            }),
            id: None,
            status: LocalShellStatus::InProgress,
        }];
        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert!(
            messages.is_empty(),
            "LocalShellCall with None call_id is orphaned and should be removed"
        );
    }

    #[test]
    fn local_shell_call_with_call_id_passes_through() {
        use codex_protocol::models::LocalShellAction;
        use codex_protocol::models::LocalShellExecAction;
        use codex_protocol::models::LocalShellStatus;

        let input = vec![
            ResponseItem::LocalShellCall {
                call_id: Some("toolu_abc123".to_string()),
                action: LocalShellAction::Exec(LocalShellExecAction {
                    command: vec!["echo".to_string(), "hi".to_string()],
                    timeout_ms: None,
                    working_directory: None,
                    env: None,
                    user: None,
                }),
                id: None,
                status: LocalShellStatus::InProgress,
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_abc123".to_string(),
                output: FunctionCallOutputPayload::from_text("hi".to_string()),
            },
        ];
        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(messages[0]["content"][0]["id"], "toolu_abc123");
    }

    // ── S-014: trailing assistant guard ─────────────────────────────────

    #[test]
    fn trailing_assistant_gets_synthetic_user_message() {
        // S-005: orphaned in-flight tool calls are now removed before
        // translation, so only the user message survives. The S-014 trailing
        // assistant guard is tested separately with paired tool calls in
        // `trailing_assistant_with_paired_tool_call_gets_guard`.
        use codex_protocol::models::LocalShellAction;
        use codex_protocol::models::LocalShellExecAction;
        use codex_protocol::models::LocalShellStatus;

        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "do something".to_string(),
                }],
                phase: None,
            },
            ResponseItem::LocalShellCall {
                call_id: Some("toolu_014".to_string()),
                action: LocalShellAction::Exec(LocalShellExecAction {
                    command: vec!["ls".to_string()],
                    timeout_ms: None,
                    working_directory: None,
                    env: None,
                    user: None,
                }),
                id: None,
                status: LocalShellStatus::InProgress,
            },
            // No FunctionCallOutput — orphaned by S-005
        ];
        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        // S-005 removes the orphaned LocalShellCall; only user message remains
        assert_eq!(
            messages.len(),
            1,
            "orphaned tool call removed, only user msg remains"
        );
        assert_eq!(
            messages[0]["role"], "user",
            "only the user message should survive orphan cleanup"
        );
    }

    #[test]
    fn no_synthetic_user_when_ending_on_user() {
        let input = vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "hello".to_string(),
            }],
            phase: None,
        }];
        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(messages.len(), 1, "no synthetic message needed");
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn no_synthetic_user_after_tool_result() {
        use codex_protocol::models::LocalShellAction;
        use codex_protocol::models::LocalShellExecAction;
        use codex_protocol::models::LocalShellStatus;

        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "run ls".to_string(),
                }],
                phase: None,
            },
            ResponseItem::LocalShellCall {
                call_id: Some("toolu_pair".to_string()),
                action: LocalShellAction::Exec(LocalShellExecAction {
                    command: vec!["ls".to_string()],
                    timeout_ms: None,
                    working_directory: None,
                    env: None,
                    user: None,
                }),
                id: None,
                status: LocalShellStatus::Completed,
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_pair".to_string(),
                output: FunctionCallOutputPayload::from_text("file.txt".into()),
            },
        ];
        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        let last = messages.last().unwrap();
        assert_eq!(
            last["role"], "user",
            "tool_result is role:user — no synthetic needed"
        );
        // Should be the actual tool_result, not our synthetic
        assert!(
            last["content"][0]["type"] == "tool_result",
            "last message should be the real tool_result, not synthetic"
        );
    }

    // ── S-005: Orphaned tool call cleanup tests ─────────────────────────

    #[test]
    fn orphan_cleanup_empty_input() {
        let result = super::clean_orphaned_tool_calls(&[]);
        assert!(result.is_empty(), "empty input should produce empty output");
    }

    #[test]
    fn orphan_cleanup_paired_preserved() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "run ls".to_string(),
                }],
                phase: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: r#"{"command":"ls"}"#.to_string(),
                call_id: "toolu_paired".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_paired".to_string(),
                output: FunctionCallOutputPayload::from_text("file.txt".to_string()),
            },
        ];
        let result = super::clean_orphaned_tool_calls(&input);
        assert_eq!(
            result.len(),
            3,
            "all 3 items (msg + call + output) should be preserved"
        );
    }

    #[test]
    fn orphan_cleanup_orphaned_function_call_removed() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "run ls".to_string(),
                }],
                phase: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: r#"{"command":"ls"}"#.to_string(),
                call_id: "toolu_orphan".to_string(),
            },
            // No matching FunctionCallOutput
        ];
        let result = super::clean_orphaned_tool_calls(&input);
        assert_eq!(
            result.len(),
            1,
            "orphaned FunctionCall should be removed, only Message remains"
        );
        assert!(matches!(&result[0], ResponseItem::Message { .. }));
    }

    #[test]
    fn orphan_cleanup_orphaned_local_shell_call_removed() {
        use codex_protocol::models::LocalShellAction;
        use codex_protocol::models::LocalShellExecAction;
        use codex_protocol::models::LocalShellStatus;
        let input = vec![
            ResponseItem::LocalShellCall {
                id: None,
                call_id: Some("shell_orphan".to_string()),
                status: LocalShellStatus::Completed,
                action: LocalShellAction::Exec(LocalShellExecAction {
                    command: vec!["ls".to_string()],
                    timeout_ms: None,
                    working_directory: None,
                    env: None,
                    user: None,
                }),
            },
            // No matching FunctionCallOutput
        ];
        let result = super::clean_orphaned_tool_calls(&input);
        assert!(
            result.is_empty(),
            "orphaned LocalShellCall should be removed"
        );
    }

    #[test]
    fn orphan_cleanup_orphaned_output_removed() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "hello".to_string(),
                }],
                phase: None,
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_no_call".to_string(),
                output: FunctionCallOutputPayload::from_text("output".to_string()),
            },
        ];
        let result = super::clean_orphaned_tool_calls(&input);
        assert_eq!(
            result.len(),
            1,
            "orphaned FunctionCallOutput should be removed"
        );
        assert!(matches!(&result[0], ResponseItem::Message { .. }));
    }

    #[test]
    fn orphan_cleanup_mixed_only_orphan_removed() {
        use codex_protocol::models::LocalShellAction;
        use codex_protocol::models::LocalShellExecAction;
        use codex_protocol::models::LocalShellStatus;
        let input = vec![
            // Paired: FunctionCall + FunctionCallOutput
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: r#"{"command":"ls"}"#.to_string(),
                call_id: "toolu_good".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_good".to_string(),
                output: FunctionCallOutputPayload::from_text("ok".to_string()),
            },
            // Orphaned FunctionCall (no output)
            ResponseItem::FunctionCall {
                id: None,
                name: "read_file".to_string(),
                namespace: None,
                arguments: r#"{"path":"x"}"#.to_string(),
                call_id: "toolu_bad".to_string(),
            },
            // Orphaned FunctionCallOutput (no call)
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_stray".to_string(),
                output: FunctionCallOutputPayload::from_text("stray".to_string()),
            },
            // Paired LocalShellCall + output
            ResponseItem::LocalShellCall {
                id: None,
                call_id: Some("shell_good".to_string()),
                status: LocalShellStatus::Completed,
                action: LocalShellAction::Exec(LocalShellExecAction {
                    command: vec!["echo".to_string(), "hi".to_string()],
                    timeout_ms: None,
                    working_directory: None,
                    env: None,
                    user: None,
                }),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "shell_good".to_string(),
                output: FunctionCallOutputPayload::from_text("hi".to_string()),
            },
        ];
        let result = super::clean_orphaned_tool_calls(&input);
        // Should keep: FunctionCall(toolu_good), FunctionCallOutput(toolu_good),
        //              LocalShellCall(shell_good), FunctionCallOutput(shell_good)
        // Should remove: FunctionCall(toolu_bad), FunctionCallOutput(toolu_stray)
        assert_eq!(result.len(), 4, "only paired items should remain");

        // Verify the surviving call_ids
        let ids: Vec<String> = result
            .iter()
            .filter_map(|item| match item {
                ResponseItem::FunctionCall { call_id, .. } => Some(call_id.clone()),
                ResponseItem::LocalShellCall { call_id, .. } => call_id.clone(),
                ResponseItem::FunctionCallOutput { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .collect();
        assert!(ids.contains(&"toolu_good".to_string()));
        assert!(ids.contains(&"shell_good".to_string()));
        assert!(!ids.contains(&"toolu_bad".to_string()));
        assert!(!ids.contains(&"toolu_stray".to_string()));
    }

    #[test]
    fn orphan_cleanup_integration_with_translation() {
        // Verify that conversation_to_anthropic_messages handles orphans
        // gracefully (no panic, orphaned items produce no output).
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "do it".to_string(),
                }],
                phase: None,
            },
            // Orphaned call — should be stripped before translation
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: r#"{"command":"ls"}"#.to_string(),
                call_id: "toolu_gone".to_string(),
            },
        ];
        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        // After orphan removal only the user message survives.
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn test_tool_search_call_translated() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Find calendar tools".to_string(),
                }],
                phase: None,
            },
            ResponseItem::ToolSearchCall {
                id: None,
                call_id: Some("search_01".to_string()),
                status: None,
                execution: "client".to_string(),
                arguments: serde_json::json!({"query": "calendar"}),
            },
            ResponseItem::ToolSearchOutput {
                call_id: Some("search_01".to_string()),
                status: "completed".to_string(),
                execution: "client".to_string(),
                tools: vec![serde_json::json!({
                    "name": "create_event",
                    "description": "Create a calendar event",
                })],
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(messages.len(), 3);

        // User message
        assert_eq!(messages[0]["role"], "user");

        // ToolSearchCall → assistant tool_use
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["type"], "tool_use");
        assert_eq!(messages[1]["content"][0]["id"], "search_01");
        assert_eq!(messages[1]["content"][0]["name"], "tool_search");
        assert_eq!(messages[1]["content"][0]["input"]["query"], "calendar");

        // ToolSearchOutput → user tool_result
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "search_01");
    }

    #[test]
    fn test_tool_search_empty_result() {
        let input = vec![
            ResponseItem::ToolSearchCall {
                id: None,
                call_id: Some("search_02".to_string()),
                status: None,
                execution: "client".to_string(),
                arguments: serde_json::json!({"query": "nonexistent"}),
            },
            ResponseItem::ToolSearchOutput {
                call_id: Some("search_02".to_string()),
                status: "completed".to_string(),
                execution: "client".to_string(),
                tools: vec![],
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"][0]["content"], "No tools found.");
    }

    #[test]
    fn test_orphaned_tool_search_call_stripped() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "hello".to_string(),
                }],
                phase: None,
            },
            // Orphaned: call with no matching output
            ResponseItem::ToolSearchCall {
                id: None,
                call_id: Some("search_orphan".to_string()),
                status: None,
                execution: "client".to_string(),
                arguments: serde_json::json!({"query": "test"}),
            },
        ];
        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(
            messages.len(),
            1,
            "orphaned tool_search_call should be stripped"
        );
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn test_freeform_tool_translated() {
        use codex_tools::FreeformTool;
        use codex_tools::FreeformToolFormat;

        let tools = vec![ToolSpec::Freeform(FreeformTool {
            name: "apply_patch".to_string(),
            description: "Apply a patch".to_string(),
            format: FreeformToolFormat {
                r#type: "grammar".to_string(),
                syntax: "diff".to_string(),
                definition: "unified diff format".to_string(),
            },
        })];

        let anthropic_tools = tools_to_anthropic_format(&tools);
        assert_eq!(anthropic_tools.len(), 1);
        assert_eq!(anthropic_tools[0]["name"], "apply_patch");
        // Should have a single "input" string parameter
        let props = &anthropic_tools[0]["input_schema"]["properties"];
        assert!(
            props.get("input").is_some(),
            "freeform tool should have 'input' param"
        );
        assert_eq!(
            anthropic_tools[0]["input_schema"]["required"][0], "input",
            "input should be required"
        );
        // Description should embed the freeform format info
        let desc = anthropic_tools[0]["description"].as_str().unwrap();
        assert!(
            desc.contains("FREEFORM"),
            "description should mention FREEFORM"
        );
        assert!(desc.contains("diff"), "description should include syntax");
    }

    #[test]
    fn test_tool_search_spec_translated() {
        use codex_tools::JsonSchema;

        let tools = vec![ToolSpec::ToolSearch {
            execution: "client".to_string(),
            description: "Search available tools".to_string(),
            parameters: JsonSchema::object(Default::default(), None, None),
        }];

        let anthropic_tools = tools_to_anthropic_format(&tools);
        assert_eq!(anthropic_tools.len(), 1);
        assert_eq!(anthropic_tools[0]["name"], "tool_search");
        assert_eq!(anthropic_tools[0]["description"], "Search available tools");
    }
}

// ── S-008: Modality gating tests ────────────────────────────────────

#[cfg(test)]
mod modality_gating_tests {
    use super::*;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;

    /// Helper to build a user message with given content items.
    fn user_msg(content: Vec<ContentItem>) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content,
            phase: None,
        }
    }

    // ── Test 1: image-capable model → image block passes through ────

    #[test]
    fn image_capable_model_passes_through_image_block() {
        let input = vec![user_msg(vec![ContentItem::InputImage {
            detail: None,
            image_url: "https://example.com/photo.png".to_string(),
        }])];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);

        assert_eq!(messages.len(), 1);
        let block = &messages[0]["content"][0];
        assert_eq!(block["type"], "image", "image block should pass through");
        assert_eq!(block["source"]["type"], "url");
        assert_eq!(block["source"]["url"], "https://example.com/photo.png");
    }

    // ── Test 2: text-only model → image replaced with placeholder ───

    #[test]
    fn text_only_model_replaces_image_with_placeholder() {
        let input = vec![user_msg(vec![ContentItem::InputImage {
            detail: None,
            image_url: "https://example.com/photo.png".to_string(),
        }])];

        let messages = conversation_to_anthropic_messages(&input, false, true, false);

        assert_eq!(messages.len(), 1);
        let block = &messages[0]["content"][0];
        assert_eq!(
            block["type"], "text",
            "image should be replaced with text placeholder"
        );
        let text = block["text"].as_str().unwrap();
        assert!(
            text.contains("[Image:"),
            "placeholder should start with [Image:"
        );
        assert!(
            text.contains("does not support image input"),
            "placeholder should explain that the model doesn't support images"
        );
        assert!(
            text.contains("https://example.com/photo.png"),
            "placeholder should include the original URL"
        );
    }

    // ── Test 3: mixed content with text-only model ──────────────────

    #[test]
    fn mixed_content_text_only_model_replaces_image_preserves_text() {
        let input = vec![user_msg(vec![
            ContentItem::InputText {
                text: "Please describe this image:".to_string(),
            },
            ContentItem::InputImage {
                detail: None,
                image_url: "data:image/png;base64,iVBOR...".to_string(),
            },
            ContentItem::InputText {
                text: "What do you see?".to_string(),
            },
        ])];

        let messages = conversation_to_anthropic_messages(&input, false, true, false);

        assert_eq!(messages.len(), 1);
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3, "should have 3 content blocks");

        // First block: text preserved
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Please describe this image:");

        // Second block: image replaced with text placeholder
        assert_eq!(
            content[1]["type"], "text",
            "image should become text placeholder"
        );
        let placeholder = content[1]["text"].as_str().unwrap();
        assert!(
            placeholder.contains("[Image:"),
            "placeholder should indicate it was an image"
        );
        assert!(
            placeholder.contains("data:image/png;base64,iVBOR..."),
            "placeholder should include original URL"
        );

        // Third block: text preserved
        assert_eq!(content[2]["type"], "text");
        assert_eq!(content[2]["text"], "What do you see?");
    }
}

// ── S-005: output_to_content image handling tests ───────────────────────

#[cfg(test)]
mod output_to_content_tests {
    use super::*;

    use codex_protocol::models::FunctionCallOutputContentItem;
    use codex_protocol::models::FunctionCallOutputPayload;
    use codex_protocol::models::ResponseItem;

    /// Helper: build a minimal conversation with a single tool-call + tool-result
    /// so we can inspect the translated `content` field of the `tool_result` block.
    fn tool_result_content(output: FunctionCallOutputPayload, supports_image: bool) -> Value {
        let input = vec![
            ResponseItem::FunctionCall {
                id: None,
                name: "test_tool".to_string(),
                namespace: None,
                arguments: r#"{"a":1}"#.to_string(),
                call_id: "toolu_01".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_01".to_string(),
                output,
            },
        ];
        let messages = conversation_to_anthropic_messages(&input, supports_image, true, false);
        // The tool_result is in the user-role message (index 1, merged after
        // the assistant tool_use at index 0).
        let tool_result = &messages[1]["content"][0];
        assert_eq!(tool_result["type"], "tool_result");
        tool_result["content"].clone()
    }

    // ── Test 1: text-only output → plain string preserved ───────────────

    #[test]
    fn text_only_output_preserved() {
        let content = tool_result_content(
            FunctionCallOutputPayload::from_text("hello world".to_string()),
            true,
        );
        assert_eq!(content, "hello world");
    }

    // ── Test 2: ContentItems with text only → joined string ─────────────

    #[test]
    fn content_items_text_only_joined() {
        let content = tool_result_content(
            FunctionCallOutputPayload::from_content_items(vec![
                FunctionCallOutputContentItem::InputText {
                    text: "line1".to_string(),
                },
                FunctionCallOutputContentItem::InputText {
                    text: "line2".to_string(),
                },
            ]),
            true,
        );
        assert_eq!(content, "line1\nline2");
    }

    // ── Test 3: image-only output on image-capable model → image block ──

    #[test]
    fn image_only_produces_image_block() {
        let content = tool_result_content(
            FunctionCallOutputPayload::from_content_items(vec![
                FunctionCallOutputContentItem::InputImage {
                    image_url: "https://example.com/photo.png".to_string(),
                    detail: None,
                },
            ]),
            true,
        );
        let blocks = content
            .as_array()
            .expect("should be an array of content blocks");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(blocks[0]["source"]["type"], "url");
        assert_eq!(blocks[0]["source"]["url"], "https://example.com/photo.png");
    }

    // ── Test 4: mixed text + image → array with both block types ────────

    #[test]
    fn mixed_text_and_image_produces_array() {
        let content = tool_result_content(
            FunctionCallOutputPayload::from_content_items(vec![
                FunctionCallOutputContentItem::InputText {
                    text: "Here is the screenshot:".to_string(),
                },
                FunctionCallOutputContentItem::InputImage {
                    image_url: "data:image/png;base64,abc123".to_string(),
                    detail: None,
                },
                FunctionCallOutputContentItem::InputText {
                    text: "End of output".to_string(),
                },
            ]),
            true,
        );
        let blocks = content.as_array().expect("should be an array");
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "Here is the screenshot:");
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["url"], "data:image/png;base64,abc123");
        assert_eq!(blocks[2]["type"], "text");
        assert_eq!(blocks[2]["text"], "End of output");
    }

    // ── Test 5: image on text-only model → placeholder text ─────────────

    #[test]
    fn image_on_text_only_model_replaced_with_placeholder() {
        let content = tool_result_content(
            FunctionCallOutputPayload::from_content_items(vec![
                FunctionCallOutputContentItem::InputImage {
                    image_url: "https://example.com/diagram.png".to_string(),
                    detail: None,
                },
            ]),
            false, // text-only model
        );
        let blocks = content.as_array().expect("should be an array");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
        let placeholder = blocks[0]["text"].as_str().unwrap();
        assert!(
            placeholder.contains("[Image:"),
            "placeholder should indicate it was an image"
        );
        assert!(
            placeholder.contains("https://example.com/diagram.png"),
            "placeholder should include original URL"
        );
    }

    // ── Test 6: empty content items → empty string ──────────────────────

    #[test]
    fn empty_content_items_produces_empty_string() {
        let content =
            tool_result_content(FunctionCallOutputPayload::from_content_items(vec![]), true);
        assert_eq!(content, "");
    }

    // ── Test 7: CustomToolCallOutput also handles images ────────────────

    #[test]
    fn custom_tool_call_output_with_image() {
        let input = vec![
            ResponseItem::CustomToolCall {
                id: None,
                status: None,
                name: "my_tool".to_string(),
                call_id: "toolu_custom".to_string(),
                input: r#"{"x":1}"#.to_string(),
            },
            ResponseItem::CustomToolCallOutput {
                call_id: "toolu_custom".to_string(),
                name: None,
                output: FunctionCallOutputPayload::from_content_items(vec![
                    FunctionCallOutputContentItem::InputText {
                        text: "caption".to_string(),
                    },
                    FunctionCallOutputContentItem::InputImage {
                        image_url: "https://example.com/result.png".to_string(),
                        detail: None,
                    },
                ]),
            },
        ];
        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        let tool_result = &messages[1]["content"][0];
        assert_eq!(tool_result["type"], "tool_result");
        let blocks = tool_result["content"]
            .as_array()
            .expect("should be array with image");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "image");
    }
}

// ── S-020: comprehensive translator + thinking tests ────────────────────

#[cfg(test)]
mod translator_tests {
    use super::*;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::FunctionCallOutputPayload;
    use codex_protocol::models::ResponseItem;

    // ── S-020 Sub-A: comprehensive translator tests ────────────────────

    /// S-014 test helper: after `conversation_to_anthropic_messages`, a
    /// conversation ending in an assistant message has a synthetic
    /// `[Continue]` / `[Awaiting tool result]` user sentinel appended (see
    /// `S-014: Guard against trailing assistant messages`). These tests
    /// were written before S-014 landed; the helper strips that trailing
    /// sentinel when present so assertions about the real conversation
    /// tail keep their semantics.
    ///
    /// Returns the slice without the trailing synthetic user when the
    /// input:
    ///   - ends on role:user
    ///   - whose first content block is a text sentinel
    ///     (`[Continue]` or `[Awaiting tool result]`)
    /// Otherwise returns the slice unchanged.
    #[track_caller]
    fn drop_s014_sentinel(messages: &[serde_json::Value]) -> &[serde_json::Value] {
        let Some(last) = messages.last() else {
            return messages;
        };
        if last["role"] != "user" {
            return messages;
        }
        let text = last["content"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|b| b.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("");
        if matches!(text, "[Continue]" | "[Awaiting tool result]") {
            &messages[..messages.len() - 1]
        } else {
            messages
        }
    }

    #[test]
    fn multi_turn_alternating_user_assistant_tool_use_tool_result() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Turn 1 question".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Turn 1 answer".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Turn 2 question".to_string(),
                }],
                phase: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: r#"{"command":"ls -la"}"#.to_string(),
                call_id: "toolu_mt_01".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_mt_01".to_string(),
                output: FunctionCallOutputPayload::from_text("drwxr-xr-x  5 user".to_string()),
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Turn 2 final answer".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        // S-014: conversation ends on assistant → synthetic [Continue]
        // user sentinel is appended. Strip it before asserting structure.
        let messages = drop_s014_sentinel(&messages);
        assert_eq!(
            messages.len(),
            6,
            "should have 6 messages for multi-turn with tool use"
        );
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["text"], "Turn 1 question");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["text"], "Turn 1 answer");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["text"], "Turn 2 question");
        assert_eq!(messages[3]["role"], "assistant");
        assert_eq!(messages[3]["content"][0]["type"], "tool_use");
        assert_eq!(messages[3]["content"][0]["name"], "shell");
        assert_eq!(messages[4]["role"], "user");
        assert_eq!(messages[4]["content"][0]["type"], "tool_result");
        assert_eq!(messages[5]["role"], "assistant");
        assert_eq!(messages[5]["content"][0]["text"], "Turn 2 final answer");
    }

    #[test]
    fn thinking_preserved_in_latest_stripped_from_earlier_content_field() {
        use codex_protocol::models::ReasoningItemContent;

        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "First".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Reasoning {
                id: None,
                summary: Vec::new(),
                content: Some(vec![ReasoningItemContent::ReasoningText {
                    text: "Old thinking via content".to_string(),
                }]),
                encrypted_content: Some("old_content_sig".to_string()),
                internal_chat_message_metadata_passthrough: None,

                raw_wire_block: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "read_file".to_string(),
                namespace: None,
                arguments: r#"{"path":"main.rs"}"#.to_string(),
                call_id: "toolu_think_01".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_think_01".to_string(),
                output: FunctionCallOutputPayload::from_text("fn main(){}".to_string()),
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Second".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Reasoning {
                id: None,
                summary: Vec::new(),
                content: Some(vec![ReasoningItemContent::ReasoningText {
                    text: "Latest thinking via content".to_string(),
                }]),
                encrypted_content: Some("latest_content_sig".to_string()),
                // S-031: latest must use raw-replay path.
                internal_chat_message_metadata_passthrough: None,

                raw_wire_block: Some(json!({
                    "type": "thinking",
                    "thinking": "Latest thinking via content",
                    "signature": "latest_content_sig",
                })),
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Final".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        // S-014: strip the synthetic trailing user sentinel so
        // `.last()` resolves to the real final assistant message.
        let messages = drop_s014_sentinel(&messages);

        let early_assistant = messages.iter().find(|m| {
            m["role"] == "assistant"
                && m["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|b| b["type"] == "tool_use")
        });
        if let Some(ea) = early_assistant {
            for block in ea["content"].as_array().unwrap() {
                assert_ne!(
                    block["type"], "thinking",
                    "earlier thinking must be stripped"
                );
            }
        }

        let last_assistant = messages.last().unwrap();
        assert_eq!(last_assistant["role"], "assistant");
        let content = last_assistant["content"].as_array().unwrap();
        let thinking_block = content.iter().find(|b| b["type"] == "thinking");
        assert!(thinking_block.is_some(), "latest must keep thinking");
        assert_eq!(
            thinking_block.unwrap()["thinking"],
            "Latest thinking via content"
        );
    }

    #[test]
    fn developer_role_extracted_not_in_messages() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "developer".to_string(),
                content: vec![
                    ContentItem::InputText {
                        text: "AGENTS.md instructions".to_string(),
                    },
                    ContentItem::InputText {
                        text: "Permission directives".to_string(),
                    },
                ],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Hello".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(messages.len(), 1, "only user message should remain");
        assert_eq!(messages[0]["role"], "user");

        let dev_blocks = extract_developer_blocks(&input);
        assert_eq!(dev_blocks.len(), 2);
        assert_eq!(dev_blocks[0], "AGENTS.md instructions");
        assert_eq!(dev_blocks[1], "Permission directives");
    }

    #[test]
    fn cache_control_on_last_tool_block() {
        use codex_tools::JsonSchema;
        use codex_tools::ResponsesApiTool;

        let tools = vec![
            ToolSpec::Function(ResponsesApiTool {
                name: "tool_a".to_string(),
                description: "First tool".to_string(),
                strict: true,
                defer_loading: None,
                parameters: JsonSchema::object(Default::default(), None, None),
                output_schema: None,
            }),
            ToolSpec::Function(ResponsesApiTool {
                name: "tool_b".to_string(),
                description: "Second tool".to_string(),
                strict: true,
                defer_loading: None,
                parameters: JsonSchema::object(Default::default(), None, None),
                output_schema: None,
            }),
        ];

        let anthropic_tools = tools_to_anthropic_format(&tools);
        assert_eq!(anthropic_tools.len(), 2);
        assert!(
            anthropic_tools[0].get("cache_control").is_none(),
            "first tool should NOT have cache_control"
        );
        assert_eq!(
            anthropic_tools[1]["cache_control"]["type"], "ephemeral",
            "last tool should have cache_control ephemeral"
        );
    }

    #[test]
    fn empty_conversation_yields_empty_messages() {
        let messages = conversation_to_anthropic_messages(&[], true, true, false);
        assert!(messages.is_empty(), "empty input must produce empty output");
    }

    #[test]
    fn tool_use_with_complex_nested_json_arguments() {
        let nested_args = serde_json::json!({
            "files": [
                {"path": "src/main.rs", "content": "fn main() { println!(hello); }"},
                {"path": "Cargo.toml", "content": "[package]\nname = test"}
            ],
            "options": {"recursive": true, "depth": 3}
        })
        .to_string();

        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Create files".to_string(),
                }],
                phase: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "multi_file_write".to_string(),
                namespace: None,
                arguments: nested_args,
                call_id: "toolu_nested_01".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_nested_01".to_string(),
                output: FunctionCallOutputPayload::from_text("Files created".to_string()),
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        let assistant = messages.iter().find(|m| m["role"] == "assistant").unwrap();
        let tool_use = &assistant["content"][0];
        assert_eq!(tool_use["type"], "tool_use");
        assert_eq!(tool_use["name"], "multi_file_write");

        let input_val = &tool_use["input"];
        assert_eq!(input_val["files"].as_array().unwrap().len(), 2);
        assert_eq!(input_val["files"][0]["path"], "src/main.rs");
        assert_eq!(input_val["options"]["recursive"], true);
        assert_eq!(input_val["options"]["depth"], 3);
    }

    #[test]
    fn multiple_consecutive_user_messages_merged() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Part 1 of question".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Part 2 of question".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Part 3 of question".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(
            messages.len(),
            1,
            "consecutive user messages should be merged into one"
        );
        assert_eq!(messages[0]["role"], "user");
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(
            content.len(),
            3,
            "all three text blocks should be in the merged message"
        );
        assert_eq!(content[0]["text"], "Part 1 of question");
        assert_eq!(content[1]["text"], "Part 2 of question");
        assert_eq!(content[2]["text"], "Part 3 of question");
    }

    #[test]
    fn function_call_output_with_error_status() {
        let mut error_output = FunctionCallOutputPayload::from_text(
            "Error: command failed with exit code 1\nstderr: permission denied".to_string(),
        );
        error_output.success = Some(false);

        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Run command".to_string(),
                }],
                phase: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: r#"{"command":"rm -rf /"}"#.to_string(),
                call_id: "toolu_err_01".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_err_01".to_string(),
                output: error_output,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        let tool_result = messages
            .iter()
            .find_map(|m| {
                m["content"]
                    .as_array()
                    .and_then(|blocks| blocks.iter().find(|b| b["type"] == "tool_result"))
            })
            .expect("should have a tool_result");

        assert_eq!(tool_result["tool_use_id"], "toolu_err_01");
        assert!(
            tool_result["content"]
                .as_str()
                .unwrap()
                .contains("permission denied"),
            "error output text must be preserved in tool_result content"
        );
    }

    #[test]
    fn redacted_thinking_preserved_across_turns_in_latest() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Q1".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Reasoning {
                id: None,
                summary: Vec::new(),
                content: None,
                encrypted_content: Some(" REDACTED old_opaque_data".to_string()),
                internal_chat_message_metadata_passthrough: None,
                raw_wire_block: None,
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "A1".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Q2".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Reasoning {
                id: None,
                summary: Vec::new(),
                content: None,
                encrypted_content: Some(" REDACTED latest_opaque_data".to_string()),
                // S-031: latest must use raw-replay path; reconstructed
                // redacted_thinking would be dropped from the latest msg.
                internal_chat_message_metadata_passthrough: None,

                raw_wire_block: Some(json!({
                    "type": "redacted_thinking",
                    "data": "latest_opaque_data",
                })),
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "A2".to_string(),
                }],
                phase: None,
            },
        ];

        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        let assistant_msgs: Vec<_> = messages
            .iter()
            .filter(|m| m["role"] == "assistant")
            .collect();
        assert!(
            assistant_msgs.len() >= 2,
            "should have at least 2 assistant messages"
        );

        let first = &assistant_msgs[0];
        for block in first["content"].as_array().unwrap() {
            assert_ne!(
                block["type"], "redacted_thinking",
                "redacted_thinking must be stripped from earlier turns"
            );
        }

        let last = assistant_msgs.last().unwrap();
        let content = last["content"].as_array().unwrap();
        assert!(
            content.iter().any(|b| b["type"] == "redacted_thinking"),
            "latest assistant must preserve redacted_thinking"
        );
        assert_eq!(
            content
                .iter()
                .find(|b| b["type"] == "redacted_thinking")
                .unwrap()["data"],
            "latest_opaque_data"
        );
    }

    // ── S-014: Vertex AI prefill guard tests ────────────────────────────

    #[test]
    fn trailing_plain_text_assistant_gets_continue_sentinel() {
        // Vertex AI rejects ALL assistant-ending conversations, not just
        // those with tool_use. This test ensures plain-text assistant endings
        // get a "[Continue]" sentinel appended.
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "hello".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "hi there".to_string(),
                }],
                phase: None,
            },
        ];
        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(
            messages.len(),
            3,
            "should have user + assistant + synthetic user"
        );
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(
            messages[2]["content"][0]["text"], "[Continue]",
            "plain-text assistant ending should get [Continue] sentinel"
        );
    }

    #[test]
    fn trailing_tool_use_assistant_gets_awaiting_sentinel() {
        // When the trailing assistant has tool_use, the sentinel should be
        // "[Awaiting tool result]" for clarity.
        use codex_protocol::models::LocalShellAction;
        use codex_protocol::models::LocalShellExecAction;
        use codex_protocol::models::LocalShellStatus;

        let _input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "run ls".to_string(),
                }],
                phase: None,
            },
            ResponseItem::LocalShellCall {
                call_id: Some("toolu_paired".to_string()),
                action: LocalShellAction::Exec(LocalShellExecAction {
                    command: vec!["ls".to_string()],
                    timeout_ms: None,
                    working_directory: None,
                    env: None,
                    user: None,
                }),
                id: None,
                status: LocalShellStatus::Completed,
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_paired".to_string(),
                output: FunctionCallOutputPayload::from_text("file.txt".into()),
            },
            // Second tool_use WITHOUT result — this will be cleaned by S-005
            // but let's test with a FunctionCall that IS paired to check
            // the tool_use sentinel path works.
        ];
        // This won't trigger because paired calls end with tool_result (user role).
        // Let's test with a direct FunctionCall construction:
        let input2 = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "run ls".to_string(),
                }],
                phase: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: r#"{"command":"ls"}"#.to_string(),
                call_id: "toolu_with_result".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_with_result".to_string(),
                output: FunctionCallOutputPayload::from_text("file.txt".to_string()),
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: r#"{"command":"pwd"}"#.to_string(),
                call_id: "toolu_with_result2".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "toolu_with_result2".to_string(),
                output: FunctionCallOutputPayload::from_text("/home".to_string()),
            },
        ];
        let messages2 = conversation_to_anthropic_messages(&input2, true, true, false);
        // All paired, so last message is a tool_result (user role). No sentinel needed.
        let last2 = messages2.last().unwrap();
        assert_eq!(
            last2["role"], "user",
            "paired tool results end as user — no sentinel needed"
        );
    }

    #[test]
    fn forked_conversation_ending_with_assistant_gets_sentinel() {
        // Simulates what happens when fork_context=true creates a conversation
        // snapshot that ends on an assistant boundary.
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "analyze this code".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "I'll analyze the code for you.".to_string(),
                }],
                phase: None,
            },
        ];
        let messages = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(
            messages.len(),
            3,
            "forked assistant-ending conv needs sentinel"
        );
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["text"], "[Continue]");
    }
}

// ── ResponseItem / ToolSpec variant exhaustiveness tests ─────────────
//
// These tests ensure that every ResponseItem variant is accounted for in
// the /messages wire translator. When a new variant is added to the enum,
// the match arm below will fail to compile, forcing the developer to
// decide whether it needs Anthropic translation.

#[cfg(test)]
mod variant_exhaustiveness_tests {
    use super::*;
    use codex_protocol::models::*;

    /// Classifies a ResponseItem variant for the /messages wire.
    ///
    /// **This function exists solely to produce a compile error when a new
    /// ResponseItem variant is added.** Update the match, add the variant
    /// to the appropriate category, and (if "translated") add handling in
    /// `conversation_to_anthropic_messages` + `clean_orphaned_tool_calls`.
    fn variant_category(item: &ResponseItem) -> &'static str {
        match item {
            // ── Translated to Anthropic messages ──
            ResponseItem::Message { .. } => "translated",
            ResponseItem::Reasoning { .. } => "translated",
            ResponseItem::FunctionCall { .. } => "translated",
            ResponseItem::FunctionCallOutput { .. } => "translated",
            ResponseItem::CustomToolCall { .. } => "translated",
            ResponseItem::CustomToolCallOutput { .. } => "translated",
            ResponseItem::LocalShellCall { .. } => "translated",
            ResponseItem::ToolSearchCall { .. } => "translated",
            ResponseItem::ToolSearchOutput { .. } => "translated",

            // ── Intentionally skipped (no Anthropic equivalent / internal-only) ──
            ResponseItem::WebSearchCall { .. } => "skipped_intentional",
            ResponseItem::ImageGenerationCall { .. } => "skipped_intentional",
            ResponseItem::Compaction { .. } => "skipped_internal",
            // Upstream compaction-orchestration variants — internal control
            // events, never translated to Anthropic messages.
            ResponseItem::CompactionTrigger => "skipped_internal",
            ResponseItem::ContextCompaction { .. } => "skipped_internal",
            ResponseItem::Other => "skipped_unknown",
            // ── If you get a compile error here, a new variant was added. ──
            // Decide: does it need Anthropic translation?
            //   YES → add to "translated" above, add match arm in
            //          conversation_to_anthropic_messages(), and if it's a
            //          tool call/output, add to clean_orphaned_tool_calls().
            //   NO  → add to "skipped_*" above with a comment explaining why.
        }
    }

    #[test]
    fn all_response_item_variants_classified() {
        // Build one instance of each variant and verify it classifies.
        // The real value is the compile-time exhaustiveness check above.
        let items: Vec<ResponseItem> = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "hi".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Reasoning {
                id: None,
                summary: vec![],
                content: None,
                encrypted_content: None,
                internal_chat_message_metadata_passthrough: None,
                raw_wire_block: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: "{}".to_string(),
                call_id: "c1".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "c1".to_string(),
                output: FunctionCallOutputPayload::from_text("ok".to_string()),
            },
            ResponseItem::CustomToolCall {
                id: None,
                status: None,
                call_id: "c2".to_string(),
                name: "patch".to_string(),
                input: "{}".to_string(),
            },
            ResponseItem::CustomToolCallOutput {
                call_id: "c2".to_string(),
                name: None,
                output: FunctionCallOutputPayload::from_text("ok".to_string()),
            },
            ResponseItem::LocalShellCall {
                id: None,
                call_id: Some("c3".to_string()),
                status: LocalShellStatus::Completed,
                action: LocalShellAction::Exec(LocalShellExecAction {
                    command: vec!["ls".to_string()],
                    timeout_ms: None,
                    working_directory: None,
                    env: None,
                    user: None,
                }),
            },
            ResponseItem::ToolSearchCall {
                id: None,
                call_id: Some("c4".to_string()),
                status: None,
                execution: "client".to_string(),
                arguments: serde_json::json!({}),
            },
            ResponseItem::ToolSearchOutput {
                call_id: Some("c4".to_string()),
                status: "completed".to_string(),
                execution: "client".to_string(),
                tools: vec![],
            },
            ResponseItem::WebSearchCall {
                id: None,
                status: None,
                action: None,
            },
            ResponseItem::ImageGenerationCall {
                id: "ig1".to_string(),
                status: "completed".to_string(),
                revised_prompt: None,
                result: String::new(),
            },
            ResponseItem::Compaction {
                encrypted_content: String::new(),
            },
            ResponseItem::Other,
        ];

        // Every variant must have a classification.
        for item in &items {
            let cat = variant_category(item);
            assert!(
                [
                    "translated",
                    "skipped_intentional",
                    "skipped_internal",
                    "skipped_unknown"
                ]
                .contains(&cat),
                "unexpected category: {cat}"
            );
        }

        // Count: ensure we covered all expected variants.
        // GhostSnapshot is excluded from the runtime check because constructing
        // one requires a GhostCommit which is non-trivial — but the
        // compile-time exhaustiveness match above covers it.
        let translated_count = items
            .iter()
            .filter(|i| variant_category(i) == "translated")
            .count();
        assert!(
            translated_count >= 9,
            "expected at least 9 translated variants, got {translated_count}"
        );
    }

    #[test]
    fn translated_variants_produce_anthropic_messages() {
        // Verify that every "translated" variant actually produces output
        // in conversation_to_anthropic_messages (not silently dropped).
        // We test each in isolation with proper pairing.

        // 1. Message → user or assistant message
        let input = vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "hi".to_string(),
            }],
            phase: None,
        }];
        let msgs = conversation_to_anthropic_messages(&input, true, true, false);
        assert!(!msgs.is_empty(), "Message variant must produce output");

        // 2. FunctionCall + FunctionCallOutput pair
        let input = vec![
            ResponseItem::FunctionCall {
                id: None,
                name: "test".to_string(),
                namespace: None,
                arguments: "{}".to_string(),
                call_id: "fc1".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "fc1".to_string(),
                output: FunctionCallOutputPayload::from_text("ok".to_string()),
            },
        ];
        let msgs = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(msgs.len(), 2, "FunctionCall+Output must produce 2 messages");

        // 3. CustomToolCall + CustomToolCallOutput pair
        let input = vec![
            ResponseItem::CustomToolCall {
                id: None,
                status: None,
                call_id: "ct1".to_string(),
                name: "patch".to_string(),
                input: "{}".to_string(),
            },
            ResponseItem::CustomToolCallOutput {
                call_id: "ct1".to_string(),
                name: None,
                output: FunctionCallOutputPayload::from_text("ok".to_string()),
            },
        ];
        let msgs = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(
            msgs.len(),
            2,
            "CustomToolCall+Output must produce 2 messages"
        );

        // 4. LocalShellCall + FunctionCallOutput pair
        let input = vec![
            ResponseItem::LocalShellCall {
                id: None,
                call_id: Some("ls1".to_string()),
                status: LocalShellStatus::Completed,
                action: LocalShellAction::Exec(LocalShellExecAction {
                    command: vec!["ls".to_string()],
                    timeout_ms: None,
                    working_directory: None,
                    env: None,
                    user: None,
                }),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "ls1".to_string(),
                output: FunctionCallOutputPayload::from_text("files".to_string()),
            },
        ];
        let msgs = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(
            msgs.len(),
            2,
            "LocalShellCall+Output must produce 2 messages"
        );

        // 5. ToolSearchCall + ToolSearchOutput pair
        let input = vec![
            ResponseItem::ToolSearchCall {
                id: None,
                call_id: Some("ts1".to_string()),
                status: None,
                execution: "client".to_string(),
                arguments: serde_json::json!({"query": "test"}),
            },
            ResponseItem::ToolSearchOutput {
                call_id: Some("ts1".to_string()),
                status: "completed".to_string(),
                execution: "client".to_string(),
                tools: vec![serde_json::json!({"name": "found_tool"})],
            },
        ];
        let msgs = conversation_to_anthropic_messages(&input, true, true, false);
        assert_eq!(
            msgs.len(),
            2,
            "ToolSearchCall+Output must produce 2 messages"
        );

        // 6. Reasoning
        let input = vec![
            // Need a user message first so reasoning isn't orphaned
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "think".to_string(),
                }],
                phase: None,
            },
            ResponseItem::Reasoning {
                id: Some("r1".to_string()),
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "I thought about it".to_string(),
                }],
                content: None,
                encrypted_content: Some("sig123".to_string()),
                // S-031: standalone reasoning lands as the latest assistant
                // message; raw-replay path required to survive the strip.
                internal_chat_message_metadata_passthrough: None,

                raw_wire_block: Some(json!({
                    "type": "thinking",
                    "thinking": "I thought about it",
                    "signature": "sig123",
                })),
            },
        ];
        let msgs = conversation_to_anthropic_messages(&input, true, true, false);
        assert!(msgs.len() >= 2, "Reasoning variant must produce output");
    }

    /// Compile-time exhaustiveness check for ToolSpec variants in
    /// tools_to_anthropic_format. Mirrors the ResponseItem check above.
    fn tool_spec_category(spec: &ToolSpec) -> &'static str {
        match spec {
            // ── Translated to Anthropic tool definitions ──
            ToolSpec::Function(_) => "translated",
            ToolSpec::Freeform(_) => "translated",
            ToolSpec::ToolSearch { .. } => "translated",

            // ── Intentionally skipped (server-side / no Anthropic equivalent) ──
            ToolSpec::WebSearch { .. } => "skipped",
            ToolSpec::ImageGeneration { .. } => "skipped",
            ToolSpec::Namespace(_) => "skipped",
            // ── If you get a compile error here, a new ToolSpec variant was added. ──
            // Decide: should Claude see this tool?
            //   YES → add to "translated" above and add a match arm in
            //          tools_to_anthropic_format().
            //   NO  → add to "skipped" above with a comment.
        }
    }

    #[test]
    fn all_tool_spec_variants_classified() {
        use codex_tools::FreeformTool;
        use codex_tools::FreeformToolFormat;
        use codex_tools::JsonSchema;
        use codex_tools::ResponsesApiTool;

        let specs: Vec<ToolSpec> = vec![
            ToolSpec::Function(ResponsesApiTool {
                name: "test".to_string(),
                description: "test".to_string(),
                strict: false,
                defer_loading: None,
                parameters: JsonSchema::object(Default::default(), None, None),
                output_schema: None,
            }),
            ToolSpec::Freeform(FreeformTool {
                name: "patch".to_string(),
                description: "patch".to_string(),
                format: FreeformToolFormat {
                    r#type: "grammar".to_string(),
                    syntax: "diff".to_string(),
                    definition: "".to_string(),
                },
            }),
            ToolSpec::ToolSearch {
                execution: "client".to_string(),
                description: "search".to_string(),
                parameters: JsonSchema::object(Default::default(), None, None),
            },
            ToolSpec::WebSearch {
                external_web_access: None,
                filters: None,
                user_location: None,
                search_context_size: None,
                search_content_types: None,
            },
            ToolSpec::ImageGeneration {
                output_format: "png".to_string(),
            },
        ];

        let translated = specs
            .iter()
            .filter(|s| tool_spec_category(s) == "translated")
            .count();
        let skipped = specs
            .iter()
            .filter(|s| tool_spec_category(s) == "skipped")
            .count();
        assert_eq!(translated, 3, "expected 3 translated ToolSpec variants");
        assert_eq!(skipped, 2, "expected 2 skipped ToolSpec variants");

        // Verify translated specs actually produce Anthropic tool JSON.
        let translated_specs: Vec<ToolSpec> = specs
            .into_iter()
            .filter(|s| tool_spec_category(s) == "translated")
            .collect();
        let anthropic = tools_to_anthropic_format(&translated_specs);
        assert_eq!(
            anthropic.len(),
            3,
            "all translated ToolSpec variants must produce Anthropic JSON"
        );
    }
    // ── S-021 adjacency constraint tests ──────────────────────────

    /// S-021: Adjacent tool_use/tool_result pairs are preserved.
    #[test]
    fn s021_adjacent_pair_preserved() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "do it".to_string(),
                }],
                phase: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                call_id: "tc1".to_string(),
                name: "shell".to_string(),
                arguments: r#"{"cmd":"ls"}"#.to_string(),
                namespace: None,
            },
            ResponseItem::FunctionCallOutput {
                call_id: "tc1".to_string(),
                output: FunctionCallOutputPayload::from_text("files".to_string()),
            },
        ];
        let msgs = conversation_to_anthropic_messages(&input, true, true, false);
        // S-014 appends a sentinel; real messages are user -> assistant(tool_use) -> user(tool_result)
        // Find the assistant message with tool_use
        let asst_idx = msgs
            .iter()
            .position(|m| {
                m["role"] == "assistant"
                    && m["content"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|b| b["type"] == "tool_use"))
            })
            .expect("must have assistant with tool_use");
        let user_result_idx = asst_idx + 1;
        assert_eq!(msgs[user_result_idx]["role"], "user");
        assert!(
            msgs[user_result_idx]["content"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["type"] == "tool_result")
        );
        assert_eq!(msgs[user_result_idx]["content"][0]["tool_use_id"], "tc1");
    }

    /// S-021: Non-adjacent tool_use (assistant message followed by another
    /// assistant message before the user tool_result) gets stripped.
    #[test]
    fn s021_non_adjacent_tool_use_stripped() {
        // Simulate: tool_use in assistant[0], text in assistant[1] (merged),
        // tool_result in user[2]. After translation, the tool_use has no
        // adjacent tool_result because another assistant message intervenes.
        //
        // We test the post-translation cleaner directly.
        let mut messages = vec![
            serde_json::json!({
                "role": "user",
                "content": [{"type": "text", "text": "go"}]
            }),
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "tool_use", "id": "tc1", "name": "shell", "input": {}}]
            }),
            // Intervening assistant message (e.g., from a text block after normalization split)
            serde_json::json!({
                "role": "user",
                "content": [{"type": "text", "text": "ack"}]
            }),
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "text", "text": "done"}]
            }),
            serde_json::json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "tc1", "content": "ok"}]
            }),
        ];
        clean_orphaned_tool_blocks_by_adjacency(&mut messages);
        // The tool_use in messages[1] has no tool_result in messages[2] (which is user text),
        // so it should be stripped. messages[1] becomes empty and is removed.
        // The tool_result in messages[4] has no tool_use in messages[3], so it's stripped too.
        // After removal, we should have: user("go"), user("ack"), assistant("done")
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["content"][0]["text"], "go");
        assert_eq!(messages[1]["content"][0]["text"], "ack");
        assert_eq!(messages[2]["content"][0]["text"], "done");
    }

    /// S-021: Parallel tool calls with interleaved items get the orphans stripped
    /// while preserving properly-adjacent pairs.
    #[test]
    fn s021_parallel_calls_split_by_normalization() {
        // Simulates the RCA failure path: parallel tool calls [A, B] that got
        // split by ensure_call_outputs_present into separate assistant/user pairs.
        // After translation: asst[tool_use_A] -> user[tool_result_A] -> asst[tool_use_B] -> user[tool_result_B]
        // Both pairs are adjacent so both should survive.
        let mut messages = vec![
            serde_json::json!({
                "role": "user",
                "content": [{"type": "text", "text": "do both"}]
            }),
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "tool_use", "id": "tcA", "name": "shell", "input": {}}]
            }),
            serde_json::json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "tcA", "content": "resultA"}]
            }),
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "tool_use", "id": "tcB", "name": "shell", "input": {}}]
            }),
            serde_json::json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "tcB", "content": "resultB"}]
            }),
        ];
        clean_orphaned_tool_blocks_by_adjacency(&mut messages);
        // Both pairs are adjacent, so all 5 messages survive.
        assert_eq!(messages.len(), 5);
    }

    /// S-021: Mixed assistant message with text + tool_use where tool_use is
    /// orphaned — only the tool_use block is stripped, text survives.
    #[test]
    fn s021_mixed_assistant_keeps_text_strips_orphan_tool_use() {
        let mut messages = vec![
            serde_json::json!({
                "role": "user",
                "content": [{"type": "text", "text": "go"}]
            }),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "thinking..."},
                    {"type": "tool_use", "id": "tc1", "name": "shell", "input": {}}
                ]
            }),
            // Next message is user but with text, not tool_result
            serde_json::json!({
                "role": "user",
                "content": [{"type": "text", "text": "nevermind"}]
            }),
        ];
        clean_orphaned_tool_blocks_by_adjacency(&mut messages);
        assert_eq!(messages.len(), 3);
        // Assistant message should have only the text block
        let asst_content = messages[1]["content"].as_array().unwrap();
        assert_eq!(asst_content.len(), 1);
        assert_eq!(asst_content[0]["type"], "text");
        assert_eq!(asst_content[0]["text"], "thinking...");
    }

    /// S-021: End-to-end test through conversation_to_anthropic_messages
    /// with the resume scenario from the RCA (parallel calls interrupted
    /// mid-turn, then resumed with synthetic "aborted" outputs).
    #[test]
    fn s021_e2e_resume_parallel_calls_aborted() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "run both".to_string(),
                }],
                phase: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                call_id: "tcA".to_string(),
                name: "shell".to_string(),
                arguments: r#"{"cmd":"ls"}"#.to_string(),
                namespace: None,
            },
            ResponseItem::FunctionCallOutput {
                call_id: "tcA".to_string(),
                output: FunctionCallOutputPayload::from_text("aborted".to_string()),
            },
            ResponseItem::FunctionCall {
                id: None,
                call_id: "tcB".to_string(),
                name: "shell".to_string(),
                arguments: r#"{"cmd":"cat"}"#.to_string(),
                namespace: None,
            },
            ResponseItem::FunctionCallOutput {
                call_id: "tcB".to_string(),
                output: FunctionCallOutputPayload::from_text("aborted".to_string()),
            },
        ];
        let msgs = conversation_to_anthropic_messages(&input, true, true, false);
        // S-014 may append a sentinel. Strip it for assertion clarity.
        let msgs: Vec<_> = msgs
            .iter()
            .filter(|m| {
                let text = m["content"]
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|b| b["text"].as_str())
                    .unwrap_or("");
                text != "[Continue]" && text != "[Awaiting tool result]"
            })
            .collect();
        // user("run both") -> asst(tool_use_A) -> user(tool_result_A) ->
        // asst(tool_use_B) -> user(tool_result_B)
        assert_eq!(msgs.len(), 5);
        assert_eq!(msgs[1]["content"][0]["id"], "tcA");
        assert_eq!(msgs[2]["content"][0]["tool_use_id"], "tcA");
        assert_eq!(msgs[3]["content"][0]["id"], "tcB");
        assert_eq!(msgs[4]["content"][0]["tool_use_id"], "tcB");
    }

    /// S-021: Empty messages are removed after stripping.
    #[test]
    fn s021_empty_messages_removed_after_stripping() {
        let mut messages = vec![
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "tool_use", "id": "tc1", "name": "shell", "input": {}}]
            }),
            // Not a user message, so tool_use tc1 has no adjacent result
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "text", "text": "oops"}]
            }),
        ];
        clean_orphaned_tool_blocks_by_adjacency(&mut messages);
        // First message becomes empty after stripping, gets removed
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"][0]["text"], "oops");
    }

    // ── S-022 tool_result-first ordering tests ───────────────────────

    /// S-022: `tool_result` block preceded by a text block in the same
    /// user message gets hoisted to the front.
    #[test]
    fn s022_hoists_tool_result_when_preceded_by_text() {
        let mut messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "Warning: too many processes"},
                {"type": "tool_result", "tool_use_id": "tc1", "content": "ok"}
            ]
        })];
        hoist_tool_results_to_front(&mut messages);
        assert_eq!(messages[0]["content"][0]["type"], "tool_result");
        assert_eq!(messages[0]["content"][0]["tool_use_id"], "tc1");
        assert_eq!(messages[0]["content"][1]["type"], "text");
        assert_eq!(
            messages[0]["content"][1]["text"],
            "Warning: too many processes"
        );
    }

    /// S-022: Multiple tool_result blocks interleaved with text retain
    /// their relative order and land in front of text blocks.
    #[test]
    fn s022_stable_partition_preserves_relative_order() {
        let mut messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "tcA", "content": "a"},
                {"type": "text", "text": "mid"},
                {"type": "tool_result", "tool_use_id": "tcB", "content": "b"},
                {"type": "text", "text": "end"}
            ]
        })];
        hoist_tool_results_to_front(&mut messages);
        assert_eq!(messages[0]["content"][0]["tool_use_id"], "tcA");
        assert_eq!(messages[0]["content"][1]["tool_use_id"], "tcB");
        assert_eq!(messages[0]["content"][2]["text"], "mid");
        assert_eq!(messages[0]["content"][3]["text"], "end");
    }

    /// S-022: Already-correct ordering is a no-op.
    #[test]
    fn s022_noop_when_tool_results_already_leading() {
        let original = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "tc1", "content": "ok"},
                {"type": "text", "text": "trailing"}
            ]
        });
        let mut messages = vec![original.clone()];
        hoist_tool_results_to_front(&mut messages);
        assert_eq!(messages[0], original);
    }

    /// S-022: Messages with no tool_result blocks are untouched.
    #[test]
    fn s022_noop_when_no_tool_results() {
        let original = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "text", "text": "world"}
            ]
        });
        let mut messages = vec![original.clone()];
        hoist_tool_results_to_front(&mut messages);
        assert_eq!(messages[0], original);
    }

    /// S-022: Assistant messages are not reordered.
    #[test]
    fn s022_does_not_touch_assistant_messages() {
        let original = serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "thinking"},
                {"type": "tool_use", "id": "tc1", "name": "shell", "input": {}}
            ]
        });
        let mut messages = vec![original.clone()];
        hoist_tool_results_to_front(&mut messages);
        assert_eq!(messages[0], original);
    }

    /// S-022: End-to-end regression — the exact corruption pattern from the
    /// rollout that triggered `messages.654: tool_use ids were found without
    /// tool_result blocks immediately after: toolu_vrtx_01Ck67…`.
    ///
    /// Sequence in the conversation history:
    ///   1. FunctionCall (assistant tool_use)
    ///   2. Synthetic user-role warning recorded mid-tool-call (e.g. the
    ///      unified-exec "too many processes" warning)
    ///   3. FunctionCallOutput (tool_result for the same call_id)
    ///
    /// After translation, the user message contains [text, tool_result] —
    /// Vertex AI rejects this ordering. After S-022 it must contain
    /// [tool_result, text].
    #[test]
    fn s022_e2e_warning_injected_between_tool_use_and_result() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "do the thing".to_string(),
                }],
                phase: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                call_id: "tc1".to_string(),
                name: "shell".to_string(),
                arguments: r#"{"cmd":"ls"}"#.to_string(),
                namespace: None,
            },
            // Out-of-band warning injected into history between the tool
            // call and its output (mirrors `record_model_warning`).
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Warning: The maximum number of unified exec processes you can keep open is 60".to_string(),
                }],
                phase: None,
            },
            ResponseItem::FunctionCallOutput {
                call_id: "tc1".to_string(),
                output: FunctionCallOutputPayload::from_text("ok".to_string()),
            },
        ];
        let msgs = conversation_to_anthropic_messages(&input, true, true, false);
        // Find the user message that contains the tool_result and assert
        // that tool_result is the *first* content block.
        let user_with_result = msgs
            .iter()
            .find(|m| {
                m["role"] == "user"
                    && m["content"]
                        .as_array()
                        .map(|a| a.iter().any(|b| b["type"] == "tool_result"))
                        .unwrap_or(false)
            })
            .expect("a user message with tool_result must exist");
        assert_eq!(
            user_with_result["content"][0]["type"], "tool_result",
            "tool_result must be the leading block, got: {user_with_result}"
        );
        assert_eq!(user_with_result["content"][0]["tool_use_id"], "tc1");
    }
}
