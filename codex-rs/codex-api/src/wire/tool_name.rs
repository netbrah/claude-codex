//! OpenAI / Gemini tool-name length cap (64 chars) with deterministic truncation.
//!
//! Ported from LiteLLM `truncate_tool_name` / `create_tool_name_mapping`:
//! `upstream-infrastructure/litellm/.../adapters/transformation.py:28-69`.

use codex_protocol::models::ResponseItem;
use sha2::Digest;
use sha2::Sha256;
use std::borrow::Cow;
use std::collections::HashMap;

pub const OPENAI_MAX_TOOL_NAME_LENGTH: usize = 64;
pub const TOOL_NAME_HASH_LENGTH: usize = 8;
pub const TOOL_NAME_PREFIX_LENGTH: usize = 55;

pub type ToolNameMapping = HashMap<String, String>;

/// Truncate tool names that exceed the OpenAI/Gemini 64-character limit.
///
/// Format: `{55-char-prefix}_{8-char-sha256-hex}` — deterministic and collision-resistant.
pub fn truncate_tool_name(name: &str) -> Cow<'_, str> {
    if name.len() <= OPENAI_MAX_TOOL_NAME_LENGTH {
        return Cow::Borrowed(name);
    }
    let hash = sha256_hex(name.as_bytes());
    let suffix = &hash[..TOOL_NAME_HASH_LENGTH];
    Cow::Owned(format!("{}_{suffix}", &name[..TOOL_NAME_PREFIX_LENGTH]))
}

/// Build truncated → original mapping for wire names that were shortened.
pub fn tool_name_mapping<'a, I>(names: I) -> ToolNameMapping
where
    I: IntoIterator<Item = &'a str>,
{
    let mut mapping = ToolNameMapping::new();
    for original in names {
        let truncated = truncate_tool_name(original);
        if truncated.as_ref() != original {
            mapping.insert(truncated.into_owned(), original.to_string());
        }
    }
    mapping
}

pub fn restore_original_tool_name(truncated: &str, mapping: &ToolNameMapping) -> String {
    mapping
        .get(truncated)
        .cloned()
        .unwrap_or_else(|| truncated.to_string())
}

pub fn restore_original_tool_names_in_item(
    item: &mut ResponseItem,
    mapping: &ToolNameMapping,
) {
    if mapping.is_empty() {
        return;
    }
    match item {
        ResponseItem::FunctionCall { name, namespace, .. } => {
            let wire_name = if let Some(ns) = namespace {
                format!("{ns}{name}")
            } else {
                name.clone()
            };
            let restored_wire = restore_original_tool_name(&wire_name, mapping);
            if let Some(ns) = namespace {
                if restored_wire.starts_with(ns.as_str()) {
                    *name = restored_wire[ns.len()..].to_string();
                } else {
                    *name = restored_wire;
                    *namespace = None;
                }
            } else {
                *name = restored_wire;
            }
        }
        _ => {}
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_name_passes_through() {
        let name = "a".repeat(64);
        assert_eq!(truncate_tool_name(&name), name);
    }

    #[test]
    fn long_name_truncates_with_hash_suffix() {
        let name = "a".repeat(65);
        let truncated = truncate_tool_name(&name);
        assert_eq!(truncated.len(), OPENAI_MAX_TOOL_NAME_LENGTH);
        let parts: Vec<_> = truncated.rsplitn(2, '_').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), TOOL_NAME_HASH_LENGTH);
    }

    #[test]
    fn truncation_is_deterministic() {
        let name = "mcp__server__very_long_namespace__tool_name_that_exceeds_limit".to_string();
        assert_eq!(
            truncate_tool_name(&name),
            truncate_tool_name(&name)
        );
    }

    #[test]
    fn near_identical_long_names_do_not_collide() {
        let a = format!("{}A", "x".repeat(100));
        let b = format!("{}B", "x".repeat(100));
        assert_ne!(truncate_tool_name(&a), truncate_tool_name(&b));
    }

    #[test]
    fn mapping_reverses_truncated_name() {
        let original = "n".repeat(80);
        let mapping = tool_name_mapping([original.as_str()]);
        let truncated = truncate_tool_name(&original);
        assert_eq!(
            restore_original_tool_name(truncated.as_ref(), &mapping),
            original
        );
    }

    #[test]
    fn restore_function_call_item_name() {
        let original = "m".repeat(80);
        let mapping = tool_name_mapping([original.as_str()]);
        let truncated = truncate_tool_name(&original);
        let mut item = ResponseItem::FunctionCall {
            id: None,
            name: truncated.to_string(),
            namespace: None,
            arguments: "{}".to_string(),
            call_id: "call_1".to_string(),
        };
        restore_original_tool_names_in_item(&mut item, &mapping);
        if let ResponseItem::FunctionCall { name, .. } = item {
            assert_eq!(name, original);
        } else {
            panic!("expected FunctionCall");
        }
    }
}
