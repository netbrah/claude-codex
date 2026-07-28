//! Stage C — Pi-projection equivalence goldens.
//!
//! For each fixture under `tests/fixtures/response_item_equiv/<name>/` that
//! ships a `pi_projection.json` (the `output.content[]` shape
//! `claude-agent-sdk-pi` would produce from the same SSE stream), assert that
//! XLI's `ResponseItem[]` from `spawn_messages_stream` is *structurally
//! equivalent* to pi's IR after applying the documented projection rules.
//!
//! Why: pi (Anthropic-published, 1258 LOC single-file) is the canonical
//! Anthropic-IR harness. Treating its state machine as a contract gives XLI a
//! well-defined fence — any drift in either harness's projection of the wire
//! shows up here as a failing test.
//!
//! Authority:
//! - cli-ops/refs/api-references/harness-invariants-pi-reference.md
//! - cli-ops/refs/api-references/scripts/pi-reference-xli-cross-ref.json
//! - cli-ops/refs/api-references/harness-invariants-response-item.md §8
//!   (Pi-reference cross-links Stage A → Stage B)
//!
//! The projection is bidirectional in concept; this file enforces it
//! one-directionally: pi-IR → XLI-equivalent `ResponseItem`. Each ASYMMETRIC
//! verdict from the cross-ref JSON is implemented as an explicit `match`
//! arm in `project_pi_block_to_response_item`. Adding a new pi branch type
//! must add a match arm here OR the projection panics, which is itself the
//! drift gate.

use codex_api::ApiError;
use codex_api::ResponseEvent;
use codex_api::spawn_messages_stream;
use codex_client::ByteStream;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use futures::StreamExt;
use serde_json::Value;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::io::ReaderStream;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/response_item_equiv"
);

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

fn load_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn load_sse_lines(name: &str) -> Vec<String> {
    let v = load_json(&fixture_dir(name).join("messages_sse.json"));
    v.as_array()
        .expect("messages_sse.json must be array of SSE lines")
        .iter()
        .map(|item| item.as_str().expect("SSE line must be string").to_string())
        .collect()
}

fn load_pi_projection(name: &str) -> Value {
    load_json(&fixture_dir(name).join("pi_projection.json"))
}

fn messages_lines_to_stream(lines: &[String]) -> ByteStream {
    let body = lines.join("\n");
    let reader = std::io::Cursor::new(body.into_bytes());
    Box::pin(
        ReaderStream::new(reader)
            .map(|r| r.map_err(|e| codex_client::TransportError::Network(e.to_string()))),
    )
}

async fn collect_messages_events(lines: &[String]) -> Vec<Result<ResponseEvent, ApiError>> {
    let stream = messages_lines_to_stream(lines);
    let mut rx = spawn_messages_stream(stream, Duration::from_secs(30)).rx_event;
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    events
}

fn extract_output_item_done(events: &[Result<ResponseEvent, ApiError>]) -> Vec<ResponseItem> {
    events
        .iter()
        .filter_map(|ev| match ev {
            Ok(ResponseEvent::OutputItemDone(item)) => Some(item.clone()),
            _ => None,
        })
        .collect()
}

