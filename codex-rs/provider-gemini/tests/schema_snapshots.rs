#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Snapshot tests for `sanitize_for_gemini` against every concrete XLI tool
//! and a representative MCP fixture set.
//!
//! Tool `parameters` JSON is captured into `tests/fixtures/tool_*.json` from
//! the live tool factories (which live in `codex-core`'s private
//! `tools::handlers` modules and cannot be reached from this crate without a
//! dependency cycle: `codex-core` -> `provider-registry` -> `provider-gemini`).
//! The fixtures freeze the real schema at capture time; regenerate them with
//! the `capture_gemini_tool_fixtures` harness in core when a tool's schema
//! changes (see S-GEMINI-FACTORY-REEXPORT / R5). This keeps the gemini
//! sanitizer contract self-contained and immune to upstream churn in the
//! core-private tool surface.
//!
//! When a new tool ships, add a fixture + snapshot test here so the diff is
//! visible in review. Run `cargo insta review` after changes to approve new
//! snapshots.

use codex_provider_gemini::schema_sanitize::sanitize_for_gemini;
use insta::assert_json_snapshot;

/// Sanitize a captured tool-`parameters` fixture.
fn sanitize_fixture(raw: &str) -> serde_json::Value {
    let schema: serde_json::Value = serde_json::from_str(raw).expect("fixture is valid JSON");
    sanitize_for_gemini(&schema)
}

// ---- Built-in tools (parameters captured from core factories) ----

#[test]
fn shell_tool_schema() {
    assert_json_snapshot!(sanitize_fixture(include_str!("fixtures/tool_shell.json")));
}

#[test]
fn apply_patch_freeform_schema() {
    // Freeform apply_patch has a grammar-based format, not JSON schema.
    // The sanitizer receives the synthetic {input: string} wrapper.
    let wrapper = serde_json::json!({
        "type": "object",
        "properties": {
            "input": {
                "type": "string",
                "description": "The freeform tool content."
            }
        },
        "required": ["input"]
    });
    assert_json_snapshot!(sanitize_for_gemini(&wrapper));
}

#[test]
fn find_files_tool_schema() {
    assert_json_snapshot!(sanitize_fixture(include_str!(
        "fixtures/tool_find_files.json"
    )));
}

#[test]
fn grep_files_tool_schema() {
    assert_json_snapshot!(sanitize_fixture(include_str!(
        "fixtures/tool_grep_files.json"
    )));
}

#[test]
fn read_file_tool_schema() {
    assert_json_snapshot!(sanitize_fixture(include_str!(
        "fixtures/tool_read_file.json"
    )));
}

#[test]
fn view_image_tool_schema() {
    assert_json_snapshot!(sanitize_fixture(include_str!(
        "fixtures/tool_view_image.json"
    )));
}

#[test]
fn analyze_symbol_source_tool_schema() {
    assert_json_snapshot!(sanitize_fixture(include_str!(
        "fixtures/tool_analyze_symbol_source.json"
    )));
}

// ---- Subagent tools ----

#[test]
fn spawn_agent_tool_schema() {
    assert_json_snapshot!(sanitize_fixture(include_str!(
        "fixtures/tool_spawn_agent.json"
    )));
}

#[test]
fn wait_agent_tool_schema() {
    assert_json_snapshot!(sanitize_fixture(include_str!(
        "fixtures/tool_wait_agent.json"
    )));
}

#[test]
fn list_agents_tool_schema() {
    assert_json_snapshot!(sanitize_fixture(include_str!(
        "fixtures/tool_list_agents.json"
    )));
}

// ---- MCP synthetic fixtures ----

#[test]
fn mcp_schema_with_refs() {
    let raw = include_str!("fixtures/mcp_with_refs.json");
    let schema: serde_json::Value = serde_json::from_str(raw).unwrap();
    let sanitized = sanitize_for_gemini(&schema);
    let serialized = serde_json::to_string(&sanitized).unwrap();
    assert!(!serialized.contains("\"$ref\""), "$ref should be inlined");
    assert!(!serialized.contains("\"$defs\""), "$defs should be removed");
    assert_json_snapshot!(sanitized);
}

#[test]
fn mcp_schema_with_pattern() {
    let raw = include_str!("fixtures/mcp_with_pattern.json");
    let schema: serde_json::Value = serde_json::from_str(raw).unwrap();
    let sanitized = sanitize_for_gemini(&schema);
    let serialized = serde_json::to_string(&sanitized).unwrap();
    assert!(
        !serialized.contains("\"pattern\""),
        "pattern should be stripped"
    );
    assert_json_snapshot!(sanitized);
}

#[test]
fn mcp_schema_deep_oneof() {
    let raw = include_str!("fixtures/mcp_deep_oneof.json");
    let schema: serde_json::Value = serde_json::from_str(raw).unwrap();
    let sanitized = sanitize_for_gemini(&schema);
    let serialized = serde_json::to_string(&sanitized).unwrap();
    assert!(
        !serialized.contains("\"oneOf\""),
        "deep oneOf should be dropped"
    );
    assert_json_snapshot!(sanitized);
}

#[test]
fn mcp_schema_with_unknown_format() {
    let raw = include_str!("fixtures/mcp_unknown_format.json");
    let schema: serde_json::Value = serde_json::from_str(raw).unwrap();
    let sanitized = sanitize_for_gemini(&schema);
    assert!(
        sanitized["properties"]["card_number"]
            .get("format")
            .is_none(),
        "credit-card format should be stripped"
    );
    assert_eq!(
        sanitized["properties"]["email"]["format"], "email",
        "email format should be preserved"
    );
    assert_json_snapshot!(sanitized);
}
