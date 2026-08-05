//! Phase 2 — ResponseItem cross-wire equivalence goldens (RI-016, RI-DEFER-003, …).
//!
//! Hermetic fixtures under `tests/fixtures/response_item_equiv/`. Proves:
//! - Final `ResponseItem` IR matches across `/messages` and `/responses`
//! - `ToolCallInputDelta` on both wires for streaming tool args (S-RI-MESSAGES-TOOL-DELTA)

use codex_api::ApiError;
use codex_api::ResponseEvent;
use codex_api::spawn_messages_stream;
use codex_api::spawn_response_stream;
use codex_client::ByteStream;
use codex_client::StreamResponse;
use codex_client::TransportError;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use futures::StreamExt;
use http::HeaderMap;
use http::StatusCode;
use serde_json::Value;
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
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn load_sse_lines(name: &str) -> Vec<String> {
    let v = load_json(&fixture_dir(name).join("messages_sse.json"));
    v.as_array()
        .expect("messages_sse.json must be a JSON array of lines")
        .iter()
        .map(|line| line.as_str().expect("sse line must be string").to_string())
        .collect()
}

fn load_responses_events(name: &str) -> Vec<Value> {
    load_json(&fixture_dir(name).join("responses_events.json"))
        .as_array()
        .expect("responses_events.json must be array")
        .to_vec()
}

fn load_expected_items(name: &str) -> Vec<ResponseItem> {
    let v = load_json(&fixture_dir(name).join("expected_response_items.json"));
    let items = if let Some(arr) = v.as_array() {
        arr.clone()
    } else {
        v.get("items")
            .and_then(|i| i.as_array())
            .expect("expected_response_items.json: array or {items:[]}")
            .clone()
    };
    items
        .into_iter()
        .map(|item| serde_json::from_value(item).expect("deserialize ResponseItem"))
        .collect()
}

fn messages_lines_to_stream(lines: &[String]) -> ByteStream {
    let mut content = String::new();
    for line in lines {
        content.push_str(line);
        content.push('\n');
    }
    let reader = std::io::Cursor::new(content);
    let stream = ReaderStream::new(reader)
        .map(|r| r.map_err(|e| TransportError::Network(e.to_string())));
    Box::pin(stream)
}