/// Pi → XLI projection contract.
///
/// Implements the cross-ref JSON's MIRRORED + ASYMMETRIC verdicts as an
/// executable mapping from a pi `output.content[]` block to an XLI
/// `ResponseItem`.
///
/// Panics on any pi block type not handled — this is the drift gate: if
/// pi's upstream introduces a new block type, this assert fires until an
/// operator adds the projection rule (and updates
/// `pi-reference-xli-cross-ref.json`).
fn project_pi_block_to_response_item(pi_block: &Value) -> ResponseItem {
    let ty = pi_block
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("pi block missing 'type': {pi_block}"));
    match ty {
        // pi.text -> XLI Message{content:[OutputText]} (assistant text).
        // Single-text-block messages match exactly; multi-block messages
        // must aggregate at a higher level (see `aggregate_pi_message_blocks`).
        "text" => {
            let text = pi_block
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText { text }],
                phase: None,
            }
        }

        // pi.thinking -> XLI Reasoning with raw_wire_block constructed from
        // the decomposed (thinking, thinkingSignature) pair. This is the
        // ASYMMETRIC verdict made executable: pi keeps thinking+signature
        // as flat fields; XLI requires both decomposed (summary +
        // encrypted_content) and the raw block (raw_wire_block) for
        // byte-identical replay. See pi-reference-xli-cross-ref.json
        // entries `block.type:thinking:438` and `block.type:thinking:1107`.
        "thinking" => {
            let thinking = pi_block
                .get("thinking")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let signature = pi_block
                .get("thinkingSignature")
                .and_then(|v| v.as_str())
                .map(str::to_string);

            let raw_wire_block = match signature.as_deref() {
                Some(sig) if !sig.is_empty() => Some(json!({
                    "type": "thinking",
                    "thinking": &thinking,
                    "signature": sig,
                })),
                _ => Some(json!({
                    "type": "thinking",
                    "thinking": &thinking,
                })),
            };

            ResponseItem::Reasoning {
                id: None,
                summary: vec![ReasoningItemReasoningSummary::SummaryText { text: thinking }],
                content: None,
                encrypted_content: signature.filter(|s| !s.is_empty()),
                raw_wire_block,
            internal_chat_message_metadata_passthrough: None,

            }
        }

        // pi.toolCall -> XLI FunctionCall. ASYMMETRIC: pi keeps arguments
        // as parsed `Record<string, unknown>` PLUS a `partialJson` side
        // band for incremental parsing. XLI keeps arguments as String and
        // parses lazily downstream (Session::handle_function_call) — the
        // partialJson is reconstructed via ToolCallInputDelta events (see
        // RI-DEFER-003 + pi-reference cross-ref `delta.type:input_json_delta:1072`).
        "toolCall" => {
            let id = pi_block
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = pi_block
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let arguments_value = pi_block
                .get("arguments")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));
            let arguments = serde_json::to_string(&arguments_value)
                .expect("serialize pi toolCall arguments");
            // pi's flat MCP name is split by XLI's parser into (namespace, bare_name)
            // per parse_flat_mcp_tool_name. For the projection here we replicate
            // that split — pi delivers the flat form; XLI's IR carries the
            // structured (namespace?, name) pair.
            let (namespace, bare_name) = parse_flat_mcp_tool_name(&name);
            ResponseItem::FunctionCall {
                id: None,
                name: bare_name,
                namespace,
                arguments,
                call_id: id,
            }
        }

        other => {
            panic!(
                "pi block type not handled by projection: '{other}'. \
                 Add a match arm in project_pi_block_to_response_item AND \
                 update refs/api-references/scripts/pi-reference-xli-cross-ref.json. \
                 Block: {pi_block}"
            );
        }
    }
}

/// Mirrors XLI's parse_flat_mcp_tool_name (sse/messages.rs:34-67). Kept
/// inline so this test file is self-contained.
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

/// Project pi's `output_content` array to a `Vec<ResponseItem>`.
///
/// Adjacent text blocks within a single assistant message are NOT aggregated
/// here — pi's IR keeps them as separate blocks. XLI's `ResponseItem::Message`
/// can hold multiple `ContentItem` entries, but for the equivalence test we
/// rely on the SSE stream itself: a single content_block_start/stop pair
/// emits one IR item, which matches pi's per-block emission.
fn project_pi_blocks(pi_blocks: &[Value]) -> Vec<ResponseItem> {
    pi_blocks
        .iter()
        .map(project_pi_block_to_response_item)
        .collect()
}

