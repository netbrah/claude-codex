use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// XLI extension over upstream `TokenUsage`: carries Anthropic-only counters on a
/// flat serde shape (no nested `inner` field in JSON).
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq, JsonSchema, TS)]
pub struct WireTokenUsage {
    #[ts(type = "number")]
    pub input_tokens: i64,
    #[ts(type = "number")]
    pub cached_input_tokens: i64,
    #[ts(type = "number")]
    pub cache_creation_input_tokens: i64,
    #[ts(type = "number")]
    pub output_tokens: i64,
    #[ts(type = "number")]
    pub reasoning_output_tokens: i64,
    #[ts(type = "number")]
    pub total_tokens: i64,
    /// Anthropic `usage.server_tool_use.web_search_requests` (billing counter).
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    #[ts(type = "number")]
    pub web_search_requests: i64,
    /// Anthropic `usage.server_tool_use.web_fetch_requests` (billing counter).
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    #[ts(type = "number")]
    pub web_fetch_requests: i64,
}

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

// Includes prompts, tools and space to call compact.
const BASELINE_TOKENS: i64 = 12000;

impl WireTokenUsage {
    pub fn is_zero(&self) -> bool {
        self.total_tokens == 0
    }

    pub fn cached_input(&self) -> i64 {
        self.cached_input_tokens.max(0)
    }

    pub fn non_cached_input(&self) -> i64 {
        (self.input_tokens - self.cached_input()).max(0)
    }

    pub fn blended_total(&self) -> i64 {
        (self.non_cached_input() + self.output_tokens.max(0)).max(0)
    }

    pub fn tokens_in_context_window(&self) -> i64 {
        self.total_tokens
    }

    pub fn percent_of_context_window_remaining(&self, context_window: i64) -> i64 {
        if context_window <= BASELINE_TOKENS {
            return 0;
        }

        let effective_window = context_window - BASELINE_TOKENS;
        let used = (self.tokens_in_context_window() - BASELINE_TOKENS).max(0);
        let remaining = (effective_window - used).max(0);
        ((remaining as f64 / effective_window as f64) * 100.0)
            .clamp(0.0, 100.0)
            .round() as i64
    }

    pub fn add_assign(&mut self, other: &WireTokenUsage) {
        self.input_tokens += other.input_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_output_tokens += other.reasoning_output_tokens;
        self.total_tokens += other.total_tokens;
        self.web_search_requests += other.web_search_requests;
        self.web_fetch_requests += other.web_fetch_requests;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn round_trip_serializes_flat_with_anthropic_fields() {
        let usage = WireTokenUsage {
            input_tokens: 10,
            cached_input_tokens: 2,
            cache_creation_input_tokens: 3,
            output_tokens: 5,
            reasoning_output_tokens: 1,
            total_tokens: 15,
            web_search_requests: 1,
            web_fetch_requests: 2,
        };
        let json = serde_json::to_value(&usage).expect("serialize");
        assert_eq!(json["input_tokens"], 10);
        assert_eq!(json["cache_creation_input_tokens"], 3);
        assert_eq!(json["web_search_requests"], 1);
        assert_eq!(json["web_fetch_requests"], 2);
        let round_trip: WireTokenUsage = serde_json::from_value(json).expect("deserialize");
        assert_eq!(usage, round_trip);
    }

    #[test]
    fn zero_anthropic_fields_skip_serialization() {
        let usage = WireTokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            ..Default::default()
        };
        let json = serde_json::to_value(&usage).expect("serialize");
        assert!(json.get("web_search_requests").is_none());
        assert!(json.get("web_fetch_requests").is_none());
        assert_eq!(json["cache_creation_input_tokens"], 0);
    }
}
