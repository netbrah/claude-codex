//! Static route table mapping model slug → preferred Copilot wire.
//!
//! See OPROD 04 §2.2 (`netbrah/copilot-codex/docs/integration/04-oprod-3wire-router.md`)
//! for the empirically confirmed wire matrix. This table encodes the
//! "prefer native over Chat" rule:
//!
//! - `claude-*` → Messages   (strictly better than Chat; native `tool_result`,
//!                             prompt caching)
//! - `gpt-5.x` → Responses   (strictly better than Chat; reasoning tokens,
//!                             native function-call streaming)
//! - everything else → `ChatCompletions` (legacy fallback; gpt-4o/4.1, o1, etc.)
//!
//! Per rubber-duck finding #4, we use an **exact allowlist** for Claude and
//! GPT-5, not a substring heuristic. Any slug not on the allowlist falls
//! through to Chat with a `warn!` — so a future model that Copilot only
//! exposes on Chat does not silently 404 against `/v1/messages`.

/// Which of Copilot's three wires serves a given model slug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopilotWire {
    /// Route to `/v1/messages` (native Anthropic Messages).
    Messages,
    /// Route to `/responses` (native `OpenAI` Responses).
    Responses,
    /// Route to `/chat/completions` (legacy, universal).
    ChatCompletions,
}

/// Exact Claude slugs Copilot exposes on `/v1/messages` today (OPROD §2.2).
const CLAUDE_MESSAGES_SLUGS: &[&str] = &[
    "claude-sonnet-4.5",
    "claude-sonnet-4.6",
    "claude-opus-4.5",
    "claude-opus-4.6",
    "claude-opus-4.7",
    "claude-opus-4.8",
    "claude-haiku-4.5",
];

/// Exact GPT-5 family slugs Copilot exposes on `/responses` today (OPROD §2.2).
const GPT5_RESPONSES_SLUGS: &[&str] = &[
    "gpt-5.2",
    "gpt-5.2-codex",
    "gpt-5.3-codex",
    "gpt-5.4",
    "gpt-5-mini",
];

/// Map a model slug to its preferred Copilot wire.
///
/// Unknown slugs fall through to `ChatCompletions` with a `tracing::warn!` —
/// this is the safe default (Chat is the only wire guaranteed to accept every
/// model). The warning surfaces in operator logs so we can add the slug to
/// the allowlist in a follow-on.
#[must_use]
pub fn route_for_model(slug: &str) -> CopilotWire {
    if CLAUDE_MESSAGES_SLUGS.contains(&slug) {
        return CopilotWire::Messages;
    }
    if GPT5_RESPONSES_SLUGS.contains(&slug) {
        return CopilotWire::Responses;
    }
    tracing::warn!(
        model = slug,
        "copilot route: unknown slug, falling through to /chat/completions — \
         add to CLAUDE_MESSAGES_SLUGS or GPT5_RESPONSES_SLUGS if native wire is supported"
    );
    CopilotWire::ChatCompletions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_opus_47_routes_to_messages() {
        assert_eq!(route_for_model("claude-opus-4.7"), CopilotWire::Messages);
    }

    #[test]
    fn claude_opus_48_routes_to_messages() {
        // Regression guard: claude-opus-4.8 must route to Messages, not
        // fall through to /chat/completions (which 400s on Anthropic slugs).
        // CAPI /models verified 2026-05-28: opus-4.8 served on /v1/messages,
        // reasoning_effort=["medium"] (server-side locked, same as 4.7).
        assert_eq!(route_for_model("claude-opus-4.8"), CopilotWire::Messages);
    }

    #[test]
    fn gpt_54_routes_to_responses() {
        assert_eq!(route_for_model("gpt-5.4"), CopilotWire::Responses);
    }

    #[test]
    fn gpt_4o_routes_to_chat() {
        assert_eq!(route_for_model("gpt-4o"), CopilotWire::ChatCompletions);
    }

    #[test]
    fn unknown_slug_routes_to_chat() {
        assert_eq!(
            route_for_model("claude-future-model-9.9"),
            CopilotWire::ChatCompletions
        );
    }

    #[test]
    fn gpt_mini_routes_to_responses() {
        assert_eq!(route_for_model("gpt-5-mini"), CopilotWire::Responses);
    }

    #[test]
    fn gpt_5_3_codex_routes_to_responses() {
        assert_eq!(route_for_model("gpt-5.3-codex"), CopilotWire::Responses);
    }

    #[test]
    fn claude_haiku_routes_to_messages() {
        assert_eq!(route_for_model("claude-haiku-4.5"), CopilotWire::Messages);
    }

    #[test]
    fn claude_sonnet_46_routes_to_messages() {
        assert_eq!(route_for_model("claude-sonnet-4.6"), CopilotWire::Messages);
    }

    #[test]
    fn empty_slug_falls_through_to_chat() {
        assert_eq!(route_for_model(""), CopilotWire::ChatCompletions);
    }

    #[test]
    fn o1_legacy_falls_through_to_chat() {
        assert_eq!(route_for_model("o1-preview"), CopilotWire::ChatCompletions);
    }

    #[test]
    fn gpt_41_legacy_falls_through_to_chat() {
        assert_eq!(route_for_model("gpt-4.1"), CopilotWire::ChatCompletions);
    }

    #[test]
    fn case_mismatch_falls_through_to_chat() {
        // Exact-match allowlist by design (rubber-duck finding #4): a caller
        // who uppercases the slug gets the safe fallback, not a 404.
        assert_eq!(
            route_for_model("CLAUDE-OPUS-4.7"),
            CopilotWire::ChatCompletions
        );
    }
}
