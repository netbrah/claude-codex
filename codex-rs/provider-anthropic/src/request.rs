//! Pure, stateless construction of `MessagesApiRequest` and the
//! associated HTTP extra-headers map from a `Prompt` + per-turn inputs.
//!
//! Lifted from `core/src/client.rs::stream_messages_api` in Sortie 1
//! Commit 5b. This module is the wire-specific *request build* half of
//! the hybrid split:
//!
//! - The provider crate (here) owns: `ResponseItem[]` → Anthropic
//!   `messages[]` translation, tool-schema translation, system-block
//!   assembly with cache-control, reasoning-effort → thinking-param
//!   mapping, tool-choice mapping, metadata propagation.
//!
//! - `codex-core` owns: 401 retry loop, per-attempt
//!   `current_client_setup` resolution, Copilot base-url splicing,
//!   telemetry composition, `ApiMessagesClient` construction, response
//!   stream mapping, auth recovery.
//!
//! The two sides communicate through [`MessagesBackend`]
//! (`codex-model-provider::stream`).

use codex_api::MessagesApiMetadata;
use codex_api::MessagesApiRequest;
use codex_models_manager::ProviderCaps;
use codex_prompt::Prompt;
use codex_protocol::config_types::ToolChoice;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use http::HeaderMap;
use http::HeaderValue;

use crate::anthropic_effort_param;
use crate::anthropic_max_output_tokens;
use crate::anthropic_thinking_param;
use crate::conversation_to_anthropic_messages;
use crate::extract_developer_blocks;
use crate::tools_to_anthropic_format;
use codex_model_provider::CacheRetentionByBlockSetting;
use codex_model_provider::CacheRetentionSetting;

/// Sampling knobs passed positionally to keep this crate free of a
/// `codex-config` dep. Fields mirror `codex_config::types::SamplingParams`
/// one-for-one; see
/// [`codex_model_provider::ProviderStreamRequest`] for the contract.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sampling {
    /// Sampling temperature.
    pub temperature: Option<f64>,
    /// Nucleus sampling threshold.
    pub top_p: Option<f64>,
    /// Top-k sampling (Anthropic-only).
    pub top_k: Option<u32>,
}

