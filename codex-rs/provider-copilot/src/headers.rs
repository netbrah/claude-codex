//! Wire-agnostic HTTP headers stamped on every Copilot LLM-plane request.
//!
//! These four headers must ride every request to the enterprise Copilot
//! endpoint across all three sub-wires (Messages, Responses, legacy
//! chat-completions). They are stable per-integration constants, so the
//! function takes no state. Per-wire extras — e.g. `anthropic-beta` on
//! the Messages path — are stamped by the dispatch arms separately.
//!
//! The source of truth for the values is `codex-copilot-adapter`; this
//! module just assembles them into a `http::HeaderMap`.

use codex_copilot_adapter::COPILOT_INTEGRATION_ID;
use codex_copilot_adapter::EDITOR_VERSION;
use codex_copilot_adapter::GITHUB_API_VERSION;
use codex_copilot_adapter::X_GITHUB_API_VERSION;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;

/// Stamps Copilot's always-on transport headers onto `headers`.
///
/// Headers set:
///
/// * `copilot-integration-id` — the integration slug registered with
///   GitHub.
/// * `editor-version` — pinned to the value `codex-copilot-adapter`
///   uses for the `/chat/completions` wire so the enterprise proxy
///   sees a single consistent editor across all three Copilot wires.
/// * `x-initiator` — always `"agent"` (Codex is the agent; the human
///   is one layer up).
/// * `x-github-api-version` — pinned to vscode-copilot-chat's value
///   (`2025-05-01`) on every Copilot LLM-plane request. Covers the
///   native `/v1/messages` and `/responses` wires that bypass
///   `codex_copilot_adapter::wire::build_headers` (which is the
///   chat-completions path's own stamp site).
pub fn stamp_copilot_shared_headers(headers: &mut HeaderMap) {
    headers.insert(
        HeaderName::from_static("copilot-integration-id"),
        HeaderValue::from_static(COPILOT_INTEGRATION_ID),
    );
    headers.insert(
        HeaderName::from_static("editor-version"),
        HeaderValue::from_static(EDITOR_VERSION),
    );
    headers.insert(
        HeaderName::from_static("x-initiator"),
        HeaderValue::from_static("agent"),
    );
    headers.insert(
        HeaderName::from_static(X_GITHUB_API_VERSION),
        HeaderValue::from_static(GITHUB_API_VERSION),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_github_api_version_override() {
        let mut headers = HeaderMap::new();
        stamp_copilot_shared_headers(&mut headers);

        let actual = headers
            .get(X_GITHUB_API_VERSION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert_eq!(
            actual, GITHUB_API_VERSION,
            "stamp_copilot_shared_headers must stamp x-github-api-version — 
             both /v1/messages and /responses dispatch rely on this; 
             /chat/completions has its own stamp in copilot-adapter",
        );

        // Single-valued — guard against an append-instead-of-overwrite bug
        // that would let upstream pick either value and leak drift.
        let count = headers.get_all(X_GITHUB_API_VERSION).iter().count();
        assert_eq!(
            count, 1,
            "x-github-api-version must be single-valued (found {count})",
        );
    }

    #[test]
    fn stamps_all_four_shared_headers() {
        let mut headers = HeaderMap::new();
        stamp_copilot_shared_headers(&mut headers);
        assert!(headers.contains_key("copilot-integration-id"));
        assert!(headers.contains_key("editor-version"));
        assert!(headers.contains_key("x-initiator"));
        assert!(headers.contains_key(X_GITHUB_API_VERSION));
    }

    #[test]
    fn x_initiator_is_agent() {
        let mut headers = HeaderMap::new();
        stamp_copilot_shared_headers(&mut headers);
        assert_eq!(
            headers
                .get("x-initiator")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default(),
            "agent",
        );
    }
}
