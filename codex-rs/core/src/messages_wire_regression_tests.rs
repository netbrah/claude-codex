//! Regression guards for the XLI /messages wire additions.
//!
//! If any test in this module fails to COMPILE, it means an upstream merge
//! deleted a type or function that our Messages wire depends on. Check the
//! merge conflict resolution in model_provider_info.rs, client.rs, config/mod.rs.
//!
//! S-020 Sub-A — these are P0 compile-guard smoke tests.

use codex_model_provider_info::WireApi;
use crate::messages_wire::{conversation_to_anthropic_messages, extract_developer_blocks, tools_to_anthropic_format};
use codex_api::{MessagesApiMetadata, MessagesApiRequest};

/// Smoke-compile guard: WireApi::Messages variant must exist.
#[test]
fn wire_api_messages_variant_exists() {
    let _: WireApi = WireApi::Messages;
}

/// Smoke-compile guard: conversation_to_anthropic_messages is callable.
#[test]
fn conversation_to_anthropic_messages_callable() {
    let result = conversation_to_anthropic_messages(&[], true);
    assert!(result.is_empty());
}

/// Smoke-compile guard: extract_developer_blocks is callable.
#[test]
fn extract_developer_blocks_callable() {
    let result = extract_developer_blocks(&[]);
    assert!(result.is_empty());
}

/// Smoke-compile guard: tools_to_anthropic_format is callable.
#[test]
fn tools_to_anthropic_format_callable() {
    let result = tools_to_anthropic_format(&[]);
    assert!(result.is_empty());
}

/// Smoke-compile guard: MessagesApiRequest struct fields exist.
#[test]
fn messages_api_request_fields_exist() {
    let _req = MessagesApiRequest {
        model: "claude-sonnet-4-6".to_string(),
        messages: vec![],
        max_tokens: 1024,
        stream: true,
        system: None,
        tools: None,
        tool_choice: None,
        thinking: None,
        temperature: None,
        top_p: None,
        top_k: None,
        stop_sequences: None,
        metadata: None,
    };
}

/// Smoke-compile guard: MessagesApiMetadata exists with user_id field.
#[test]
fn messages_api_metadata_exists() {
    let _meta = MessagesApiMetadata {
        user_id: "testuser".to_string(),
    };
}

/// Smoke-compile guard: WireApi::Messages roundtrips through serde.
#[test]
fn wire_api_messages_serde_roundtrip() {
    let json = serde_json::to_string(&WireApi::Messages).unwrap();
    assert_eq!(json, r#""messages""#);
    let back: WireApi = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, WireApi::Messages));
}