/// Builds a `MessagesApiRequest` for one Anthropic Messages turn.
///
/// Pure: no I/O, no allocation outside the returned struct, no access
/// to session state. All per-turn inputs are arguments.
///
/// # Semantics preserved from the in-core lift
///
/// - **Developer-role injection.** Developer-role blocks carry
///   `AGENTS.md`, permission directives, and personality config. They
///   must land in `system[]`, not `messages[]` (`BREAK-1` / `W-7`).
/// - **Cache-control placement.** `cache_control: {"type": "ephemeral"}`
///   sits on the **last** system block so Anthropic caches everything
///   from the start of system through every static developer block.
///   Placing it earlier forfeits cache hits on stable developer
///   instructions that rarely change turn-to-turn.
/// - **Adaptive thinking + effort.** Two orthogonal wire knobs:
///   `thinking: {"type": "adaptive"}` is always sent when effort is
///   `Low`..`XHigh` (the model decides *when* to think).
///   `output_config: {"effort": "..."}` controls *how hard* the model
///   works -- mapped from `model_reasoning_effort` via
///   `anthropic_effort_param` (`XHigh` -> `"max"`).
///   `max_tokens` is held at the model-specific output cap so a long
///   deliberation has room to land the final answer.
/// - **Tool-choice clamping.** If no tools are present, `tool_choice`
///   is omitted. Anthropic's `"none"` tool-choice doesn't exist; a
///   caller-supplied `ToolChoice::None` maps to `auto` because tools
///   will simply be absent from the request when `has_tools` is false.
pub fn build_messages_request(
    prompt: &Prompt,
    model_info: &ModelInfo,
    effort: Option<ReasoningEffortConfig>,
    sampling: Sampling,
    tool_choice: Option<&ToolChoice>,
    messages_metadata_user_id: Option<&str>,
    // Controls `output_config.effort` computation:
    // - `None`: compute from `effort` via `anthropic_effort_param`
    //   (Anthropic-direct path).
    // - `Some(val)`: use `val` verbatim, bypassing the standard
    //   mapping. Copilot CAPI passes `Some(effort_to_capi_str(...))`
    //   which may be `None` to suppress `output_config` entirely
    //   for effort levels CAPI rejects.
    output_effort_override: Option<Option<&str>>,
    // Anthropic /messages cache_control retention. Controls the
    // `cache_control` object on system/tool/user blocks.
    cache_retention: CacheRetentionSetting,
    // Per-anchor cache retention overrides (fallback: cache_retention).
    cache_retention_by_block: CacheRetentionByBlockSetting,
    // Top-level `output_config.effort` fallback used when the per-turn
    // `effort` resolves to no string and `output_effort_override` is None.
    model_effort_default: Option<&str>,
) -> MessagesApiRequest {
    let input = prompt.get_formatted_input();
    let supports_image = model_info.input_modalities.contains(&InputModality::Image);

    let provider_caps = ProviderCaps::for_model_info(model_info);

    // Whether the model advertises 1h cache support. Used to gate the
    // `{"type":"ephemeral","ttl":"1h"}` value and the beta header.
    let use_1h = provider_caps.supports_1h_cache;
    let effective_retention = cache_retention;

    let messages = conversation_to_anthropic_messages(
        &input,
        supports_image,
        provider_caps.supports_prompt_caching,
        provider_caps.supports_assistant_prefill,
    );
    let mut anthropic_tools = tools_to_anthropic_format(&prompt.tools);
    // Fix TTL ordering: tools_to_anthropic_format hardcodes {"type":"ephemeral"}
    // (5m) on the last tool, but system blocks may carry a 1h TTL. Anthropic
    // enforces monotonically non-increasing TTLs in processing order
    // (tools -> system -> messages). Overwrite the last tool cache_control
    // using the same retention resolution as system so all blocks agree.
    if let Some(last) = anthropic_tools.last_mut()
        && let Some(obj) = last.as_object_mut()
    {
        let tool_retention = cache_retention_by_block.resolve_tool(effective_retention);
        obj.insert(
            "cache_control".to_owned(),
            cache_control_value(tool_retention, use_1h),
        );
    }

    let developer_blocks = extract_developer_blocks(&input);
    let system = build_system_blocks(
        &prompt.base_instructions.text,
        &developer_blocks,
        provider_caps.supports_prompt_caching,
        cache_retention_by_block.resolve_system(effective_retention),
        use_1h,
    );

    let model_output_cap = provider_caps
        .max_output_tokens
        .and_then(|t| u32::try_from(t).ok())
        .unwrap_or_else(|| anthropic_max_output_tokens(&model_info.slug));
    let thinking = anthropic_thinking_param(effort, &model_info.slug);
    // `max_tokens` is already the cap; the conditional here mirrors the
    // in-core behavior (thinking presence forces the cap). Kept explicit
    // for clarity with the original site.
    let max_tokens: u32 = model_output_cap;

    let has_tools = !anthropic_tools.is_empty();
    let metadata = messages_metadata_user_id.map(|user_id| MessagesApiMetadata {
        user_id: user_id.to_string(),
    });

    let effort_str = match output_effort_override {
        Some(explicit) => explicit,
        // Anthropic-direct path: per-turn effort > model_effort_default.
        None => anthropic_effort_param(effort).or(model_effort_default),
    };
    let output_config = effort_str.map(|e| serde_json::json!({ "effort": e }));

    MessagesApiRequest {
        model: model_info.slug.clone(),
        messages,
        max_tokens,
        stream: true,
        system,
        tools: if has_tools {
            Some(anthropic_tools)
        } else {
            None
        },
        tool_choice: if has_tools {
            Some(messages_api_tool_choice(tool_choice))
        } else {
            None
        },
        thinking,
        output_config,
        temperature: sampling.temperature,
        top_p: sampling.top_p,
        top_k: sampling.top_k,
        stop_sequences: None,
        metadata,
    }
}

/// Builds the `extra_headers` map for one Messages turn.
///
/// Currently carries only the `x-codex-turn-metadata` header. Copilot
/// wire adds `anthropic-beta: prompt-caching-2024-07-31` — that header
/// is Copilot-specific and stays with core's orchestration layer, not
/// with the provider's per-turn request build.
pub fn build_messages_extra_headers(turn_metadata_header: Option<&str>) -> HeaderMap {
    let mut extra_headers = HeaderMap::new();
    if let Some(metadata) = turn_metadata_header
        && let Ok(val) = HeaderValue::from_str(metadata)
    {
        extra_headers.insert("x-codex-turn-metadata", val);
    }
    extra_headers
}

/// Extended variant of [`build_messages_extra_headers`] that also injects
/// the Anthropic `extended-cache-ttl-2025-04-11` beta header when 1h
/// retention is active.
pub fn build_messages_extra_headers_with_retention(
    turn_metadata_header: Option<&str>,
    needs_1h_beta: bool,
) -> HeaderMap {
    let mut extra_headers = HeaderMap::new();
    if let Some(metadata) = turn_metadata_header
        && let Ok(val) = HeaderValue::from_str(metadata)
    {
        extra_headers.insert("x-codex-turn-metadata", val);
    }
    if needs_1h_beta {
        // When 1h TTL is active we need both:
        //   prompt-caching-2024-07-31  — enables cache_control blocks
        //   extended-cache-ttl-2025-04-11 — enables ttl: "1h"
        // Emit them as a single comma-separated value so a subsequent
        // header insert in the caller does not clobber either one.
        extra_headers.insert(
            http::HeaderName::from_static("anthropic-beta"),
            http::HeaderValue::from_static(
                "prompt-caching-2024-07-31,extended-cache-ttl-2025-04-11",
            ),
        );
    }
    extra_headers
}

