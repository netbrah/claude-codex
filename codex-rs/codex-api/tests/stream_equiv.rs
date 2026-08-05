//! HCE-05 stream ≡ non-stream equivalence goldens (XLI /messages + /responses).

use codex_api::project_anthropic_assistant_message;
use codex_api::project_responses_api_output;
use codex_api::spawn_messages_stream;
use codex_api::spawn_response_stream;
use codex_api::ApiError;
use codex_api::ResponseEvent;
use codex_client::ByteStream;
use codex_client::StreamResponse;
use codex_client::TransportError;
use codex_protocol::models::ResponseItem;
use futures::StreamExt;
use http::HeaderMap;
use http::StatusCode;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::io::ReaderStream;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/stream_equiv"
);

#[derive(Debug, Deserialize)]
struct Metadata {
    fixture_id: String,
    hi_rows: Vec<String>,
    #[serde(default)]
    ri_rows: Vec<String>,
    spokes: Vec<String>,
    fidelity: String,
}

#[derive(Debug, Deserialize)]
struct PolicyAssertions {
    expected: PolicyExpected,
}

#[derive(Debug, Deserialize)]
struct PolicyExpected {
    stop_reason: Option<String>,
    tool_args_state: Option<String>,
    #[serde(default)]
    no_throw: bool,
    error_kind: Option<String>,
}

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

fn load_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn load_metadata(dir: &Path) -> Metadata {
    serde_json::from_value(load_json(&dir.join("metadata.json"))).expect("metadata.json")
}

fn load_sse_lines(dir: &Path) -> Vec<String> {
    let v = load_json(&dir.join("messages_sse.json"));
    v.as_array()
        .expect("messages_sse.json array")
        .iter()
        .map(|line| line.as_str().expect("sse line string").to_string())
        .collect()
}

