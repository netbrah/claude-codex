//! Regression guards for the XLI /messages wire additions.
//!
//! If any test in this module fails to COMPILE, it means an upstream merge
//! deleted a type or function that our Messages wire depends on. Check the
//! merge conflict resolution in model_provider_info.rs, client.rs, config/mod.rs.
//!
//! S-020 Sub-A — these are P0 compile-guard smoke tests.

use crate::wire::conversation_to_anthropic_messages;
use crate::wire::extract_developer_blocks;
use crate::wire::tools_to_anthropic_format;
use codex_api::MessagesApiMetadata;
use codex_api::MessagesApiRequest;
use codex_model_provider_info::WireApi;

/// Smoke-compile guard: WireApi::Messages variant must exist.
#[test]
fn wire_api_messages_variant_exists() {
    let _: WireApi = WireApi::Messages;
}

/// Smoke-compile guard: conversation_to_anthropic_messages is callable.
#[test]
fn conversation_to_anthropic_messages_callable() {
    let result = conversation_to_anthropic_messages(&[], true, true, false);
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
        output_config: None,
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

/// Translator-completeness invariant: every `ToolSpec` variant must have
/// an EXPLICIT decision (translate or drop) in `tools_to_anthropic_format`.
///
/// This test pins the policy at compile time. Adding a new ToolSpec variant
/// upstream forces the match arm here to be updated, which forces the
/// developer to also decide what `tools_to_anthropic_format` does with it.
///
/// Without this guard, an upstream merge that adds a new variant would
/// silently fall into the wildcard `_ => Vec::new()` arm and disappear on
/// the wire — exactly the bug class that hid `ToolSpec::Namespace`
/// (every namespaced MCP tool) from Claude for the entire S-020+ window.
/// See commit ae52f83511.
#[test]
fn tool_spec_translator_completeness() {
    use codex_tools::FreeformTool;
    use codex_tools::FreeformToolFormat;
    use codex_tools::JsonSchema;
    use codex_tools::ResponsesApiNamespace;
    use codex_tools::ResponsesApiTool;
    use codex_tools::ToolSpec;

    let function_schema: JsonSchema = serde_json::from_value(serde_json::json!({
        "type": "object",
        "properties": {},
    }))
    .unwrap();
    // Build one of every variant. The match below MUST be exhaustive —
    // adding a new variant fails to compile until handled here.
    let function = ToolSpec::Function(ResponsesApiTool {
        name: "f".into(),
        description: "d".into(),
        parameters: function_schema.clone(),
        strict: false,
        defer_loading: None,
        output_schema: None,
    });
    let freeform = ToolSpec::Freeform(FreeformTool {
        name: "ff".into(),
        description: "d".into(),
        format: FreeformToolFormat {
            r#type: "t".into(),
            syntax: "s".into(),
            definition: "g".into(),
        },
    });
    let namespace = ToolSpec::Namespace(ResponsesApiNamespace {
        name: "mcp__server__".into(),
        description: "d".into(),
        tools: Vec::new(),
    });
    let search = ToolSpec::ToolSearch {
        execution: "client".into(),
        description: "d".into(),
        parameters: function_schema,
    };
    let image_gen = ToolSpec::ImageGeneration {
        output_format: "png".into(),
    };
    let web_search = ToolSpec::WebSearch {
        external_web_access: None,
        filters: None,
        user_location: None,
        search_content_types: None,
        search_context_size: None,
    };

    let all = [function, freeform, namespace, search, image_gen, web_search];

    // Translate each variant individually and pin the EXPECTED policy.
    // If a variant should NOT translate, document why with an explicit
    // expected count = 0 (intentional drop) — never a silent wildcard.
    let policy: &[(&str, usize)] = &[
        ("Function", 1),        // direct → Anthropic tool
        ("Freeform", 1),        // grammar embedded in description
        ("Namespace", 0),       // empty inner tools list still emits zero
        ("ToolSearch", 1),      // exposed as `tool_search`
        ("ImageGeneration", 0), // no Anthropic equivalent
        ("WebSearch", 0),       // no Anthropic equivalent (yet)
    ];

    for (variant, (name, expected)) in all.iter().zip(policy.iter()) {
        let translated = tools_to_anthropic_format(std::slice::from_ref(variant));
        assert_eq!(
            translated.len(),
            *expected,
            "ToolSpec::{name} translation policy violated: \
             expected {expected} tools, got {} — \
             update tools_to_anthropic_format AND this test together",
            translated.len()
        );
    }

    // Exhaustiveness compile-guard: matching on ToolSpec without a wildcard.
    // If a new variant lands upstream, this `match` fails to compile and
    // forces the dev to add the variant to both the translator and the
    // policy table above.
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

/// Translator-completeness invariant for `ResponseItem` → Anthropic
/// messages translation.
///
/// Same teeth as `tool_spec_translator_completeness`: adding a new
/// `ResponseItem` variant upstream forces the exhaustive match below to
/// fail compilation, which forces the developer to decide what
/// `conversation_to_anthropic_messages` does with it. Without this guard
/// a new ResponseItem variant would silently fall through and
/// disappear from the conversation history sent to Claude — same bug
/// class as ToolSpec::Namespace, just on the conversation side.
///
/// We don't assert exact counts here (each variant has its own
/// dedicated test elsewhere in `wire.rs`); the contract is **the
/// translator does not panic** on any well-formed variant and the
/// match exhaustiveness is enforced at compile time.
#[test]
fn response_item_translator_completeness() {
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::FunctionCallOutputPayload;
    use codex_protocol::models::ResponseItem;

    let items: Vec<ResponseItem> = vec![
        ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText { text: "hi".into() }],
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
            name: "shell".into(),
            namespace: None,
            arguments: r#"{"cmd":"ls"}"#.into(),
            call_id: "call_1".into(),
        },
        ResponseItem::FunctionCallOutput {
            call_id: "call_1".into(),
            output: FunctionCallOutputPayload {
                body: codex_protocol::models::FunctionCallOutputBody::Text("ok".into()),
                success: Some(true),
            },
        },
        ResponseItem::Compaction {
            encrypted_content: "X".into(),
        },
        ResponseItem::Other,
    ];

    // Smoke: must not panic on any of the variants we ship today.
    let _ = conversation_to_anthropic_messages(&items, true, true, false);

    // Compile-time exhaustiveness guard. New variants upstream MUST be
    // added here AND in the wire-side translator's match arms.
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

/// RI-018: thinking SSE sequence preserves byte-identical `raw_wire_block`.
#[test]
fn thinking_sse_round_trip_preserved() {
    use crate::stream_accumulator::StreamAccumulator;
    use crate::stream_accumulator::StreamEvent;
    use crate::stream_invariants::BlockKind;
    use codex_protocol::models::ResponseItem;
    use serde_json::json;

    let expected_raw = json!({
        "type": "thinking",
        "thinking": "Let me think about this",
        "signature": "sig_abc123"
    });

    let mut acc = StreamAccumulator::new();
    acc.accumulate_event(StreamEvent::ContentBlockStart {
        index: 0,
        kind: BlockKind::Thinking,
    })
    .unwrap();
    acc.accumulate_event(StreamEvent::ContentBlockDelta {
        index: 0,
        delta_type: "thinking_delta".to_owned(),
        text: None,
        thinking: Some("Let me think".to_owned()),
        signature: None,
        partial_json: None,
    })
    .unwrap();
    acc.accumulate_event(StreamEvent::ContentBlockDelta {
        index: 0,
        delta_type: "thinking_delta".to_owned(),
        text: None,
        thinking: Some(" about this".to_owned()),
        signature: None,
        partial_json: None,
    })
    .unwrap();
    acc.accumulate_event(StreamEvent::ContentBlockDelta {
        index: 0,
        delta_type: "signature_delta".to_owned(),
        text: None,
        thinking: None,
        signature: Some("sig_abc123".to_owned()),
        partial_json: None,
    })
    .unwrap();
    acc.accumulate_event(StreamEvent::ContentBlockStop { index: 0 })
        .unwrap();
    acc.accumulate_event(StreamEvent::MessageStop).unwrap();

    assert_eq!(acc.finished_items().len(), 1);
    let ResponseItem::Reasoning { raw_wire_block, .. } = &acc.finished_items()[0] else {
        panic!("expected Reasoning item");
    };
    assert_eq!(raw_wire_block.as_ref().unwrap(), &expected_raw);
}