/// Maps a `ToolChoice` into the Anthropic `tool_choice` JSON shape.
///
/// `ToolChoice::None` falls through to `auto` because on the Anthropic
/// wire, omitting the `tools` array already disables tool use — so the
/// caller should arrange for `has_tools == false` instead of relying
/// on this mapping to express "never".
fn messages_api_tool_choice(tool_choice: Option<&ToolChoice>) -> serde_json::Value {
    match tool_choice {
        None | Some(ToolChoice::Auto) => serde_json::json!({"type": "auto"}),
        Some(ToolChoice::Required) => serde_json::json!({"type": "any"}),
        Some(ToolChoice::Specific { name }) => {
            serde_json::json!({"type": "tool", "name": name})
        }
        Some(ToolChoice::None) => serde_json::json!({"type": "auto"}),
    }
}

/// Assembles the Anthropic `system` parameter from base instructions
/// and developer-role blocks, placing `cache_control` on the last
/// block for optimal prompt caching.
///
/// Returns `None` when both inputs are empty so the caller can omit
/// the field entirely rather than sending `system: []`.
fn build_system_blocks(
    base_instructions: &str,
    developer_blocks: &[String],
    supports_prompt_caching: bool,
    system_retention: CacheRetentionSetting,
    supports_1h_cache: bool,
) -> Option<serde_json::Value> {
    let mut system_parts: Vec<serde_json::Value> = Vec::new();
    if !base_instructions.is_empty() {
        system_parts.push(serde_json::json!({
            "type": "text",
            "text": base_instructions,
        }));
    }
    for dev_text in developer_blocks {
        system_parts.push(serde_json::json!({
            "type": "text",
            "text": dev_text
        }));
    }
    if supports_prompt_caching && let Some(last) = system_parts.last_mut() {
        last["cache_control"] = cache_control_value(system_retention, supports_1h_cache);
    }
    if system_parts.is_empty() {
        None
    } else {
        Some(serde_json::Value::Array(system_parts))
    }
}

