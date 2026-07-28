//! Anthropic model-slug helpers.
//!
//! Pure functions that classify a model slug or compute a Claude-specific
//! parameter. Shared by:
//!
//! * The Messages-wire orchestration that still lives in
//!   `codex-core::client::ModelClientSession::stream_messages_api`
//!   (until Commit 3c+ moves it).
//! * The forthcoming `AnthropicMessagesProvider` that will implement
//!   `codex_model_provider::ModelProvider::effective_wire_api` using
//!   [`is_anthropic_model`] to auto-upgrade non-Anthropic slugs
//!   to the Responses wire.
//!
//! Everything here is stateless, side-effect free, and re-exported at the
//! crate root so `codex-core` can consume them via
//! `codex_provider_anthropic::{is_anthropic_model, anthropic_thinking_param,
//! anthropic_max_output_tokens, anthropic_effort_param}`.

use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;

/// Maps XLI's `ReasoningEffort` to the Anthropic `output_config.effort`
/// wire string.
///
/// The Anthropic Messages API accepts `"low"`, `"medium"`, `"high"`,
/// and `"max"` as effort levels in `output_config: { "effort": "..." }`.
/// This controls how hard the model works on its response, orthogonal
/// to whether thinking is enabled (which is always `"adaptive"` when
/// effort is active).
///
/// Returns `None` for `None` and `Minimal` -- those disable thinking
/// entirely, so no effort level is sent.
pub fn anthropic_effort_param(effort: Option<ReasoningEffortConfig>) -> Option<&'static str> {
    match effort? {
        ReasoningEffortConfig::None | ReasoningEffortConfig::Minimal => None,
        ReasoningEffortConfig::Low => Some("low"),
        ReasoningEffortConfig::Medium => Some("medium"),
        ReasoningEffortConfig::High => Some("high"),
        ReasoningEffortConfig::XHigh => Some("max"),
        ReasoningEffortConfig::Max => Some("max"),
    }
}

/// Returns `true` if the model slug identifies an Anthropic Claude model.
///
/// Matches the substring `claude` (case-insensitive) so that provider-proxied
/// slugs like `anthropic/claude-sonnet-4-6` and Vertex AI variants like
/// `claude-sonnet-4-6@default` are both recognized.
pub fn is_anthropic_model(slug: &str) -> bool {
    let s = slug.to_ascii_lowercase();
    // All current Anthropic model slugs contain "claude".
    // Vertex AI slugs follow patterns like "claude-sonnet-4-6@default".
    s.contains("claude")
}

/// Returns `true` for Anthropic models whose reasoning depth cannot be
/// modulated by the effort knob, so the native `thinking` block is the only
/// lever that reaches the model.
///
/// Opus 4.7+ ignores both `output_config.effort` and a top-level `effort`
/// field: the Vertex AI Anthropic passthrough strips them before the model
/// sees them (verified empirically 2026-06-01 against the LLM proxy). On
/// those models, omitting the `thinking` block makes the model stop thinking
/// entirely. Product intent is "always think" on these models, so the wire
/// must force a `thinking` block regardless of the requested effort.
pub fn anthropic_thinking_always_on(model_slug: &str) -> bool {
    let s = model_slug.to_ascii_lowercase();
    // Slugs appear as `claude-opus-4.7`, `claude-opus-4-7`, and Vertex
    // variants like `claude-opus-4-7@default`.
    s.contains("opus") && (s.contains("4.7") || s.contains("4-7"))
}

