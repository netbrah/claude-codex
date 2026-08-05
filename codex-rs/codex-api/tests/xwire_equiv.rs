//! S-XWIRE-EQUIV — golden cross-wire usage equivalence fixtures.
//!
//! Hermetic CI gate: each wire's native usage shape projects to the same
//! canonical [`TokenUsage`] (cache-inclusive `input_tokens`, split preserved in
//! `cached_input_tokens` / `cache_creation_input_tokens`). Provenance per fixture
//! is in `tests/fixtures/xwire_equiv/*.json` and the S-USAGE-TYPED matrix.

use codex_api::RawUsage;
use codex_api::normalize_token_usage;
use codex_protocol::protocol::TokenUsage;
use serde::Deserialize;
use std::path::Path;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/xwire_equiv");

/// Canonical projection for the shared logical usage across all four wires:
/// 100 non-cached + 50 cache-read prompt; 42 output; 15 reasoning.
/// (`cache_creation` is Anthropic-only — omitted here so all wires can express
/// the same logical counts in native shape.)
fn canonical_usage() -> TokenUsage {
    TokenUsage {
        input_tokens: 150,
        cached_input_tokens: 50,
        cache_creation_input_tokens: 0,
        output_tokens: 42,
        reasoning_output_tokens: 15,
        total_tokens: 192,
        ..Default::default()
    }
}

fn load_fixture(name: &str) -> String {
    std::fs::read_to_string(Path::new(FIXTURES).join(name))
        .unwrap_or_else(|e| panic!("load fixture {name}: {e}"))
}

