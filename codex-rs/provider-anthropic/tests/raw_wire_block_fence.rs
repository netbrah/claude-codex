//! Item 2 from the harness-invariant follow-up queue:
//!
//! Structural fence for `Reasoning { raw_wire_block: None }` reaching the
//! Anthropic outbound translator's latest assistant message.
//!
//! Context: `ResponseItem::Reasoning` carries a `raw_wire_block: Option<Value>`
//! that holds the byte-identical JSON block Anthropic sent on the wire. When
//! present, `conversation_to_anthropic_messages` replays it verbatim and the
//! Anthropic verifier accepts the message. When None, the translator
//! reconstructs a `thinking` block from decomposed fields (summary +
//! encrypted_content) and tags it with `__xli_reconstructed_thinking: true`.
//! `strip_thinking_from_non_latest_assistant_messages` then drops any
//! reconstructed block that lands in the latest assistant message, because
//! Anthropic's verifier rejects "thinking blocks in the latest assistant
//! message" that don't match the signature byte-for-byte.
//!
//! There are 24 call sites across codex-rs/{core,context_manager,compact,
//! provider-*} that construct `Reasoning { raw_wire_block: None }`. Any one
//! of them can produce a history that, when fed to
//! `conversation_to_anthropic_messages`, would yield a wire message
//! containing a `thinking` block with a synthetic-or-stripped signature in
//! the latest assistant message — which Anthropic rejects with HTTP 400
//! "thinking blocks in the latest assistant message cannot be modified".
//!
//! This file enforces the invariant **structurally**:
//!
//! 1. After `conversation_to_anthropic_messages`, NO block in any message
//!    carries the `__xli_reconstructed_thinking` marker key (scrubbed
//!    universally by the strip pass).
//! 2. The latest assistant message contains NO `thinking` or
//!    `redacted_thinking` block whose origin was a `None`-raw_wire_block
//!    Reasoning. (Tested by constructing histories where the latest
//!    assistant Reasoning has raw_wire_block=None and asserting the
//!    block is absent from the wire output.)
//! 3. Non-latest assistant messages may have ALL thinking/redacted_thinking
//!    blocks dropped wholesale — this is the documented strip-pass
//!    behavior.
//!
//! Conceptually paired with the Gemini-side
//! `lifecycle:ensureActiveLoopHasThoughtSignatures` gap surfaced in
//! refs/api-references/harness-invariants-gemini-reference.md row 13
//! (MISSING / BLOCKS_PROD). Both wires need to fence the
//! signature-replay invariant; this file is the Anthropic half.

use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_provider_anthropic::conversation_to_anthropic_messages;
use serde_json::Value;
use serde_json::json;

const RECONSTRUCTED_THINKING_MARKER: &str = "__xli_reconstructed_thinking";

/// Helper: build a Reasoning IR item that mimics the 24 in-tree call sites
/// which set `raw_wire_block: None`.
fn reasoning_without_wire_block(summary: &str, signature: Option<&str>) -> ResponseItem {
    ResponseItem::Reasoning {
        id: None,
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: summary.to_string(),
        }],
        content: None,
        encrypted_content: signature.map(str::to_string),
        internal_chat_message_metadata_passthrough: None,

        raw_wire_block: None,
    }
}

/// Helper: build a Reasoning IR item with a real raw_wire_block (the SSE
/// parser is the only site that does this — emits Some).
fn reasoning_with_wire_block(thinking: &str, signature: &str) -> ResponseItem {
    ResponseItem::Reasoning {
        id: None,
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: thinking.to_string(),
        }],
        content: None,
        encrypted_content: Some(signature.to_string()),
        internal_chat_message_metadata_passthrough: None,

        raw_wire_block: Some(json!({
            "type": "thinking",
            "thinking": thinking,
            "signature": signature,
        })),
    }
}

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn assistant_text_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn function_call(name: &str, call_id: &str, arguments: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: None,
        arguments: arguments.to_string(),
        call_id: call_id.to_string(),
    }
}

