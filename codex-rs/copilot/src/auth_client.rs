//! Reqwest client used for Copilot CAPI bearer minting and refresh.
//!
//! The native `WireApi::Copilot` dispatch path in `codex-rs/core/src/client.rs`
//! lazily initializes a session-scoped [`crate::ctx::CopilotCtx`] on the first
//! Copilot turn. That init step performs a CAPI mint against the GitHub
//! token endpoint. Without a read timeout on the underlying reqwest client,
//! a stalled proxy or upstream connection would black-hole the entire turn
//! since the mint future never wakes up.
//!
//! The chat fallback adapter already hardens this path (see
//! `crate::adapter::ensure_session` — 90s `read_timeout`). This module
//! exposes the same hardening for the native `/v1/messages` and `/responses`
//! paths so all three Copilot sub-wires fail fast on a black-hole.

/// Builds the reqwest client used for Copilot CAPI bearer minting and refresh.
///
/// Mirrors the default client behavior plus a 90s `read_timeout` so a stalled
/// proxy/upstream surfaces a fast error instead of black-holing the turn.
#[must_use]
pub fn build_copilot_auth_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .read_timeout(std::time::Duration::from_secs(90))
        .build()
        .expect("copilot auth http client builder")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: the builder must not panic and must produce a usable
    /// client. This is the minimum bar — proves the `read_timeout` value is
    /// accepted by the installed reqwest version and the default TLS backend
    /// links cleanly.
    #[test]
    fn build_copilot_auth_http_client_constructs() {
        let _client: reqwest::Client = build_copilot_auth_http_client();
    }
}