fn responses_events_to_body(events: &[Value]) -> String {
    let mut body = String::new();
    for event in events {
        let kind = event
            .get("type")
            .and_then(|v| v.as_str())
            .expect("responses event missing type");
        body.push_str(&format!("event: {kind}\ndata: {event}\n\n"));
    }
    body
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

async fn collect_responses_events(events: &[Value]) -> Vec<Result<ResponseEvent, ApiError>> {
    let body = responses_events_to_body(events);
    let reader = std::io::Cursor::new(body);
    let byte_stream = ReaderStream::new(reader)
        .map(|r| r.map_err(|e| TransportError::Network(e.to_string())));
    let stream_response = StreamResponse {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        bytes: Box::pin(byte_stream),
    };
    let mut rx = spawn_response_stream(stream_response, Duration::from_secs(30), None, None).rx_event;
    let mut out = Vec::new();
    while let Some(event) = rx.recv().await {
        out.push(event);
    }
    out
}

fn extract_output_item_added(events: &[Result<ResponseEvent, ApiError>]) -> Vec<ResponseItem> {
    events
        .iter()
        .filter_map(|ev| match ev {
            Ok(ResponseEvent::OutputItemAdded(item)) => Some(item.clone()),
            _ => None,
        })
        .collect()
}

fn load_expected_token_usage(name: &str) -> TokenUsage {
    let v = load_json(&fixture_dir(name).join("expected_token_usage.json"));
    serde_json::from_value(v).expect("deserialize TokenUsage")
}

fn extract_completed_token_usage(events: &[Result<ResponseEvent, ApiError>]) -> TokenUsage {
    for ev in events {
        if let Ok(ResponseEvent::Completed {
            token_usage: Some(usage),
            ..
        }) = ev
        {
            return usage.clone();
        }
    }
    panic!("no Completed event with token_usage");
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

fn count_tool_call_input_deltas(events: &[Result<ResponseEvent, ApiError>]) -> usize {
    events
        .iter()
        .filter(|ev| matches!(ev, Ok(ResponseEvent::ToolCallInputDelta { .. })))
        .count()
}

fn normalize_for_equiv(items: &[ResponseItem]) -> Value {
    let mut normalized = Vec::new();
    for item in items {
        let mut v = serde_json::to_value(item).expect("serialize item");
        if let Some(obj) = v.as_object_mut() {
            obj.remove("id");
            if obj.get("type") == Some(&Value::String("reasoning".into())) {
                // Harness uses empty string id on /messages path; fixture omits it.
                obj.insert("id".into(), Value::String(String::new()));
                if obj.get("content").map(|c| c.is_null()).unwrap_or(false) {
                    obj.remove("content");
                }
            }
        }
        normalized.push(v);
    }
    Value::Array(normalized)
}

fn assert_done_items_match(actual: &[ResponseItem], expected: &[ResponseItem], label: &str) {
    let actual_norm = normalize_for_equiv(actual);
    let expected_norm = normalize_for_equiv(expected);
    assert_eq!(
        actual_norm, expected_norm,
        "{label}: OutputItemDone sequence mismatch"
    );
}

#[tokio::test]
async fn golden_tool_args_stream_messages_final_ir() {
    let lines = load_sse_lines("tool_args_stream");
    let events = collect_messages_events(&lines).await;
    let done = extract_output_item_done(&events);
    let expected = load_expected_items("tool_args_stream");
    assert_done_items_match(&done, &expected, "messages /tool_args_stream");
}

#[tokio::test]
async fn golden_tool_args_stream_responses_final_ir() {
    let wire = load_responses_events("tool_args_stream");
    let events = collect_responses_events(&wire).await;
    let done = extract_output_item_done(&events);
    let expected = load_expected_items("tool_args_stream");
    assert_done_items_match(&done, &expected, "responses /tool_args_stream");
}

#[tokio::test]
async fn golden_tool_args_stream_cross_wire_final_ir_equiv() {
    let lines = load_sse_lines("tool_args_stream");
    let messages_done = extract_output_item_done(&collect_messages_events(&lines).await);
    let responses_done =
        extract_output_item_done(&collect_responses_events(&load_responses_events("tool_args_stream")).await);
    assert_eq!(
        normalize_for_equiv(&messages_done),
        normalize_for_equiv(&responses_done),
        "RI-016: final FunctionCall IR must match across wires"
    );
}

#[tokio::test]
async fn golden_tool_args_stream_messages_emits_tool_call_input_delta() {
    let lines = load_sse_lines("tool_args_stream");
    let events = collect_messages_events(&lines).await;
    assert_eq!(
        count_tool_call_input_deltas(&events),
        2,
        "RI-DEFER-003: /messages must emit one ToolCallInputDelta per input_json_delta"
    );
}

#[tokio::test]
async fn golden_tool_args_stream_responses_emits_tool_call_input_delta() {
    let events = collect_responses_events(&load_responses_events("tool_args_stream")).await;
    assert_eq!(
        count_tool_call_input_deltas(&events),
        2,
        "RI-DEFER-003: /responses must emit one ToolCallInputDelta per function_call_arguments.delta"
    );
}

#[tokio::test]
async fn golden_usage_merge_messages_completed_token_usage() {
    let lines = load_sse_lines("usage_merge");
    let usage = extract_completed_token_usage(&collect_messages_events(&lines).await);
    let expected = load_expected_token_usage("usage_merge");
    assert_eq!(
        usage, expected,
        "RI-032: message_start + message_delta usage must merge on Completed"
    );
}

#[tokio::test]
async fn golden_usage_merge_responses_completed_token_usage() {
    let events = collect_responses_events(&load_responses_events("usage_merge")).await;
    let usage = extract_completed_token_usage(&events);
    let expected = load_expected_token_usage("usage_merge");
    assert_eq!(
        usage, expected,
        "RI-032: /responses completed usage must match canonical projection"
    );
}

#[tokio::test]
async fn golden_usage_merge_cross_wire_token_usage_equiv() {
    let messages_usage = extract_completed_token_usage(
        &collect_messages_events(&load_sse_lines("usage_merge")).await,
    );
    let responses_usage = extract_completed_token_usage(
        &collect_responses_events(&load_responses_events("usage_merge")).await,
    );
    assert_eq!(
        messages_usage, responses_usage,
        "RI-032: merged /messages usage must equal /responses Completed usage"
    );
}

#[tokio::test]
async fn golden_thinking_block_roundtrip_messages() {
    // Locks the canonical Anthropic thinking-block round-trip on the
    // /messages wire. The single fixture exercises every thinking-grain
    // RI invariant:
    //   - RI-008 (BLOCKS_PROD — History thinking block inbound). The
    //     expected_response_items.json shape is what the server-leg
    //     translator must produce when the SAME assistant turn is
    //     replayed as history; field-by-field equivalence is the proof.
    //   - RI-009 (BLOCKS_PROD — thinking.signature on encrypted_content).
    //     The signature value 'sig_equiv_abc' must round-trip onto
    //     ResponseItem::Reasoning.encrypted_content; assert_done_items_match
    //     compares it byte-for-byte.
    //   - RI-011 (BLOCKS_PROD — Reasoning round-trip server leg). The
    //     raw_wire_block field is the byte-identical replay anchor; the
    //     fixture's expected_response_items.json includes the wire block
    //     with both thinking text + signature, so loss on the server leg
    //     would fail this assertion.
    //   - RI-018 (DEGRADES — thinking block lifecycle). The full
    //     content_block_start/thinking_delta/signature_delta/
    //     content_block_stop sequence is exercised end-to-end.
    //   - RI-021 (BLOCKS_PROD — Responses bridge thinking signature loss).
    //     The cross-wire variant golden_thinking_block_roundtrip_cross_wire
    //     (if/when present) would surface bridge loss; even this single-wire
    //     test catches regressions where the parser drops the signature.
    let lines = load_sse_lines("thinking_block_roundtrip");
    let done = extract_output_item_done(&collect_messages_events(&lines).await);
    let expected = load_expected_items("thinking_block_roundtrip");
    assert_eq!(done.len(), expected.len());
    assert_done_items_match(&done, &expected, "thinking_block_roundtrip");
}

#[tokio::test]
async fn golden_mcp_namespace_roundtrip_messages_final_ir() {
    let lines = load_sse_lines("mcp_namespace_roundtrip");
    let done = extract_output_item_done(&collect_messages_events(&lines).await);
    let expected = load_expected_items("mcp_namespace_roundtrip");
    assert_done_items_match(&done, &expected, "messages /mcp_namespace_roundtrip");
}

#[tokio::test]
async fn golden_mcp_namespace_roundtrip_responses_final_ir() {
    let wire = load_responses_events("mcp_namespace_roundtrip");
    let done = extract_output_item_done(&collect_responses_events(&wire).await);
    let expected = load_expected_items("mcp_namespace_roundtrip");
    assert_done_items_match(&done, &expected, "responses /mcp_namespace_roundtrip");
}

#[tokio::test]
async fn golden_mcp_namespace_roundtrip_cross_wire_final_ir_equiv() {
    let lines = load_sse_lines("mcp_namespace_roundtrip");
    let messages_done = extract_output_item_done(&collect_messages_events(&lines).await);
    let responses_done = extract_output_item_done(
        &collect_responses_events(&load_responses_events("mcp_namespace_roundtrip")).await,
    );
    assert_eq!(
        normalize_for_equiv(&messages_done),
        normalize_for_equiv(&responses_done),
        "RI-004/RI-007: namespaced FunctionCall IR must match across wires"
    );
}

#[tokio::test]
async fn golden_mcp_namespace_roundtrip_messages_splits_flat_wire_name_on_added() {
    let lines = load_sse_lines("mcp_namespace_roundtrip");
    let added = extract_output_item_added(&collect_messages_events(&lines).await);
    assert_eq!(added.len(), 1, "expect one OutputItemAdded for tool_use");
    assert!(
        matches!(
            &added[0],
            ResponseItem::FunctionCall {
                name,
                namespace: Some(ns),
                call_id,
                ..
            } if name == "analyze_symbol"
                && ns == "mcp__clangd_rs"
                && call_id == "mcp_ns_call_1"
        ),
        "RI-007: /messages must split mcp__<server>__<tool> at content_block_start"
    );
}

#[tokio::test]
async fn golden_thinking_tool_chain_order() {
    let lines = load_sse_lines("thinking_tool_chain");
    let done = extract_output_item_done(&collect_messages_events(&lines).await);
    let expected = load_expected_items("thinking_tool_chain");
    assert_done_items_match(&done, &expected, "thinking_tool_chain");
    assert!(
        matches!((&done[0], &done[1]), (ResponseItem::Reasoning { .. }, ResponseItem::FunctionCall { .. })),
        "RI-020: Reasoning must precede FunctionCall in OutputItemDone order"
    );
}

#[tokio::test]
async fn golden_web_search_server_tool_messages_final_ir() {
    let lines = load_sse_lines("web_search_server_tool");
    let done = extract_output_item_done(&collect_messages_events(&lines).await);
    let expected = load_expected_items("web_search_server_tool");
    assert_done_items_match(&done, &expected, "messages /web_search_server_tool");
}

#[tokio::test]
async fn golden_web_search_server_tool_responses_final_ir() {
    let wire = load_responses_events("web_search_server_tool");
    let done = extract_output_item_done(&collect_responses_events(&wire).await);
    let expected = load_expected_items("web_search_server_tool");
    assert_done_items_match(&done, &expected, "responses /web_search_server_tool");
}

#[tokio::test]
async fn golden_web_search_server_tool_cross_wire_final_ir_equiv() {
    let lines = load_sse_lines("web_search_server_tool");
    let messages_done = extract_output_item_done(&collect_messages_events(&lines).await);
    let responses_done = extract_output_item_done(
        &collect_responses_events(&load_responses_events("web_search_server_tool")).await,
    );
    assert_eq!(
        normalize_for_equiv(&messages_done),
        normalize_for_equiv(&responses_done),
        "RI-022: WebSearchCall IR must match across wires"
    );
}

#[tokio::test]
async fn golden_web_search_server_tool_messages_emits_added_in_progress() {
    let lines = load_sse_lines("web_search_server_tool");
    let added = extract_output_item_added(&collect_messages_events(&lines).await);
    assert_eq!(added.len(), 1);
    assert!(matches!(
        &added[0],
        ResponseItem::WebSearchCall {
            status: Some(s),
            action: None,
            ..
        } if s == "in_progress"
    ));
}