/// Builds the `thinking` parameter for the Anthropic Messages API from a
/// reasoning-effort setting and the target model.
///
/// For "always-on" thinking models (see [`anthropic_thinking_always_on`]) this
/// returns `{"type": "adaptive"}` unconditionally, because the effort knob is
/// stripped en route and dropping the block would silence the model's
/// reasoning. For all other models it returns `None` for `None`,
/// `ReasoningEffort::None`, and `ReasoningEffort::Minimal`; otherwise
/// `{"type": "adaptive"}`, which lets the model self-regulate reasoning depth
/// per turn.
pub fn anthropic_thinking_param(
    effort: Option<ReasoningEffortConfig>,
    model_slug: &str,
) -> Option<serde_json::Value> {
    if anthropic_thinking_always_on(model_slug) {
        return Some(serde_json::json!({ "type": "adaptive" }));
    }
    effort.and_then(|e| {
        if matches!(
            e,
            ReasoningEffortConfig::None | ReasoningEffortConfig::Minimal
        ) {
            return None;
        }
        Some(serde_json::json!({ "type": "adaptive" }))
    })
}

/// Returns the Anthropic per-model `max_tokens` output cap in tokens for the
/// given model slug.
///
/// Only applies Claude-specific caps when the normalized slug actually
/// starts with `claude`. Proxy or custom model names that happen to contain
/// `opus` / `haiku` as substrings get the conservative 64K default.
pub fn anthropic_max_output_tokens(slug: &str) -> u32 {
    let normalized = slug.to_lowercase();
    // Only apply Claude-specific caps to actual Claude model slugs.
    // Proxy/custom model names that happen to contain "opus" or "haiku"
    // as substrings get the conservative default.
    if normalized.starts_with("claude") {
        if normalized.contains("opus") {
            return 128_000;
        }
        if normalized.contains("haiku") {
            return 8_192;
        }
        return 64_000; // sonnet and future claude models
    }
    // Non-Claude models on /messages wire (e.g. routed via proxy): conservative default
    64_000
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- anthropic_effort_param --

    #[test]
    fn effort_param_none_when_no_effort() {
        assert_eq!(anthropic_effort_param(None), None);
    }

    #[test]
    fn effort_param_none_for_none_variant() {
        assert_eq!(
            anthropic_effort_param(Some(ReasoningEffortConfig::None)),
            None
        );
    }

    #[test]
    fn effort_param_none_for_minimal() {
        assert_eq!(
            anthropic_effort_param(Some(ReasoningEffortConfig::Minimal)),
            None
        );
    }

    #[test]
    fn effort_param_low() {
        assert_eq!(
            anthropic_effort_param(Some(ReasoningEffortConfig::Low)),
            Some("low")
        );
    }

    #[test]
    fn effort_param_medium() {
        assert_eq!(
            anthropic_effort_param(Some(ReasoningEffortConfig::Medium)),
            Some("medium")
        );
    }

    #[test]
    fn effort_param_high() {
        assert_eq!(
            anthropic_effort_param(Some(ReasoningEffortConfig::High)),
            Some("high")
        );
    }

    #[test]
    fn effort_param_xhigh_maps_to_max() {
        assert_eq!(
            anthropic_effort_param(Some(ReasoningEffortConfig::XHigh)),
            Some("max")
        );
    }

    // -- anthropic_thinking_param --

    #[test]
    fn thinking_param_none_when_no_effort() {
        assert!(anthropic_thinking_param(None, "claude-sonnet-4.6").is_none());
    }

    #[test]
    fn thinking_param_none_for_none_variant() {
        assert!(
            anthropic_thinking_param(Some(ReasoningEffortConfig::None), "claude-sonnet-4.6")
                .is_none()
        );
    }

    #[test]
    fn thinking_param_none_for_minimal() {
        assert!(
            anthropic_thinking_param(Some(ReasoningEffortConfig::Minimal), "claude-sonnet-4.6")
                .is_none()
        );
    }

    #[test]
    fn thinking_param_adaptive_for_low() {
        let v = anthropic_thinking_param(Some(ReasoningEffortConfig::Low), "claude-sonnet-4.6")
            .unwrap();
        assert_eq!(v["type"], "adaptive");
    }

    #[test]
    fn thinking_param_adaptive_for_medium() {
        let v = anthropic_thinking_param(Some(ReasoningEffortConfig::Medium), "claude-sonnet-4.6")
            .unwrap();
        assert_eq!(v["type"], "adaptive");
    }

    #[test]
    fn thinking_param_adaptive_for_high() {
        let v = anthropic_thinking_param(Some(ReasoningEffortConfig::High), "claude-sonnet-4.6")
            .unwrap();
        assert_eq!(v["type"], "adaptive");
    }

    #[test]
    fn thinking_param_adaptive_for_xhigh() {
        let v = anthropic_thinking_param(Some(ReasoningEffortConfig::XHigh), "claude-sonnet-4.6")
            .unwrap();
        assert_eq!(v["type"], "adaptive");
    }

    // -- orthogonality: thinking + effort are independent --

    #[test]
    fn thinking_and_effort_both_active_for_xhigh() {
        let thinking =
            anthropic_thinking_param(Some(ReasoningEffortConfig::XHigh), "claude-sonnet-4.6");
        let effort = anthropic_effort_param(Some(ReasoningEffortConfig::XHigh));
        assert_eq!(thinking.unwrap()["type"], "adaptive");
        assert_eq!(effort, Some("max"));
    }

    #[test]
    fn thinking_and_effort_both_none_for_minimal() {
        let thinking =
            anthropic_thinking_param(Some(ReasoningEffortConfig::Minimal), "claude-sonnet-4.6");
        let effort = anthropic_effort_param(Some(ReasoningEffortConfig::Minimal));
        assert!(thinking.is_none());
        assert!(effort.is_none());
    }

    // -- always-on thinking models (opus-4.7) --

    #[test]
    fn always_on_true_for_opus_47_variants() {
        assert!(anthropic_thinking_always_on("claude-opus-4.7"));
        assert!(anthropic_thinking_always_on("claude-opus-4-7"));
        assert!(anthropic_thinking_always_on("claude-opus-4-7@default"));
    }

    #[test]
    fn always_on_false_for_other_models() {
        assert!(!anthropic_thinking_always_on("claude-sonnet-4.6"));
        assert!(!anthropic_thinking_always_on("claude-opus-4-6"));
        assert!(!anthropic_thinking_always_on("claude-haiku-4.5"));
    }

    #[test]
    fn always_on_forces_thinking_even_for_none() {
        let v = anthropic_thinking_param(None, "claude-opus-4.7").unwrap();
        assert_eq!(v["type"], "adaptive");
    }

    #[test]
    fn always_on_forces_thinking_even_for_minimal() {
        let v = anthropic_thinking_param(Some(ReasoningEffortConfig::Minimal), "claude-opus-4-7")
            .unwrap();
        assert_eq!(v["type"], "adaptive");
    }

    // -- anthropic_max_output_tokens --

    #[test]
    fn max_output_tokens_opus_128k() {
        assert_eq!(anthropic_max_output_tokens("claude-opus-4-6"), 128_000);
    }

    #[test]
    fn max_output_tokens_sonnet_64k() {
        assert_eq!(anthropic_max_output_tokens("claude-sonnet-4-6"), 64_000);
    }

    #[test]
    fn max_output_tokens_haiku_8k() {
        assert_eq!(anthropic_max_output_tokens("claude-haiku-3-5"), 8_192);
    }

    #[test]
    fn max_output_tokens_non_claude_default() {
        assert_eq!(anthropic_max_output_tokens("gpt-5.3-codex"), 64_000);
    }

    // -- is_anthropic_model --

    #[test]
    fn recognizes_claude_slugs() {
        assert!(is_anthropic_model("claude-sonnet-4-6"));
        assert!(is_anthropic_model("claude-opus-4-6"));
        assert!(is_anthropic_model("anthropic/claude-sonnet-4-6"));
        assert!(is_anthropic_model("claude-sonnet-4-6@default"));
    }

    #[test]
    fn rejects_non_claude_slugs() {
        assert!(!is_anthropic_model("gpt-5.3-codex"));
        assert!(!is_anthropic_model("o3-mini"));
    }
}