fn load_responses_events(dir: &Path) -> Vec<Value> {
    load_json(&dir.join("responses_events.json"))
        .as_array()
        .expect("responses_events.json array")
        .to_vec()
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
            .expect("responses event type");
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

fn extract_output_item_done(events: &[Result<ResponseEvent, ApiError>]) -> Vec<ResponseItem> {
    events
        .iter()
        .filter_map(|ev| match ev {
            Ok(ResponseEvent::OutputItemDone(item)) => Some(item.clone()),
            _ => None,
        })
        .collect()
}

fn extract_completed_stop_reason(events: &[Result<ResponseEvent, ApiError>]) -> Option<String> {
    for ev in events {
        if let Ok(ResponseEvent::Completed { stop_reason, .. }) = ev {
            return stop_reason.clone();
        }
    }
    None
}

fn normalize_for_equiv(items: &[ResponseItem]) -> Value {
    let mut normalized = Vec::new();
    for item in items {
        let mut v = serde_json::to_value(item).expect("serialize item");
        if let Some(obj) = v.as_object_mut() {
            obj.remove("id");
            if obj.get("type") == Some(&Value::String("function_call".into())) {
                if let Some(args) = obj.get("arguments").and_then(|a| a.as_str()) {
                    let parsed = serde_json::from_str::<Value>(args)
                        .unwrap_or_else(|_| Value::String(args.to_string()));
                    obj.insert("arguments".into(), parsed);
                }
            }
            if obj.get("type") == Some(&Value::String("reasoning".into())) {
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

fn policy_path(dir: &Path) -> Option<PathBuf> {
    let xli = dir.join("policy_assertions.xli.json");
    if xli.exists() {
        return Some(xli);
    }
    let generic = dir.join("policy_assertions.json");
    if generic.exists() {
        return Some(generic);
    }
    None
}

fn assert_policy(events: &[Result<ResponseEvent, ApiError>], policy: &PolicyAssertions) {
    if policy.expected.no_throw {
        let first_err = events
            .iter()
            .find_map(|ev| ev.as_ref().err())
            .map(ToString::to_string);
        assert!(
            first_err.is_none(),
            "policy expects no stream error, found {:?}",
            first_err
        );
    }
    if let Some(expected_error) = policy.expected.error_kind.as_deref() {
        let found = events.iter().any(|ev| match (expected_error, ev) {
            ("server_overloaded", Err(ApiError::ServerOverloaded)) => true,
            ("rate_limit", Err(ApiError::RateLimit(_))) => true,
            ("stream", Err(ApiError::Stream(_))) => true,
            _ => false,
        });
        assert!(found, "expected {expected_error} error in stream events");
    }
    if let Some(expected_stop) = policy.expected.stop_reason.as_deref() {
        assert_eq!(
            extract_completed_stop_reason(events).as_deref(),
            Some(expected_stop),
            "stop_reason policy mismatch"
        );
    }
    if let Some(tool_state) = policy.expected.tool_args_state.as_deref() {
        let done = extract_output_item_done(events);
        let calls: Vec<_> = done
            .iter()
            .filter_map(|item| {
                if let ResponseItem::FunctionCall { arguments, .. } = item {
                    Some(arguments.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(!calls.is_empty(), "policy expects at least one FunctionCall");
        match tool_state {
            "invalid_json_retained" => {
                assert!(
                    calls.iter().any(|args| serde_json::from_str::<Value>(args).is_err()),
                    "expected invalid JSON retained in tool args"
                );
            }
            "fallback_empty_object" => {
                assert!(
                    calls.iter().any(|args| *args == "{}"),
                    "expected empty-object fallback"
                );
            }
            other => panic!("unknown tool_args_state policy: {other}"),
        }
    }
}

async fn run_equiv(fixture_name: &str) {
    let dir = fixture_dir(fixture_name);
    let meta = load_metadata(&dir);
    if !meta.spokes.iter().any(|s| s == "xli") {
        return;
    }

    let has_messages_sse = dir.join("messages_sse.json").exists();
    let messages_events = if has_messages_sse {
        let lines = load_sse_lines(&dir);
        collect_messages_events(&lines).await
    } else {
        Vec::new()
    };

    if let Some(policy_file) = policy_path(&dir) {
        let policy: PolicyAssertions = serde_json::from_value(load_json(&policy_file))
            .expect("policy assertions");
        assert_policy(&messages_events, &policy);
        return;
    }

    let non_stream_message_path = dir.join("non_stream_message.json");
    if has_messages_sse && non_stream_message_path.exists() {
        let stream_items = extract_output_item_done(&messages_events);
        let non_stream_message = load_json(&non_stream_message_path);
        let non_stream_items = project_anthropic_assistant_message(&non_stream_message);
        assert_done_items_match(
            &stream_items,
            &non_stream_items,
            &format!("{fixture_name} /messages"),
        );
    }

    let responses_path = dir.join("responses_events.json");
    if responses_path.exists() {
        let responses_events = load_responses_events(&dir);
        let responses_stream_items =
            extract_output_item_done(&collect_responses_events(&responses_events).await);
        let non_stream_response = load_json(&dir.join("non_stream_response.json"));
        let non_stream_response_items = project_responses_api_output(&non_stream_response);
        assert_done_items_match(
            &responses_stream_items,
            &non_stream_response_items,
            &format!("{fixture_name} /responses"),
        );
    }
}

macro_rules! equiv_test {
    ($name:ident, $dir:expr) => {
        #[tokio::test]
        async fn $name() {
            run_equiv($dir).await;
        }
    };
}

equiv_test!(eq_01_text_normal, "eq-01-text-normal");
equiv_test!(eq_02_text_tool_normal, "eq-02-text-tool-normal");
equiv_test!(eq_03_text_thinking_normal, "eq-03-text-thinking-normal");
equiv_test!(eq_04_thinking_tool_normal, "eq-04-thinking-tool-normal");
equiv_test!(eq_05_mcp_namespaced, "eq-05-mcp-namespaced");
equiv_test!(eq_06_parallel_tools, "eq-06-parallel-tools");
equiv_test!(eq_07_tool_with_preamble, "eq-07-tool-with-preamble");
equiv_test!(eq_08_text_ping, "eq-08-text-ping");
equiv_test!(eq_09_tool_ping, "eq-09-tool-ping");
equiv_test!(eq_10_tool_truncated_invalid_json, "eq-10-tool-truncated-invalid-json");
equiv_test!(eq_11_tool_no_stop, "eq-11-tool-no-stop");
equiv_test!(eq_12_parallel_one_bad, "eq-12-parallel-one-bad");
equiv_test!(eq_13_provider_partial, "eq-13-provider-partial");
equiv_test!(eq_14_redacted_thinking, "eq-14-redacted-thinking");
equiv_test!(eq_15_tool_response_only, "eq-15-tool-response-only");
equiv_test!(eq_16_thinking_tool_response, "eq-16-thinking-tool-response");
equiv_test!(eq_20_tool_truncated_mid, "eq-20-tool-truncated-mid");
equiv_test!(eq_21_parameterless_tool, "eq-21-parameterless-tool");
equiv_test!(eq_23_error_overload_sse, "eq-23-error-overload-sse");
equiv_test!(eq_24_error_rate_limit_sse, "eq-24-error-rate-limit-sse");
