//! Single normalization boundary for wire-native usage → protocol [`TokenUsage`].
//!
//! Every wire parser builds a [`RawUsage`] from its native shape and calls
//! [`normalize_token_usage`]. This enforces the harness contract:
//! `input_tokens` is **cache-inclusive** (cached is a subset of input), and
//! `non_cached_input() = input_tokens - cached_input_tokens` is correct on all
//! wires (S-USAGE-NEWTYPE / S-USAGE-TYPED F1).

use codex_protocol::protocol::TokenUsage;

/// Wire-native usage counts **before** the cache-inclusive invariant is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawUsage {
    /// Prompt/input tokens as the wire reports them (may be cache-exclusive).
    pub api_input: i64,
    /// Cache read (hit) tokens.
    pub cache_read: i64,
    /// Cache write (creation) tokens — Anthropic-only today.
    pub cache_creation: i64,
    pub output: i64,
    pub reasoning_output: i64,
    /// `true` for native `/responses`, Copilot chat, Gemini (prompt already
    /// cache-inclusive). `false` for Anthropic `/messages` (additive cache fields).
    pub input_is_cache_inclusive: bool,
    /// Server-reported total when trusted verbatim. When `None`, total is
    /// `normalized_input + output` (Anthropic path).
    pub api_total: Option<i64>,
    /// Anthropic `usage.server_tool_use.web_search_requests` (0 on other wires).
    pub web_search_requests: i64,
    /// Anthropic `usage.server_tool_use.web_fetch_requests` (0 on other wires).
    pub web_fetch_requests: i64,
}

impl RawUsage {
    /// Native `/responses`, Copilot chat, Gemini: `api_input` already includes cache.
    pub fn cache_inclusive_prompt(
        api_input: i64,
        cache_read: i64,
        cache_creation: i64,
        output: i64,
        reasoning_output: i64,
        api_total: i64,
    ) -> Self {
        Self {
            api_input,
            cache_read,
            cache_creation,
            output,
            reasoning_output,
            input_is_cache_inclusive: true,
            api_total: Some(api_total),
            web_search_requests: 0,
            web_fetch_requests: 0,
        }
    }

    /// Anthropic `/messages`: `api_input` is non-cached prompt; cache fields are additive.
    pub fn cache_exclusive_prompt(
        api_input: i64,
        cache_read: i64,
        cache_creation: i64,
        output: i64,
        reasoning_output: i64,
    ) -> Self {
        Self {
            api_input,
            cache_read,
            cache_creation,
            output,
            reasoning_output,
            input_is_cache_inclusive: false,
            api_total: None,
            web_search_requests: 0,
            web_fetch_requests: 0,
        }
    }
}

/// Project wire-native usage onto the protocol [`TokenUsage`].
pub fn normalize_token_usage(raw: RawUsage) -> TokenUsage {
    let api_input = raw.api_input.max(0);
    let cache_read = raw.cache_read.max(0);
    let cache_creation = raw.cache_creation.max(0);
    let output = raw.output.max(0);
    let reasoning_output = raw.reasoning_output.max(0);

    let input_tokens = if raw.input_is_cache_inclusive {
        api_input
    } else {
        api_input + cache_read + cache_creation
    };

    let total_tokens = raw
        .api_total
        .unwrap_or(input_tokens + output)
        .max(0);

    TokenUsage {
        input_tokens,
        cached_input_tokens: cache_read,
        cache_creation_input_tokens: cache_creation,
        output_tokens: output,
        reasoning_output_tokens: reasoning_output,
        total_tokens,
        web_search_requests: raw.web_search_requests.max(0),
        web_fetch_requests: raw.web_fetch_requests.max(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_cache_exclusive_folds_cache_into_input() {
        let usage = normalize_token_usage(RawUsage::cache_exclusive_prompt(
            100, 50, 25, 42, 0,
        ));
        assert_eq!(usage.input_tokens, 175);
        assert_eq!(usage.cached_input_tokens, 50);
        assert_eq!(usage.cache_creation_input_tokens, 25);
        assert_eq!(usage.output_tokens, 42);
        assert_eq!(usage.reasoning_output_tokens, 0);
        assert_eq!(usage.total_tokens, 217);
    }

    #[test]
    fn anthropic_cache_read_included_in_total() {
        let usage = normalize_token_usage(RawUsage::cache_exclusive_prompt(
            100, 50, 25, 10, 0,
        ));
        assert_eq!(usage.input_tokens, 175);
        assert_eq!(usage.total_tokens, 185);
    }

    #[test]
    fn native_responses_trusts_api_total() {
        let usage = normalize_token_usage(RawUsage::cache_inclusive_prompt(
            120, 30, 0, 45, 15, 165,
        ));
        assert_eq!(usage.input_tokens, 120);
        assert_eq!(usage.cached_input_tokens, 30);
        assert_eq!(usage.output_tokens, 45);
        assert_eq!(usage.reasoning_output_tokens, 15);
        assert_eq!(usage.total_tokens, 165);
    }

    #[test]
    fn gemini_projection() {
        let usage = normalize_token_usage(RawUsage::cache_inclusive_prompt(
            5, 2, 0, 3, 1, 8,
        ));
        assert_eq!(usage.input_tokens, 5);
        assert_eq!(usage.cached_input_tokens, 2);
        assert_eq!(usage.output_tokens, 3);
        assert_eq!(usage.reasoning_output_tokens, 1);
        assert_eq!(usage.total_tokens, 8);
    }

    #[test]
    fn copilot_projection() {
        let usage = normalize_token_usage(RawUsage::cache_inclusive_prompt(
            100, 20, 0, 50, 10, 150,
        ));
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.cached_input_tokens, 20);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.reasoning_output_tokens, 10);
        assert_eq!(usage.total_tokens, 150);
        assert_eq!(usage.cache_creation_input_tokens, 0);
    }

    #[test]
    fn anthropic_thinking_tokens_project_to_reasoning_output() {
        let usage = normalize_token_usage(RawUsage::cache_exclusive_prompt(
            100, 0, 0, 200, 80,
        ));
        assert_eq!(usage.output_tokens, 200);
        assert_eq!(usage.reasoning_output_tokens, 80);
        assert_eq!(usage.total_tokens, 300);
    }

    #[test]
    fn anthropic_server_tool_use_projects_to_token_usage() {
        let usage = normalize_token_usage(RawUsage {
            web_search_requests: 2,
            web_fetch_requests: 1,
            ..RawUsage::cache_exclusive_prompt(100, 0, 0, 20, 0)
        });
        assert_eq!(usage.web_search_requests, 2);
        assert_eq!(usage.web_fetch_requests, 1);
    }

    #[test]
    fn negative_fields_clamped_to_zero() {
        let usage = normalize_token_usage(RawUsage::cache_exclusive_prompt(
            -1, -2, -3, -4, -5,
        ));
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
    }
}
