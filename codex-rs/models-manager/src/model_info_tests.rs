use super::*;
use crate::ModelsManagerConfig;
use crate::ProviderCaps;
use pretty_assertions::assert_eq;

#[test]
fn reasoning_summaries_override_true_enables_support() {
    let model = model_info_from_slug("unknown-model");
    let config = ModelsManagerConfig {
        model_supports_reasoning_summaries: Some(true),
        ..Default::default()
    };

    let updated = with_config_overrides(model.clone(), &config);
    let mut expected = model;
    expected.supports_reasoning_summaries = true;

    assert_eq!(updated, expected);
}

#[test]
fn reasoning_summaries_override_false_does_not_disable_support() {
    let mut model = model_info_from_slug("unknown-model");
    model.supports_reasoning_summaries = true;
    let config = ModelsManagerConfig {
        model_supports_reasoning_summaries: Some(false),
        ..Default::default()
    };

    let updated = with_config_overrides(model.clone(), &config);

    assert_eq!(updated, model);
}

#[test]
fn reasoning_summaries_override_false_is_noop_when_model_is_false() {
    let model = model_info_from_slug("unknown-model");
    let config = ModelsManagerConfig {
        model_supports_reasoning_summaries: Some(false),
        ..Default::default()
    };

    let updated = with_config_overrides(model.clone(), &config);

    assert_eq!(updated, model);
}

#[test]
fn model_context_window_override_clamps_to_max_context_window() {
    let mut model = model_info_from_slug("unknown-model");
    model.context_window = Some(273_000);
    model.max_context_window = Some(400_000);
    let config = ModelsManagerConfig {
        model_context_window: Some(500_000),
        ..Default::default()
    };

    let updated = with_config_overrides(model.clone(), &config);
    let mut expected = model;
    expected.context_window = Some(400_000);

    assert_eq!(updated, expected);
}

#[test]
fn model_context_window_uses_model_value_without_override() {
    let mut model = model_info_from_slug("unknown-model");
    model.context_window = Some(273_000);
    model.max_context_window = Some(400_000);
    let config = ModelsManagerConfig::default();

    let updated = with_config_overrides(model.clone(), &config);

    assert_eq!(updated, model);
}

/// Regression: ALL Claude models must return `supports_assistant_prefill = false`
/// in our deployment.  Our LiteLLM proxy routes every Claude model through
/// Vertex AI, and Vertex rejects requests that end with role:assistant for
/// every model variant, not just Opus.
///
/// LiteLLM `/v1/model/info` reports sonnet/haiku as prefill-capable, but that
/// reflects the *direct Anthropic API* capability.  Our Vertex-backed proxy is
/// stricter.  Returning `true` for Sonnet was the source of the "does not
/// support assistant message prefill" error in session 019f2604
/// (model claude-sonnet-4.6, req_vrtx_ request ID, 2026-07-02).
///
/// S-014 in wire.rs appends a synthetic user sentinel when this returns false.
/// The sentinel is always safe to append; prefill = true is only safe on
/// direct Anthropic native, which we do not use.
#[test]
fn claude_all_models_no_assistant_prefill() {
    let cases: &[(&str, bool)] = &[
        // Opus -- false (pre-existing correct behaviour)
        ("claude-opus-4.5", false),
        ("claude-opus-4.6", false),
        ("claude-opus-4.7", false),
        ("claude-opus-5.0", false), // future-proofing
        ("claude-opus-4-6", false), // dash form
        // Sonnet -- now false (regression fix, session 019f2604)
        ("claude-sonnet-4.5", false),
        ("claude-sonnet-4.6", false),
        // Haiku -- now false (same reasoning as Sonnet)
        ("claude-haiku-4.5", false),
        // Non-Claude -- still false
        ("gemini-3.1-pro-preview", false),
        ("gpt-5.4", false),
    ];
    for (slug, expected) in cases {
        let caps = ProviderCaps::for_model(slug);
        assert_eq!(
            caps.supports_assistant_prefill, *expected,
            "slug `{slug}`: expected supports_assistant_prefill={expected}, got {} — \
             this regression silently breaks the S-014 trailing-assistant guard",
            caps.supports_assistant_prefill
        );
    }
}

/// Audit: every `ModelInfo` boolean capability that's keyed off
/// `ModelFamily` must reflect *actual sub-family-aware reality*.
/// "Family-wide boolean for a sub-family-varying capability" is the
/// bug class that silently 400'd opus-4.6 on parallel spawn_agent
/// runs (commit 40756de0fa). This test pins the LiteLLM
/// `/v1/model/info` truth captured 2026-05-07 against the static
/// fallback so a future "tighten the family arm" change can't
/// regress without the test failing.
///
/// Source-of-truth values are the maintainer's manual transcription
/// of the proxy catalogue. Refresh by re-running the audit script
/// in `cli-ops/sortie-board/xli-v3/audit-family-booleans.md`.
#[test]
fn family_keyed_booleans_match_proxy_truth() {
    // (slug, vision, prompt_caching, prefill)
    let cases: &[(&str, bool, bool, bool)] = &[
        // Claude
        ("claude-opus-4.6", true, true, false),
        ("claude-opus-4.5", true, true, false),
        // Sonnet/Haiku: prefill=false because Vertex AI rejects prefill for all Claude models
        ("claude-sonnet-4.6", true, true, false),
        ("claude-sonnet-4.5", true, true, false),
        ("claude-haiku-4.5", true, true, false),
        // Gemini — prefill is N/A on Gemini wire; we set false.
        ("gemini-3.1-pro-preview", true, true, false),
        ("gemini-3-flash-preview", true, true, false),
        ("gemini-3.1-flash-lite-preview", true, true, false),
        // GPT-5 — prefill is N/A on /responses wire; we set false.
        ("gpt-5.3-codex", true, true, false),
        ("gpt-5.4", true, true, false),
        ("gpt-5.5", true, true, false),
    ];
    for (slug, vision, caching, prefill) in cases {
        let info = model_info_from_slug(slug);
        let caps = ProviderCaps::for_model(slug);
        let actual_vision = info
            .input_modalities
            .contains(&codex_protocol::openai_models::InputModality::Image);
        assert_eq!(
            actual_vision, *vision,
            "slug `{slug}`: vision modality mismatch (expected {vision}, got {actual_vision})"
        );
        assert_eq!(
            caps.supports_prompt_caching, *caching,
            "slug `{slug}`: supports_prompt_caching mismatch (expected {caching}, \
             got {})",
            caps.supports_prompt_caching
        );
        assert_eq!(
            caps.supports_assistant_prefill, *prefill,
            "slug `{slug}`: supports_assistant_prefill mismatch (expected {prefill}, \
             got {})",
            caps.supports_assistant_prefill
        );
    }
}