/// Strip XLI-internal fields that the equivalence comparison should ignore.
/// Mirrors `normalize_for_equiv` in response_item_equiv.rs, with one extra
/// canonicalization specific to Stage C: FunctionCall.arguments is parsed
/// to a JSON Value when possible so semantic equality holds across the
/// pi/XLI shape asymmetry.
///
/// Why: pi keeps `arguments` as a parsed `Record<string, unknown>` and the
/// projection helper re-serializes via `serde_json::to_string` (compact
/// form, no inter-key whitespace). XLI preserves the raw partial_json
/// bytes streamed by the model verbatim (`{"cmd": "ls"}` with whatever
/// whitespace the model emitted). The cross-ref JSON marks
/// `block.type:toolCall:439` ASYMMETRIC for exactly this reason. The
/// contract Stage C enforces is *semantic* JSON equivalence of arguments,
/// not byte-identity. raw_wire_block on Reasoning is left intact because
/// byte-identity IS its contract (S-OPUS47 replay anchor).
fn normalize_for_equiv(items: &[ResponseItem]) -> Value {
    let mut normalized = Vec::new();
    for item in items {
        let mut v = serde_json::to_value(item).expect("serialize item");
        if let Some(obj) = v.as_object_mut() {
            obj.remove("id");
            if obj.get("type") == Some(&Value::String("reasoning".into())) {
                obj.insert("id".into(), Value::String(String::new()));
                if obj.get("content").map(|c| c.is_null()).unwrap_or(false) {
                    obj.remove("content");
                }
            }
            if obj.get("type") == Some(&Value::String("function_call".into()))
                && let Some(args) = obj.get_mut("arguments")
                && let Some(s) = args.as_str()
                && let Ok(parsed) = serde_json::from_str::<Value>(s)
            {
                *args = parsed;
            }
        }
        normalized.push(v);
    }
    Value::Array(normalized)
}

async fn assert_pi_projection_equivalent(name: &str) {
    let lines = load_sse_lines(name);
    let events = collect_messages_events(&lines).await;
    let xli_items = extract_output_item_done(&events);

    let pi_doc = load_pi_projection(name);
    let pi_blocks = pi_doc
        .get("output_content")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("pi_projection.json[{name}] missing 'output_content' array"))
        .clone();
    let projected = project_pi_blocks(&pi_blocks);

    let xli_norm = normalize_for_equiv(&xli_items);
    let projected_norm = normalize_for_equiv(&projected);

    assert_eq!(
        xli_norm, projected_norm,
        "fixture '{name}': pi-IR projection does not match XLI ResponseItem[] sequence. \
         If pi/index.ts changed shape, update scripts/pi-reference-xli-cross-ref.json AND \
         the corresponding match arm in project_pi_block_to_response_item."
    );
}