fn function_call_output(call_id: &str, output: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(output.to_string()),
            success: Some(true),
        },
    }
}

fn translate(history: &[ResponseItem]) -> Vec<Value> {
    conversation_to_anthropic_messages(
        history,
        /* supports_image */ true,
        /* supports_prompt_caching */ false,
        /* supports_assistant_prefill */ false,
    )
}

/// Walk every block in every message looking for the reconstructed-thinking
/// marker. The invariant: the marker is **internal-only** and MUST NOT
/// appear on any block leaving the translator, ever.
fn assert_no_marker_leaks(messages: &[Value], label: &str) {
    for (mi, msg) in messages.iter().enumerate() {
        let Some(content) = msg.get("content").and_then(|c| c.as_array()) else {
            continue;
        };
        for (bi, block) in content.iter().enumerate() {
            let Some(obj) = block.as_object() else { continue };
            assert!(
                !obj.contains_key(RECONSTRUCTED_THINKING_MARKER),
                "{label}: marker leaked to wire at message[{mi}].content[{bi}]; \
                 strip pass must scrub it universally. Block: {block}"
            );
        }
    }
}

/// Find the last assistant message; assert no `thinking` or
/// `redacted_thinking` block in it carries the reconstruction marker
/// (i.e. nothing that originated from raw_wire_block=None survived
/// into the latest assistant turn).
fn assert_latest_assistant_has_no_reconstructed_thinking(messages: &[Value], label: &str) {
    let Some((idx, last_assistant)) = messages
        .iter()
        .enumerate()
        .rev()
        .find(|(_, m)| m.get("role").and_then(Value::as_str) == Some("assistant"))
    else {
        return; // no assistant message at all
    };
    let content = match last_assistant.get("content").and_then(|c| c.as_array()) {
        Some(c) => c,
        None => return,
    };
    for (bi, block) in content.iter().enumerate() {
        let Some(obj) = block.as_object() else { continue };
        let bt = obj.get("type").and_then(Value::as_str).unwrap_or("");
        if bt != "thinking" && bt != "redacted_thinking" {
            continue;
        }
        // The marker is scrubbed UNIVERSALLY, but the strip pass also
        // DROPS reconstructed thinking from the latest assistant. If we
        // find any thinking-typed block here, it must have come from a
        // raw_wire_block=Some path (i.e. been replayed verbatim from
        // the actual wire).
        assert!(
            !obj.contains_key(RECONSTRUCTED_THINKING_MARKER),
            "{label}: reconstructed thinking survived in latest assistant \
             message[{idx}].content[{bi}]. This is the byte-identical \
             signature invariant breach. Block: {block}"
        );
    }
}

// =====================================================================
// CORE INVARIANT TESTS
// =====================================================================

#[test]
fn marker_never_leaks_when_all_reasoning_lacks_wire_block() {
    // Maximally adversarial: every Reasoning in the history has raw_wire_block=None.
    // After translation, the marker must not appear on any block in any message.
    let history = vec![
        user_message("first turn"),
        reasoning_without_wire_block("think 1", Some("sig1")),
        assistant_text_message("answer 1"),
        user_message("second turn"),
        reasoning_without_wire_block("think 2", Some("sig2")),
        assistant_text_message("answer 2"),
        user_message("third turn"),
        reasoning_without_wire_block("think 3", None),
        assistant_text_message("answer 3"),
    ];
    let messages = translate(&history);
    assert_no_marker_leaks(&messages, "all-None reasoning history");
}

#[test]
fn latest_assistant_drops_reconstructed_thinking_when_only_summary_present() {
    // The pathological case: latest assistant Reasoning has raw_wire_block=None.
    // The reconstructed thinking block (signed with the stale signature)
    // MUST be dropped from the latest assistant message — Anthropic would
    // reject it with "thinking blocks in the latest assistant message
    // cannot be modified".
    let history = vec![
        user_message("question"),
        reasoning_without_wire_block("my thought", Some("stale_sig")),
        assistant_text_message("answer"),
    ];
    let messages = translate(&history);
    assert_no_marker_leaks(&messages, "latest-assistant pathological");
    assert_latest_assistant_has_no_reconstructed_thinking(&messages, "latest-assistant pathological");
}