#[derive(Debug, Deserialize)]
struct AnthropicUsageFixture {
    input_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    output_tokens: i64,
    output_tokens_details: Option<OutputTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct OutputTokensDetails {
    thinking_tokens: Option<i64>,
}

fn project_anthropic(json: &str) -> TokenUsage {
    let f: AnthropicUsageFixture = serde_json::from_str(json).expect("anthropic usage json");
    let reasoning = f
        .output_tokens_details
        .and_then(|d| d.thinking_tokens)
        .unwrap_or(0);
    normalize_token_usage(RawUsage::cache_exclusive_prompt(
        f.input_tokens,
        f.cache_read_input_tokens,
        f.cache_creation_input_tokens,
        f.output_tokens,
        reasoning,
    ))
}

#[derive(Debug, Deserialize)]
struct ResponsesUsageFixture {
    input_tokens: i64,
    input_tokens_details: Option<CachedTokensDetails>,
    output_tokens: i64,
    output_tokens_details: Option<ReasoningTokensDetails>,
    total_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct CachedTokensDetails {
    cached_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct ReasoningTokensDetails {
    reasoning_tokens: i64,
}

fn project_responses(json: &str) -> TokenUsage {
    let f: ResponsesUsageFixture = serde_json::from_str(json).expect("responses usage json");
    normalize_token_usage(RawUsage::cache_inclusive_prompt(
        f.input_tokens,
        f.input_tokens_details.map(|d| d.cached_tokens).unwrap_or(0),
        0,
        f.output_tokens,
        f.output_tokens_details
            .map(|d| d.reasoning_tokens)
            .unwrap_or(0),
        f.total_tokens,
    ))
}

#[derive(Debug, Deserialize)]
struct GeminiUsageFixture {
    prompt_token_count: i64,
    cached_content_token_count: i64,
    candidates_token_count: i64,
    thoughts_token_count: i64,
    total_token_count: i64,
}

fn project_gemini(json: &str) -> TokenUsage {
    let f: GeminiUsageFixture = serde_json::from_str(json).expect("gemini usage json");
    normalize_token_usage(RawUsage::cache_inclusive_prompt(
        f.prompt_token_count,
        f.cached_content_token_count,
        0,
        f.candidates_token_count,
        f.thoughts_token_count,
        f.total_token_count,
    ))
}

#[derive(Debug, Deserialize)]
struct CopilotUsageFixture {
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    prompt_tokens_details: Option<CachedTokensDetails>,
    completion_tokens_details: Option<ReasoningTokensDetails>,
}

fn project_copilot(json: &str) -> TokenUsage {
    let f: CopilotUsageFixture = serde_json::from_str(json).expect("copilot usage json");
    normalize_token_usage(RawUsage::cache_inclusive_prompt(
        f.prompt_tokens,
        f.prompt_tokens_details.map(|d| d.cached_tokens).unwrap_or(0),
        0,
        f.completion_tokens,
        f.completion_tokens_details
            .map(|d| d.reasoning_tokens)
            .unwrap_or(0),
        f.total_tokens,
    ))
}

#[test]
fn golden_anthropic_usage_matches_canonical() {
    let projected = project_anthropic(&load_fixture("usage_anthropic.json"));
    assert_eq!(projected, canonical_usage());
}

#[test]
fn golden_responses_usage_matches_canonical() {
    let projected = project_responses(&load_fixture("usage_responses.json"));
    assert_eq!(projected, canonical_usage());
}

#[test]
fn golden_gemini_usage_matches_canonical() {
    let projected = project_gemini(&load_fixture("usage_gemini.json"));
    assert_eq!(projected, canonical_usage());
}

#[test]
fn golden_copilot_usage_matches_canonical() {
    let projected = project_copilot(&load_fixture("usage_copilot.json"));
    assert_eq!(projected, canonical_usage());
}

#[test]
fn cross_wire_usage_equivalence_all_four_wires() {
    let anthropic = project_anthropic(&load_fixture("usage_anthropic.json"));
    let responses = project_responses(&load_fixture("usage_responses.json"));
    let gemini = project_gemini(&load_fixture("usage_gemini.json"));
    let copilot = project_copilot(&load_fixture("usage_copilot.json"));

    assert_eq!(anthropic, responses);
    assert_eq!(responses, gemini);
    assert_eq!(gemini, copilot);
    assert_eq!(copilot, canonical_usage());
}

/// Proves the net has teeth: treating Anthropic `input_tokens` as cache-inclusive
/// (the pre-S-USAGE-NEWTYPE F1 bug) diverges from every other wire + canonical.
#[test]
fn anthropic_cache_exclusive_without_normalization_fails_equiv() {
    let f: AnthropicUsageFixture =
        serde_json::from_str(&load_fixture("usage_anthropic.json")).expect("anthropic json");
    let naive = TokenUsage {
        input_tokens: f.input_tokens,
        cached_input_tokens: f.cache_read_input_tokens,
        cache_creation_input_tokens: f.cache_creation_input_tokens,
        output_tokens: f.output_tokens,
        reasoning_output_tokens: f
            .output_tokens_details
            .as_ref()
            .and_then(|d| d.thinking_tokens)
            .unwrap_or(0),
        total_tokens: f.input_tokens + f.output_tokens,
        ..Default::default()
    };
    assert_ne!(naive, canonical_usage());
    assert_ne!(naive, project_responses(&load_fixture("usage_responses.json")));
}

/// Anthropic-only cache-creation band (not expressible on /responses, Gemini, Copilot).
#[test]
fn anthropic_cache_creation_projects_separately_from_cross_wire_set() {
    let usage = normalize_token_usage(RawUsage::cache_exclusive_prompt(
        100, 50, 25, 42, 15,
    ));
    assert_eq!(usage.input_tokens, 175);
    assert_eq!(usage.cache_creation_input_tokens, 25);
    assert_ne!(usage, canonical_usage());
}

// ─── Error projection matrix (S-ERROR-TYPED folded) ───

#[derive(Debug, PartialEq, Eq)]
enum HarnessErrorClass {
    ServerOverloaded,
    RateLimit,
    UsageLimitReached,
    Generic,
}

fn classify_anthropic_error_type(error_type: &str) -> HarnessErrorClass {
    match error_type {
        "overloaded_error" => HarnessErrorClass::ServerOverloaded,
        "rate_limit_error" => HarnessErrorClass::RateLimit,
        _ => HarnessErrorClass::Generic,
    }
}

fn classify_responses_error_type(error_type: &str) -> HarnessErrorClass {
    match error_type {
        "server_overloaded" => HarnessErrorClass::ServerOverloaded,
        "rate_limit_exceeded" => HarnessErrorClass::RateLimit,
        "usage_limit_reached" | "usage_not_included" => HarnessErrorClass::UsageLimitReached,
        _ => HarnessErrorClass::Generic,
    }
}

#[test]
fn error_overloaded_maps_to_same_harness_class_across_wires() {
    assert_eq!(
        classify_anthropic_error_type("overloaded_error"),
        HarnessErrorClass::ServerOverloaded
    );
    assert_eq!(
        classify_responses_error_type("server_overloaded"),
        HarnessErrorClass::ServerOverloaded
    );
}

#[test]
fn error_rate_limit_maps_to_same_harness_class_across_wires() {
    assert_eq!(
        classify_anthropic_error_type("rate_limit_error"),
        HarnessErrorClass::RateLimit
    );
    assert_eq!(
        classify_responses_error_type("rate_limit_exceeded"),
        HarnessErrorClass::RateLimit
    );
}