/// Asymmetric fence: XLI is a strict superset of pi's vocabulary for the
/// given SSE. The fixture declares pi's `output_content[]` (possibly empty)
/// AND an `_xli_only_items[]` array of already-typed ResponseItem JSON for
/// the blocks pi cannot represent (e.g. `web_search_call`,
/// `image_generation_call`). The assertion: XLI's OutputItemDone sequence
/// must equal `project(output_content) ++ _xli_only_items` in order.
///
/// This is the canonical Stage C fence for RI-022 (server_tool_use →
/// WebSearchCall) and the home for any future XLI-extension blocks beyond
/// pi's content_block vocabulary. The fixture's `_doc` field MUST cite the
/// pi line ranges where the missing arm would live, and
/// `_pi_state_machine_branches_MISSING[]` must enumerate the pi branches
/// that have no equivalent — making the asymmetry self-documenting in the
/// fixture itself.
async fn assert_pi_projection_with_xli_extension(name: &str) {
    let lines = load_sse_lines(name);
    let events = collect_messages_events(&lines).await;
    let xli_items = extract_output_item_done(&events);

    let pi_doc = load_pi_projection(name);
    let pi_blocks = pi_doc
        .get("output_content")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("pi_projection.json[{name}] missing 'output_content' array"))
        .clone();

    // Build the expected XLI sequence. Two modes:
    //
    // 1) DEFAULT (append mode): expected = project(output_content) ++ _xli_only_items.
    //    Used when XLI-extension items arrive AFTER all pi-known blocks
    //    in SSE order, so a simple append captures the order.
    //
    // 2) STRICT ORDER (`_xli_full_sequence`): the fixture supplies the
    //    complete expected ResponseItem[] verbatim. Used when XLI-only
    //    items are INTERLEAVED with pi-projected items (e.g.,
    //    `redacted_thinking` before `text` in the SSE → XLI emits
    //    [Reasoning, Message]; the simple append would produce
    //    [Message, Reasoning] which is wrong). `output_content` is still
    //    required as documentation of what pi would see.
    let projected = if let Some(full_arr) = pi_doc
        .get("_xli_full_sequence")
        .and_then(|v| v.as_array())
    {
        let mut full = Vec::new();
        for item_value in full_arr {
            let item: ResponseItem = serde_json::from_value(item_value.clone())
                .unwrap_or_else(|e| panic!("_xli_full_sequence[?] deserialize: {e}"));
            full.push(item);
        }
        full
    } else {
        let mut projected = project_pi_blocks(&pi_blocks);
        let xli_only_raw = pi_doc
            .get("_xli_only_items")
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| {
                panic!(
                    "pi_projection.json[{name}] uses assert_pi_projection_with_xli_extension but \
                     lacks both '_xli_only_items' and '_xli_full_sequence' arrays; use \
                     assert_pi_projection_equivalent instead, or add the missing XLI-extension items."
                )
            });
        for item_value in xli_only_raw {
            let item: ResponseItem = serde_json::from_value(item_value.clone())
                .unwrap_or_else(|e| panic!("_xli_only_items[?] deserialize: {e}"));
            projected.push(item);
        }
        projected
    };

    let xli_norm = normalize_for_equiv(&xli_items);
    let projected_norm = normalize_for_equiv(&projected);

    assert_eq!(
        xli_norm, projected_norm,
        "fixture '{name}': XLI ResponseItem[] sequence does not match expected. \
         If pi gained a new content_block arm, move the item from _xli_only_items/\
         _xli_full_sequence into output_content + update \
         refs/api-references/scripts/pi-reference-xli-cross-ref.json. Otherwise \
         XLI's extension-projection drifted."
    );
}

#[tokio::test]
async fn pi_projection_thinking_block_roundtrip() {
    // Exercises pi branches: message_start, content_block_start, thinking,
    // content_block_delta, thinking_delta, signature_delta, content_block_stop
    // (thinking + text), text, text_delta, message_delta, message_stop.
    // XLI target: Reasoning{raw_wire_block} + Message{[OutputText]}.
    assert_pi_projection_equivalent("thinking_block_roundtrip").await;
}

#[tokio::test]
async fn pi_projection_thinking_tool_chain() {
    // Closes RI-020 (raw_wire_block preservation across a thinking->tool
    // chain) and RI-026 (Gemini thoughtSignature on tool chains; the
    // cross-wire invariant is that XLI's Reasoning carries raw_wire_block
    // for both the messages and gemini legs).
    //
    // Exercises pi branches: message_start, content_block_start{thinking},
    // thinking_delta, signature_delta, content_block_stop{thinking},
    // content_block_start{tool_use}, input_json_delta,
    // content_block_stop{tool_use}, message_delta{stop_reason=tool_use},
    // message_stop. XLI target: Reasoning{raw_wire_block} followed by
    // FunctionCall in ordered OutputItemDone emission.
    assert_pi_projection_equivalent("thinking_tool_chain").await;
}