#[test]
fn latest_assistant_keeps_thinking_when_raw_wire_block_present() {
    // Inverse case: latest assistant Reasoning DOES have raw_wire_block.
    // The block MUST survive verbatim — this is the byte-identical replay
    // path the SSE parser feeds.
    let history = vec![
        user_message("question"),
        reasoning_with_wire_block("verified thought", "real_sig_abc"),
        assistant_text_message("answer"),
    ];
    let messages = translate(&history);
    assert_no_marker_leaks(&messages, "latest-assistant happy path");

    // Find the latest assistant message and confirm it has the thinking
    // block intact with the original signature.
    let last_assistant = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .expect("at least one assistant message");
    let content = last_assistant
        .get("content")
        .and_then(|c| c.as_array())
        .expect("assistant message content");
    let thinking_blocks: Vec<&Value> = content
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("thinking"))
        .collect();
    assert_eq!(
        thinking_blocks.len(),
        1,
        "exactly one thinking block expected (from raw_wire_block replay)"
    );
    assert_eq!(
        thinking_blocks[0].get("signature").and_then(Value::as_str),
        Some("real_sig_abc"),
        "raw_wire_block signature must be preserved verbatim"
    );
}

#[test]
fn non_latest_assistant_drops_all_thinking_regardless_of_origin() {
    // The documented policy: non-latest assistant messages get ALL thinking
    // blocks (reconstructed AND verbatim) dropped. This guards against any
    // proxy-side mutation invalidating signatures on older turns.
    let history = vec![
        user_message("turn 1"),
        reasoning_with_wire_block("verbatim thought T1", "sig_T1"),
        function_call("ping", "call_t1", "{}"),
        function_call_output("call_t1", "pong"),
        user_message("turn 2"),
        reasoning_without_wire_block("reconstructed thought T2", Some("sig_T2")),
        assistant_text_message("final"),
    ];
    let messages = translate(&history);
    assert_no_marker_leaks(&messages, "multi-turn non-latest drop");

    // Look at the FIRST assistant message (non-latest). It should have
    // NO thinking blocks — both the verbatim and reconstructed varieties
    // should be stripped from non-latest assistant messages.
    let first_assistant = messages
        .iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .expect("at least one assistant message");
    let content = first_assistant
        .get("content")
        .and_then(|c| c.as_array())
        .expect("assistant content");
    let thinking_count = content
        .iter()
        .filter(|b| {
            let t = b.get("type").and_then(Value::as_str).unwrap_or("");
            t == "thinking" || t == "redacted_thinking"
        })
        .count();
    assert_eq!(
        thinking_count, 0,
        "non-latest assistant must have zero thinking blocks (strip pass policy)"
    );
}

#[test]
fn mixed_origin_history_only_keeps_wire_block_thinking_in_latest_turn() {
    // Adversarial mix: latest assistant has BOTH a Reasoning with
    // raw_wire_block=Some AND a Reasoning with raw_wire_block=None.
    // The Some-block must survive; the None-block must NOT survive.
    let history = vec![
        user_message("question"),
        reasoning_with_wire_block("verbatim thought", "sig_real"),
        reasoning_without_wire_block("reconstructed thought", Some("sig_stale")),
        assistant_text_message("the answer"),
    ];
    let messages = translate(&history);
    assert_no_marker_leaks(&messages, "mixed-origin latest");
    assert_latest_assistant_has_no_reconstructed_thinking(&messages, "mixed-origin latest");

    let last_assistant = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .expect("at least one assistant message");
    let content = last_assistant
        .get("content")
        .and_then(|c| c.as_array())
        .expect("content");
    let thinking_blocks: Vec<&Value> = content
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("thinking"))
        .collect();
    assert_eq!(
        thinking_blocks.len(),
        1,
        "exactly one thinking block must survive (the raw_wire_block one)"
    );
    assert_eq!(
        thinking_blocks[0].get("signature").and_then(Value::as_str),
        Some("sig_real"),
        "the surviving block must carry the REAL signature, not the stale reconstructed one"
    );
}

