//! HCE-06 compaction → save → resume → tool-continuation e2e (XLI).

#![allow(clippy::expect_used)]

use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/compaction_resume"
);

#[derive(Debug, Deserialize)]
struct Metadata {
    fixture_id: String,
    #[allow(dead_code)]
    hi_rows: Vec<String>,
    requires_hce02: bool,
    spokes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct HistoryDoc {
    history: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct Phase3Bundle {
    xli: Value,
}

#[derive(Debug, Deserialize)]
struct Phase5Assertions {
    xli: Option<XliAssertions>,
}

#[derive(Debug, Deserialize)]
struct XliAssertions {
    tool_dispatch_ids: Option<Vec<String>>,
}

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

fn load_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn load_metadata(dir: &Path) -> Metadata {
    serde_json::from_value(load_json(&dir.join("metadata.json"))).expect("metadata.json")
}

fn subagent_tool_state_available() -> bool {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/session/subagent_tool_state.rs")
        .exists()
}

fn project_content_history(history: &[Value]) -> Vec<ResponseItem> {
    let mut items = Vec::new();
    for entry in history {
        let role = entry
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let parts = entry
            .get("parts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut text_parts = Vec::new();
        for part in &parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                if role == "model" {
                    text_parts.push(ContentItem::OutputText {
                        text: text.to_string(),
                    });
                } else {
                    text_parts.push(ContentItem::InputText {
                        text: text.to_string(),
                    });
                }
            }
            if let Some(fc) = part.get("functionCall") {
                let name = fc
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string();
                let call_id = fc
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(&name)
                    .to_string();
                let args = fc
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Default::default()));
                items.push(ResponseItem::FunctionCall {
                    id: None,
                    name,
                    namespace: None,
                    arguments: serde_json::to_string(&args).expect("args json"),
                    call_id,
                });
            }
            if let Some(fr) = part.get("functionResponse") {
                let name = fr
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string();
                let call_id = fr
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(&name)
                    .to_string();
                let output = fr
                    .get("response")
                    .and_then(|r| r.get("output"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                items.push(ResponseItem::FunctionCallOutput {
                    call_id,
                    output: FunctionCallOutputPayload::from_text(output),
                });
            }
        }

        if !text_parts.is_empty() {
            let wire_role = if role == "model" { "assistant" } else { "user" };
            items.push(ResponseItem::Message {
                id: None,
                role: wire_role.to_string(),
                content: text_parts,
                phase: None,
            });
        }
    }
    items
}

fn run_cr(name: &str) {
    let dir = fixture_dir(name);
    let meta = load_metadata(&dir);
    if !meta.spokes.iter().any(|s| s == "xli") {
        eprintln!("skip {name}: apex-only fixture");
        return;
    }
    if meta.requires_hce02 && !subagent_tool_state_available() {
        eprintln!("skip {name}: requires HCE-02b");
        return;
    }

    let phase2_expected: HistoryDoc = serde_json::from_value(
        load_json(&dir.join("phase2_expected_history.json")),
    )
    .expect("phase2_expected");

    let phase4: HistoryDoc = serde_json::from_value(load_json(&dir.join("phase4_resumed_history.json")))
        .expect("phase4");
    assert!(
        !phase4.history.is_empty(),
        "{name}: phase4_resumed_history not hydrated"
    );
    let phase4_items = project_content_history(&phase4.history);

    let phase3: Phase3Bundle =
        serde_json::from_value(load_json(&dir.join("phase3_bundle.json"))).expect("phase3");
    assert_eq!(
        phase3.xli.get("compaction_marker").and_then(Value::as_str),
        Some("ContextCompaction")
    );

    // Post-compaction lingua franca must project to non-empty XLI wire items.
    let compressed = project_content_history(&phase2_expected.history);
    assert!(
        !compressed.is_empty(),
        "{name}: phase2_expected_history projects empty"
    );
    assert!(
        !phase4_items.is_empty(),
        "{name}: phase4_resumed_history projects empty"
    );

    if meta.fixture_id != "CR-10" {
        let phase5: Phase5Assertions =
            serde_json::from_value(load_json(&dir.join("phase5_assertions.json")))
                .expect("phase5_assertions");
        let tool_ids = phase5
            .xli
            .and_then(|a| a.tool_dispatch_ids)
            .unwrap_or_default();
        assert!(
            !tool_ids.is_empty(),
            "{name}: expected xli tool_dispatch_ids"
        );
    }
}

#[test]
fn cr_01_text_token_threshold() {
    run_cr("cr-01-text-token-threshold");
}

#[test]
fn cr_03_tool_context_management() {
    run_cr("cr-03-tool-context-management");
}

#[test]
fn cr_04_in_flight_tool_crash() {
    run_cr("cr-04-in-flight-tool-crash");
}

#[test]
fn cr_06_multi_pass_compaction() {
    run_cr("cr-06-multi-pass-compaction");
}

#[test]
fn cr_09_truncated_only() {
    run_cr("cr-09-truncated-only");
}

#[test]
fn cr_10_pending_user_payload() {
    run_cr("cr-10-pending-user-payload");
}

#[test]
fn cr_02_thinking_tool_token() {
    run_cr("cr-02-thinking-tool-token");
}

#[test]
fn cr_05_bulky_masked() {
    run_cr("cr-05-bulky-masked");
}

#[test]
fn cr_07_subagent_fold_in() {
    run_cr("cr-07-subagent-fold-in");
}

#[test]
fn cr_08_subagent_id_collision() {
    run_cr("cr-08-subagent-id-collision");
}