#[tokio::test]
async fn pi_projection_tool_args_stream() {
    // Closes RI-016 (final FunctionCall.arguments IR after streamed
    // input_json_delta fragments) and the well-formed half of RI-017
    // (when the stream completes cleanly, both harnesses converge on the
    // parsed arguments record). RI-DEFER-003's wire-event side band
    // (ToolCallInputDelta emission count) is locked separately by
    // response_item_equiv.rs::golden_tool_args_stream_messages_emits_tool_call_input_delta;
    // here Stage C asserts the post-stream IR invariant: pi accumulates
    // partialJson into a parsed Record, XLI keeps the raw bytes, and
    // normalize_for_equiv parses both to a serde_json::Value so semantic
    // equality holds (cross-ref `block.type:toolCall:439` ASYMMETRIC).
    assert_pi_projection_equivalent("tool_args_stream").await;
}

#[tokio::test]
async fn pi_projection_mcp_namespace_roundtrip() {
    // Closes RI-004 (FunctionCall lifecycle), RI-007 (MCP namespace
    // split — ASYMMETRIC verdict per pi-reference-xli-cross-ref.json
    // `content_block.type:tool_use:1031`), and RI-025 (cross-wire
    // FunctionCall.name consistency: same model wire shape projected by
    // both /messages and /responses to namespace+name).
    //
    // The shape contract: pi keeps the flat wire name
    // (`mcp__clangd_rs__analyze_symbol`) verbatim on its toolCall block
    // because its consumer (Claude Code) routes by the full string. XLI
    // must split because its tool registry holds structured ToolName
    // values; project_pi_block_to_response_item replicates the split via
    // parse_flat_mcp_tool_name. The fixture asserts the post-split IR
    // matches XLI's emission byte-for-byte (namespace + bare_name).
    assert_pi_projection_equivalent("mcp_namespace_roundtrip").await;
}

#[tokio::test]
async fn pi_projection_usage_merge_content() {
    // RI-032 content-side projection. Pi's token-usage merge invariant
    // lives at `output.usage` (NOT in output_content[]) and is locked by
    // response_item_equiv.rs::golden_usage_merge_messages_completed_token_usage.
    // This fixture fences the other half: the SSE that drives the usage
    // merge ALSO emits exactly one text block, and both harnesses must
    // project it to a single Message{[OutputText('done')]}. Catches
    // regressions where a usage-merge change accidentally swallows or
    // duplicates content blocks (e.g. emitting the text twice if
    // message_delta is consumed as content rather than usage-only).
    //
    // Also fences RI-034 (INVARIANT — Stream terminal message_stop).
    // Every SSE fixture in this suite includes a trailing message_stop;
    // the assert_pi_projection_equivalent helper drives the parser to
    // completion, so the Completed envelope synthesis path is exercised
    // on every run. usage_merge is the natural anchor because
    // golden_usage_merge_messages_completed_token_usage (a sibling
    // test in response_item_equiv.rs) explicitly inspects the
    // Completed event; the citation here makes the §6 rollup aware
    // that the invariant has live coverage.
    assert_pi_projection_equivalent("usage_merge").await;
}

#[tokio::test]
async fn pi_projection_web_search_server_tool_xli_extension() {
    // RI-022 ASYMMETRIC fence. Pi's content_block.type switch in
    // claude-agent-sdk-pi/index.ts has arms ONLY for text, thinking,
    // and tool_use — it has no projection for server_tool_use or
    // web_search_tool_result. XLI extends pi's vocabulary by projecting
    // server_tool_use(web_search) + the paired web_search_tool_result
    // into a single ResponseItem::WebSearchCall (sse/messages.rs:381,
    // sse/messages.rs:521). This fixture declares the asymmetry
    // explicitly: pi's output_content[] is empty, _xli_only_items[]
    // contains the WebSearchCall, and the assertion verifies XLI's
    // emission matches that union. If pi ever gains a server_tool_use
    // arm upstream, move the item from _xli_only_items into
    // output_content + update pi-reference-xli-cross-ref.json.
    assert_pi_projection_with_xli_extension("web_search_server_tool").await;
}

