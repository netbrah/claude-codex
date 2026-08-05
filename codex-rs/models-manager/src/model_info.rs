use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::openai_models::ApplyPatchToolType;
use codex_protocol::openai_models::ConfigShellToolType;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelInstructionsVariables;
use codex_protocol::openai_models::ModelMessages;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::TruncationMode;
use codex_protocol::openai_models::TruncationPolicyConfig;
use codex_protocol::openai_models::WebSearchToolType;
use codex_protocol::openai_models::default_input_modalities;

use crate::config::ModelsManagerConfig;
use codex_utils_output_truncation::approx_bytes_for_tokens;
use tracing::warn;

pub const BASE_INSTRUCTIONS: &str = include_str!("../prompt.md");
const DEFAULT_PERSONALITY_HEADER: &str = "You are Codex, a coding agent based on GPT-5. You and the user share the same workspace and collaborate to achieve the user's goals.";
const LOCAL_FRIENDLY_TEMPLATE: &str =
    "You optimize for team morale and being a supportive teammate as much as code quality.";
const LOCAL_PRAGMATIC_TEMPLATE: &str = "You are a deeply pragmatic, effective software engineer.";
const PERSONALITY_PLACEHOLDER: &str = "{{ personality }}";

pub fn with_config_overrides(mut model: ModelInfo, config: &ModelsManagerConfig) -> ModelInfo {
    if let Some(supports_reasoning_summaries) = config.model_supports_reasoning_summaries
        && supports_reasoning_summaries
    {
        model.supports_reasoning_summaries = true;
    }
    if let Some(context_window) = config.model_context_window {
        model.context_window = Some(
            model
                .max_context_window
                .map_or(context_window, |max_context_window| {
                    context_window.min(max_context_window)
                }),
        );
    }
    if let Some(auto_compact_token_limit) = config.model_auto_compact_token_limit {
        model.auto_compact_token_limit = Some(auto_compact_token_limit);
    }
    if let Some(token_limit) = config.tool_output_token_limit {
        model.truncation_policy = match model.truncation_policy.mode {
            TruncationMode::Bytes => {
                let byte_limit =
                    i64::try_from(approx_bytes_for_tokens(token_limit)).unwrap_or(i64::MAX);
                TruncationPolicyConfig::bytes(byte_limit)
            }
            TruncationMode::Tokens => {
                let limit = i64::try_from(token_limit).unwrap_or(i64::MAX);
                TruncationPolicyConfig::tokens(limit)
            }
        };
    }

    if let Some(base_instructions) = &config.base_instructions {
        model.base_instructions = base_instructions.clone();
        model.model_messages = None;
    } else if !config.personality_enabled {
        model.model_messages = None;
    }

    model
}