/// Builds the `cache_control` JSON value for a given retention setting.
///
/// `OneHour` emits `{"type":"ephemeral","ttl":"1h"}` only when `supports_1h_cache`
/// is `true`; otherwise silently degrades to `{"type":"ephemeral"}`.
pub fn cache_control_value(
    retention: CacheRetentionSetting,
    supports_1h_cache: bool,
) -> serde_json::Value {
    match retention {
        CacheRetentionSetting::OneHour if supports_1h_cache => {
            serde_json::json!({"type": "ephemeral", "ttl": "1h"})
        }
        _ => serde_json::json!({"type": "ephemeral"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn tool_choice_maps_none_and_auto_to_auto() {
        assert_eq!(
            messages_api_tool_choice(None),
            serde_json::json!({"type": "auto"})
        );
        assert_eq!(
            messages_api_tool_choice(Some(&ToolChoice::Auto)),
            serde_json::json!({"type": "auto"})
        );
        // ToolChoice::None also falls through to auto — see the fn
        // docstring for why.
        assert_eq!(
            messages_api_tool_choice(Some(&ToolChoice::None)),
            serde_json::json!({"type": "auto"})
        );
    }

    #[test]
    fn tool_choice_maps_required_to_any() {
        assert_eq!(
            messages_api_tool_choice(Some(&ToolChoice::Required)),
            serde_json::json!({"type": "any"})
        );
    }

    #[test]
    fn tool_choice_maps_specific_to_tool_name() {
        assert_eq!(
            messages_api_tool_choice(Some(&ToolChoice::Specific {
                name: "shell".to_string()
            })),
            serde_json::json!({"type": "tool", "name": "shell"})
        );
    }

    #[test]
    fn system_blocks_are_none_when_inputs_empty() {
        assert!(build_system_blocks("", &[], true, CacheRetentionSetting::Ephemeral, false).is_none());
    }

    #[test]
    fn system_blocks_place_cache_control_on_last() {
        let system = build_system_blocks("base", &["dev1".to_string(), "dev2".to_string()], true, CacheRetentionSetting::Ephemeral, false)
            .expect("three blocks should produce Some");

        let arr = system.as_array().expect("system must be an array");
        assert_eq!(arr.len(), 3);
        // First two blocks: no cache_control.
        assert!(arr[0].get("cache_control").is_none());
        assert!(arr[1].get("cache_control").is_none());
        // Last block carries cache_control: ephemeral.
        assert_eq!(
            arr[2]["cache_control"],
            serde_json::json!({"type": "ephemeral"}),
        );
    }

    #[test]
    fn system_blocks_place_cache_control_on_single_base_only() {
        let system =
            build_system_blocks("base only", &[], true, CacheRetentionSetting::Ephemeral, false).expect("single base should be Some");
        let arr = system.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0]["cache_control"],
            serde_json::json!({"type": "ephemeral"}),
        );
    }

    #[test]
    fn extra_headers_empty_when_no_metadata() {
        let headers = build_messages_extra_headers(None);
        assert!(headers.is_empty());
    }

    #[test]
    fn extra_headers_include_x_codex_turn_metadata() {
        let headers = build_messages_extra_headers(Some("some-json-blob"));
        assert_eq!(
            headers.get("x-codex-turn-metadata").expect("header"),
            "some-json-blob"
        );
    }

    #[test]
    fn extra_headers_silently_drop_invalid_metadata() {
        // HeaderValue::from_str refuses control characters — we swallow
        // the error rather than propagate it because the metadata is
        // a best-effort telemetry bolt-on. Preserves the in-core
        // behavior.
        let headers = build_messages_extra_headers(Some("bad\nvalue"));
        assert!(headers.get("x-codex-turn-metadata").is_none());
    }

    // --- cache_control_value tests ---

    #[test]
    fn cache_control_value_ephemeral_always_emits_standard() {
        assert_eq!(
            cache_control_value(CacheRetentionSetting::Ephemeral, false),
            serde_json::json!({"type": "ephemeral"}),
        );
        assert_eq!(
            cache_control_value(CacheRetentionSetting::Ephemeral, true),
            serde_json::json!({"type": "ephemeral"}),
            "Ephemeral must never add ttl even on capable model"
        );
    }

    #[test]
    fn cache_control_value_one_hour_requires_capable_model() {
        // capable model → extended TTL object
        assert_eq!(
            cache_control_value(CacheRetentionSetting::OneHour, true),
            serde_json::json!({"type": "ephemeral", "ttl": "1h"}),
        );
        // incapable model → silent fallback to 5-minute ephemeral
        assert_eq!(
            cache_control_value(CacheRetentionSetting::OneHour, false),
            serde_json::json!({"type": "ephemeral"}),
            "OneHour must degrade silently on incapable models"
        );
    }

    #[test]
    fn system_blocks_emit_1h_when_capable_and_retention_is_one_hour() {
        let system = build_system_blocks(
            "instructions",
            &[],
            /*supports_prompt_caching*/ true,
            CacheRetentionSetting::OneHour,
            /*supports_1h_cache*/ true,
        )
        .expect("non-empty instructions produce Some");
        let arr = system.as_array().expect("array");
        assert_eq!(
            arr[0]["cache_control"],
            serde_json::json!({"type": "ephemeral", "ttl": "1h"}),
        );
    }

    #[test]
    fn system_blocks_degrade_1h_on_incapable_model() {
        let system = build_system_blocks(
            "instructions",
            &[],
            /*supports_prompt_caching*/ true,
            CacheRetentionSetting::OneHour,
            /*supports_1h_cache*/ false,
        )
        .expect("non-empty instructions produce Some");
        let arr = system.as_array().expect("array");
        // Must fall back to standard ephemeral.
        assert_eq!(
            arr[0]["cache_control"],
            serde_json::json!({"type": "ephemeral"}),
        );
    }

    #[test]
    fn extra_headers_with_retention_injects_beta_when_1h_active() {
        let headers = build_messages_extra_headers_with_retention(None, true);
        assert_eq!(
            headers.get("anthropic-beta").expect("beta header"),
            // Both betas are required when 1h TTL is active:
            //   prompt-caching-2024-07-31     — enables cache_control blocks
            //   extended-cache-ttl-2025-04-11 — enables ttl: "1h"
            "prompt-caching-2024-07-31,extended-cache-ttl-2025-04-11",
        );
    }

    #[test]
    fn extra_headers_with_retention_no_beta_when_not_1h() {
        let headers = build_messages_extra_headers_with_retention(Some("meta"), false);
        assert!(
            headers.get("anthropic-beta").is_none(),
            "beta header must be absent when 1h is not active"
        );
        assert_eq!(
            headers.get("x-codex-turn-metadata").expect("turn metadata"),
            "meta",
        );
    }
}