#[tokio::test]
async fn pi_projection_redacted_thinking_block_xli_extension() {
    // RI-010 ASYMMETRIC fence (BLOCKS_PROD / HI-C1-008). Pi has no arm
    // for `redacted_thinking` (verified by ripgrep against
    // refs/anthropic/claude-agent-sdk-recon-2026-06-23/upstream/
    // claude-agent-sdk-pi/index.ts — zero matches). Anthropic emits a
    // redacted_thinking block when the model returns a thinking block
    // that has been safety-redacted server-side; XLI MUST preserve it
    // verbatim for byte-identical replay on the next turn.
    //
    // XLI's projection (sse/messages.rs:657-678) builds a
    // Reasoning{encrypted_content='\u0000REDACTED\u0000<data>',
    // raw_wire_block={type:redacted_thinking, data}}. The sentinel
    // prefix distinguishes redacted thinking from real Anthropic
    // signatures (which are base64 and can never contain null bytes).
    //
    // This fixture uses `_xli_full_sequence` (strict order) because the
    // SSE interleaves the redacted_thinking block BEFORE a regular text
    // block, so the append-mode helper would mis-order the emission.
    assert_pi_projection_with_xli_extension("redacted_thinking_block").await;
}

#[tokio::test]
async fn pi_projection_parallel_block_indices() {
    // RI-033 INVARIANT fence (HI-C5-001 — Parallel content block
    // indices). Two text blocks start in parallel (index 0 + index 1),
    // deltas arrive INTERLEAVED across indices (delta@1 'second' BEFORE
    // delta@0 'first'), and blocks stop in start-order (0 then 1).
    // Both harnesses route deltas by index — pi's `blocks` keyed by
    // index (index.ts:1023) is isomorphic with XLI's BlockTracker
    // HashMap (sse/messages.rs). OutputItemDone fires in stop-arrival
    // order, so the final sequence is [Message('first'),
    // Message('second')] on both wires. A regression that routes
    // deltas FIFO instead of by-index would corrupt this to
    // [Message('second'), Message('first')] — this fence catches that.
    assert_pi_projection_equivalent("parallel_block_indices").await;
}

#[tokio::test]
async fn pi_projection_empty_signed_thinking_drop_xli_subset() {
    // RI-019 ASYMMETRIC fence (BLOCKS_PROD — Empty signed thinking
    // drop, S-OPUS47-EMPTY-THINKING). Opus 4.7 adaptive thinking on
    // the Vertex route streams a signature_delta but withholds every
    // thinking_delta. Pi (index.ts:1107) would emit a thinking block
    // with empty content + the signature on output.content[]; XLI
    // DROPS the block (sse/messages.rs:614-620) because Anthropic's
    // signature verifier rejects replay of a signature computed over
    // content the proxy never returned ('messages.N.content.M:
    // thinking blocks in the latest assistant message cannot be
    // modified').
    //
    // XLI emission is a strict SUBSET of what pi would emit; the
    // fixture uses _xli_full_sequence to lock the post-drop sequence.
    // If a regression re-enables the empty-thinking emission, this
    // fence fires and a downstream production replay would 400.
    assert_pi_projection_with_xli_extension("empty_signed_thinking_drop").await;
}