/// Build a minimal fallback model descriptor for missing/unknown slugs.
///
/// Applies family-aware defaults for recognized model families (Claude,
/// GPT-5) so the fallback metadata is not wildly wrong even when the
/// slug is absent from the bundled catalog (common for Copilot-routed
/// models and new releases).
pub fn model_info_from_slug(slug: &str) -> ModelInfo {
    let family = classify_model_family(slug);
    let (context_window, parallel_tools, vision) = match family {
        ModelFamily::Claude => (claude_context_window(slug), true, true),
        ModelFamily::Gemini => (gemini_context_window(slug), true, true),
        ModelFamily::Gpt5 => (272_000, true, true),
        ModelFamily::Unknown => (272_000, false, false),
    };
    if matches!(family, ModelFamily::Unknown) {
        warn!("Unknown model {slug} is used. This will use fallback model metadata.");
    }
    let input_modalities = if vision {
        default_input_modalities()
    } else {
        vec![InputModality::Text]
    };
    ModelInfo {
        slug: slug.to_string(),
        display_name: slug.to_string(),
        description: None,
        default_reasoning_level: None,
        supported_reasoning_levels: Vec::new(),
        shell_type: ConfigShellToolType::Default,
        visibility: ModelVisibility::None,
        supported_in_api: true,
        priority: 99,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        availability_nux: None,
        upgrade: None,
        base_instructions: BASE_INSTRUCTIONS.to_string(),
        model_messages: local_personality_messages_for_slug(slug),
        supports_reasoning_summaries: false,
        default_reasoning_summary: ReasoningSummary::Auto,
        support_verbosity: false,
        default_verbosity: None,
        // Family-aware fallback: Claude (Anthropic /messages, including
        // Copilot Claude) and Gemini both honor the freeform apply_patch
        // tool — we shipped it that way pre-2026-05-29 upstream merge.
        // The upstream tool-planning refactor (spec_plan.rs) now gates
        // apply_patch registration on `apply_patch_tool_type.is_some()`,
        // so this fallback must announce freeform for non-GPT families
        // or apply_patch silently disappears from every Claude/Gemini
        // profile. GPT-5 metadata comes from the bundled models.json.
        apply_patch_tool_type: match family {
            ModelFamily::Claude | ModelFamily::Gemini => Some(ApplyPatchToolType::Freeform),
            ModelFamily::Gpt5 | ModelFamily::Unknown => None,
        },
        web_search_tool_type: WebSearchToolType::Text,
        truncation_policy: TruncationPolicyConfig::bytes(/*limit*/ 10_000),
        supports_parallel_tool_calls: parallel_tools,
        supports_image_detail_original: false,
        context_window: Some(context_window),
        max_context_window: Some(context_window),
        auto_compact_token_limit: None,
        effective_context_window_percent: 95,
        experimental_supported_tools: Vec::new(),
        input_modalities,
        used_fallback_model_metadata: matches!(family, ModelFamily::Unknown),
        supports_search_tool: false,
    }
}

/// Recognized model families for fallback metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelFamily {
    Claude,
    Gemini,
    Gpt5,
    Unknown,
}

/// Context windows and output caps per Claude sub-family.
///
/// Derived from LiteLLM `/v1/model/info` on 2026-04-24:
///   4.5 generation → 200K input
///   4.6+ generation → 1M input
///   Haiku max_output → 8K; Sonnet → 64K; Opus → 128K
fn claude_context_window(slug: &str) -> i64 {
    let s = slug.to_ascii_lowercase();
    if s.contains("4.5") || s.contains("4-5") {
        return 200_000;
    }
    // 4.6, 4.7, and any future generation default to 1M.
    1_000_000
}

/// Context window for Gemini fallback. Gemini 1.5 / 2.x / 3.x all ship 1M
/// input on Vertex; older generations had 32K-128K but we don't surface
/// them as profiles. LiteLLM enrichment overrides this with the live
/// catalog value when reachable.
fn gemini_context_window(slug: &str) -> i64 {
    let s = slug.to_ascii_lowercase();
    if s.contains("1.0") || s.contains("1-0") {
        return 32_000;
    }
    1_048_576
}

/// Output cap for Gemini fallback. 3.x generation ships ~64K; 2.x and 1.5
/// shipped 8K. LiteLLM enrichment overrides this at runtime.
/// Classify a model slug into a known family for smarter fallback defaults.
fn classify_model_family(slug: &str) -> ModelFamily {
    let s = slug.to_ascii_lowercase();
    if s.contains("claude") {
        return ModelFamily::Claude;
    }
    if s.contains("gemini") {
        return ModelFamily::Gemini;
    }
    if s.starts_with("gpt-5") {
        return ModelFamily::Gpt5;
    }
    ModelFamily::Unknown
}

fn local_personality_messages_for_slug(slug: &str) -> Option<ModelMessages> {
    match slug {
        "gpt-5.2-codex" | "exp-codex-personality" => Some(ModelMessages {
            instructions_template: Some(format!(
                "{DEFAULT_PERSONALITY_HEADER}\n\n{PERSONALITY_PLACEHOLDER}\n\n{BASE_INSTRUCTIONS}"
            )),
            instructions_variables: Some(ModelInstructionsVariables {
                personality_default: Some(String::new()),
                personality_friendly: Some(LOCAL_FRIENDLY_TEMPLATE.to_string()),
                personality_pragmatic: Some(LOCAL_PRAGMATIC_TEMPLATE.to_string()),
            }),
        }),
        _ => None,
    }
}

#[cfg(test)]
#[path = "model_info_tests.rs"]
mod tests;
