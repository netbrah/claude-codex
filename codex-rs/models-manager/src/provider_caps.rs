use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::RwLock;

use codex_protocol::openai_models::ModelInfo;

/// Provider-specific capability flags outside the upstream `ModelInfo` lingua-franca.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProviderCaps {
    pub max_output_tokens: Option<i64>,
    pub supports_prompt_caching: bool,
    pub supports_assistant_prefill: bool,
    /// Whether the model supports the extended 1-hour cache TTL.
    /// When `true`, `cache_control: {"type":"ephemeral","ttl":"1h"}` is valid
    /// and the `extended-cache-ttl-2025-04-11` beta header must be sent.
    /// When `false`, `"1h"` retention requests silently degrade to `"ephemeral"`.
    pub supports_1h_cache: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProviderCapsPatch {
    pub max_output_tokens: Option<Option<i64>>,
    pub supports_prompt_caching: Option<bool>,
    pub supports_assistant_prefill: Option<bool>,
    pub supports_1h_cache: Option<bool>,
}

static OVERRIDES: LazyLock<RwLock<HashMap<String, ProviderCaps>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

impl ProviderCaps {
    pub fn for_model(slug: &str) -> Self {
        if let Some(caps) = Self::override_for(slug) {
            return caps;
        }
        Self::static_for_slug(slug)
    }

    pub fn for_model_info(info: &ModelInfo) -> Self {
        Self::for_model(&info.slug)
    }

    pub fn set_override(slug: &str, caps: ProviderCaps) {
        if let Ok(mut overrides) = OVERRIDES.write() {
            overrides.insert(slug.to_string(), caps);
        }
    }

    pub fn merge_override(slug: &str, patch: ProviderCapsPatch) {
        let mut caps = Self::for_model(slug);
        if let Some(max_output_tokens) = patch.max_output_tokens {
            caps.max_output_tokens = max_output_tokens;
        }
        if let Some(supports_prompt_caching) = patch.supports_prompt_caching {
            caps.supports_prompt_caching = supports_prompt_caching;
        }
        if let Some(supports_assistant_prefill) = patch.supports_assistant_prefill {
            caps.supports_assistant_prefill = supports_assistant_prefill;
        }
        if let Some(supports_1h_cache) = patch.supports_1h_cache {
            caps.supports_1h_cache = supports_1h_cache;
        }
        Self::set_override(slug, caps);
    }

    #[cfg(test)]
    pub fn clear_overrides() {
        if let Ok(mut overrides) = OVERRIDES.write() {
            overrides.clear();
        }
    }

    fn override_for(slug: &str) -> Option<Self> {
        OVERRIDES.read().ok()?.get(slug).copied()
    }

    fn static_for_slug(slug: &str) -> Self {
        let family = classify_model_family(slug);
        Self {
            max_output_tokens: match family {
                ModelFamily::Claude => Some(claude_max_output_tokens(slug)),
                ModelFamily::Gemini => Some(gemini_max_output_tokens(slug)),
                _ => None,
            },
            supports_prompt_caching: matches!(
                family,
                ModelFamily::Claude | ModelFamily::Gemini | ModelFamily::Gpt5
            ),
            supports_assistant_prefill: claude_supports_assistant_prefill(slug, family),
            supports_1h_cache: claude_supports_1h_cache(slug, family),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelFamily {
    Claude,
    Gemini,
    Gpt5,
    Unknown,
}

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

fn claude_max_output_tokens(slug: &str) -> i64 {
    let normalized = slug.to_lowercase();
    if normalized.contains("opus") {
        return 128_000;
    }
    if normalized.contains("haiku") {
        return 8_192;
    }
    64_000
}

fn gemini_max_output_tokens(slug: &str) -> i64 {
    let s = slug.to_ascii_lowercase();
    if s.contains("3.") || s.contains("3-") {
        return 65_536;
    }
    8_192
}

fn claude_supports_assistant_prefill(_slug: &str, family: ModelFamily) -> bool {
    // Our deployment routes ALL Claude models through the LiteLLM proxy which
    // backends to Vertex AI.  Vertex rejects requests that end with role:assistant
    // ("does not support assistant message prefill") for every model variant --
    // not only Opus.  The original non-Opus exception was wrong for our stack.
    //
    // S-014 in wire.rs appends a synthetic user sentinel when this returns false,
    // which is the correct behaviour.  The intermittent error on req_vrtx_ request
    // IDs (session 019f2604, model claude-sonnet-4.6) was caused by Sonnet turns
    // ending with a trailing assistant message slipping past the guard.
    //
    // To re-enable prefill for a direct Anthropic (non-Vertex) path, call
    // ProviderCaps::merge_override at provider init time instead.
    if !matches!(family, ModelFamily::Claude) {
        return false;
    }
    false
}

/// Models that support the extended 1-hour prompt-cache TTL.
///
/// Derived from Apex's `MODELS_SUPPORTING_1H_CACHE` list (sortie-26/27).
/// Slug matching uses substring checks so versioned aliases like
/// `claude-opus-4.7-max` match `"claude-opus-4"`.
const MODELS_SUPPORTING_1H_CACHE: &[&str] = &[
    "claude-opus-4",
    "claude-sonnet-4",
    "claude-haiku-4",
    "claude-haiku-3-5",
    "claude-3-5-haiku",
    "claude-3-5-sonnet",
    "claude-3-7-sonnet",
];

fn claude_supports_1h_cache(slug: &str, family: ModelFamily) -> bool {
    if !matches!(family, ModelFamily::Claude) {
        return false;
    }
    let s = slug.to_ascii_lowercase();
    MODELS_SUPPORTING_1H_CACHE
        .iter()
        .any(|prefix| s.contains(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_defaults_no_assistant_prefill() {
        ProviderCaps::clear_overrides();
        let caps = ProviderCaps::for_model("claude-opus-4.6");
        assert!(!caps.supports_assistant_prefill);
        assert!(caps.supports_prompt_caching);
    }

    // Regression: session 019f2604, model claude-sonnet-4.6, req_vrtx_ error
    // "does not support assistant message prefill".  The old implementation
    // returned true for non-Opus models, skipping the S-014 sentinel.
    #[test]
    fn sonnet_also_no_assistant_prefill() {
        ProviderCaps::clear_overrides();
        let caps = ProviderCaps::for_model("claude-sonnet-4.6");
        assert!(
            !caps.supports_assistant_prefill,
            "Sonnet routes through Vertex AI which rejects prefill -- must be false"
        );
        assert!(caps.supports_prompt_caching);
    }

    #[test]
    fn haiku_also_no_assistant_prefill() {
        ProviderCaps::clear_overrides();
        let caps = ProviderCaps::for_model("claude-haiku-4.5");
        assert!(
            !caps.supports_assistant_prefill,
            "Haiku routes through Vertex AI which rejects prefill -- must be false"
        );
    }
}