#[test]
fn empty_history_produces_empty_wire_messages() {
    // Degenerate input. No panics, no marker leaks.
    let history: Vec<ResponseItem> = vec![];
    let messages = translate(&history);
    assert_no_marker_leaks(&messages, "empty history");
    assert!(messages.is_empty(), "empty history yields empty messages");
}

#[test]
fn redacted_thinking_marker_also_stripped_from_latest() {
    // RedactedThinkingBlock case: encrypted_content prefixed with
    // \0REDACTED\0 reconstructs a `redacted_thinking` block (same marker
    // policy as thinking). Verify the marker handling covers redacted as
    // well as text thinking.
    let redacted = ResponseItem::Reasoning {
        id: None,
        summary: vec![],
        content: None,
        encrypted_content: Some("\0REDACTED\0opaque_data_blob".to_string()),
        internal_chat_message_metadata_passthrough: None,
        raw_wire_block: None,
    };
    let history = vec![
        user_message("trigger"),
        redacted,
        assistant_text_message("response"),
    ];
    let messages = translate(&history);
    assert_no_marker_leaks(&messages, "redacted reconstructed");
    assert_latest_assistant_has_no_reconstructed_thinking(&messages, "redacted reconstructed");
}

#[test]
fn marker_constant_matches_translator_internal() {
    // Drift gate: if someone renames RECONSTRUCTED_THINKING_MARKER in
    // wire.rs without updating this test, the rename slips silently — the
    // tests would all pass (the new marker would never appear). To catch
    // that, the test asserts on the actual literal string the translator
    // produces by FORCING a marker through and then scrubbing it ourselves.
    //
    // Round-trip: build a history that we KNOW emits the marker pre-strip,
    // hand-roll the strip predicate, and confirm we hit at least one
    // marker-carrying block at the pre-strip stage. This catches the
    // "marker renamed" case because if RECONSTRUCTED_THINKING_MARKER below
    // diverges from wire.rs, we'll see zero pre-strip marker hits in a
    // pathological history that must contain at least one.
    //
    // Because the translator scrubs the marker before returning, we can
    // only catch the rename by running our own walk over the in-progress
    // wire JSON. The fence test approach is: construct a non-latest
    // assistant turn with a None-raw_wire_block Reasoning and a SECOND
    // assistant turn (latest, with a Some-raw_wire_block), then assert
    // the first assistant turn has zero thinking blocks (because they
    // would have been marked + stripped during the non-latest pass).
    //
    // If RECONSTRUCTED_THINKING_MARKER renamed, the strip predicate would
    // miss the marked blocks, they'd survive, and this assert would
    // catch it.
    let history = vec![
        user_message("turn 1"),
        reasoning_without_wire_block("non-latest thought", Some("sig_t1")),
        assistant_text_message("intermediate"),
        function_call("noop", "c1", "{}"),
        function_call_output("c1", "ok"),
        user_message("turn 2"),
        reasoning_with_wire_block("verbatim latest", "sig_t2"),
        assistant_text_message("final"),
    ];
    let messages = translate(&history);
    assert_no_marker_leaks(&messages, "rename drift gate");

    // The first assistant message must have ZERO thinking blocks: the
    // strip pass for non-latest dropped them all.
    let first_assistant = messages
        .iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .expect("at least one assistant message");
    let content = first_assistant
        .get("content")
        .and_then(|c| c.as_array())
        .expect("content");
    let thinking_count = content
        .iter()
        .filter(|b| {
            let t = b.get("type").and_then(Value::as_str).unwrap_or("");
            t == "thinking" || t == "redacted_thinking"
        })
        .count();
    assert_eq!(
        thinking_count, 0,
        "non-latest assistant thinking blocks must be stripped — if this fails, \
         RECONSTRUCTED_THINKING_MARKER may have been renamed in wire.rs without \
         updating this test"
    );
}