#[tokio::test]
async fn pi_projection_tool_args_truncated_xli_recovery_policy() {
    // RI-017 ASYMMETRIC fence (BLOCKS_PROD — Truncated tool args /
    // S-004). The wire stream truncates a tool_use input_json_delta
    // mid-string ('{"command": "ls' without closing quote/brace). The
    // two harnesses diverge on RECOVERY POLICY:
    //   - pi accumulates partialJson AND parses live via
    //     parsePartialJson(), recovering a best-effort args Record like
    //     {command:'ls'}; emits toolCall with that recovered record.
    //   - XLI keeps the raw partial_json bytes verbatim on
    //     FunctionCall.arguments AND detects the JSON-parse failure to
    //     override Completed.stop_reason from 'tool_use' to 'max_tokens'
    //     (S-004 retry signal at sse/messages.rs:545-561).
    //
    // The IR-level fence locks XLI's verbatim-bytes shape via
    // _xli_full_sequence (the pi-projected shape cannot be reached
    // through the projection helper because of the recovery
    // divergence). The second assertion locks the stop_reason override
    // — without it, S-004 silently regresses to vanilla 'tool_use'
    // forwarding and the harness loses its retry signal.
    assert_pi_projection_with_xli_extension("tool_args_truncated").await;

    let lines = load_sse_lines("tool_args_truncated");
    let events = collect_messages_events(&lines).await;
    let stop_reason = events
        .iter()
        .find_map(|ev| match ev {
            Ok(ResponseEvent::Completed { stop_reason, .. }) => Some(stop_reason.clone()),
            _ => None,
        })
        .expect("expected a Completed event in the stream");
    assert_eq!(
        stop_reason.as_deref(),
        Some("max_tokens"),
        "RI-017 / S-004: truncated tool_use args must override Completed.stop_reason \
         from 'tool_use' to 'max_tokens' so the harness retries"
    );
}

#[test]
fn pi_projection_helper_panics_on_unknown_block_type() {
    let result = std::panic::catch_unwind(|| {
        project_pi_block_to_response_item(&json!({
            "type": "unknown_future_block_type",
            "details": "this should never happen — drift gate fired"
        }))
    });
    assert!(
        result.is_err(),
        "projection helper must panic on unknown pi block types — that is the drift gate"
    );
}

#[test]
fn pi_projection_helper_handles_mcp_flat_name_split() {
    // Pi delivers tool names as flat wire form (mcp__server__tool); XLI's
    // IR splits them into (namespace, bare_name). This test locks the split
    // policy independently of the SSE pipeline.
    let item = project_pi_block_to_response_item(&json!({
        "type": "toolCall",
        "id": "call_abc",
        "name": "mcp__foo__bar",
        "arguments": {"x": 1}
    }));
    match item {
        ResponseItem::FunctionCall {
            namespace,
            name,
            call_id,
            arguments,
            ..
        } => {
            assert_eq!(namespace.as_deref(), Some("mcp__foo"));
            assert_eq!(name, "bar");
            assert_eq!(call_id, "call_abc");
            let parsed: Value = serde_json::from_str(&arguments).unwrap();
            assert_eq!(parsed, json!({"x": 1}));
        }
        other => panic!("expected FunctionCall, got {other:?}"),
    }
}

#[test]
fn pi_projection_helper_handles_thinking_with_empty_signature() {
    // pi's signature_delta may emit empty string when the model returns
    // a thinking block without a signature (rare; legitimate). The XLI
    // projection must NOT populate encrypted_content in that case — and
    // raw_wire_block must omit the signature field entirely. See
    // sse/messages.rs:622-647 (the symmetric XLI emit path).
    let item = project_pi_block_to_response_item(&json!({
        "type": "thinking",
        "thinking": "hello",
        "thinkingSignature": ""
    }));
    match item {
        ResponseItem::Reasoning {
            encrypted_content,
            raw_wire_block,
            ..
        } => {
            assert!(
                encrypted_content.is_none(),
                "empty thinkingSignature must not become encrypted_content"
            );
            let raw = raw_wire_block.expect("raw_wire_block must be Some");
            assert!(
                raw.get("signature").is_none(),
                "raw_wire_block must omit signature when pi signature is empty"
            );
            assert_eq!(raw.get("thinking").and_then(|v| v.as_str()), Some("hello"));
        }
        other => panic!("expected Reasoning, got {other:?}"),
    }
}
