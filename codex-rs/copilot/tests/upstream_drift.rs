//! Drift canaries against the pinned upstream `codex-copilot` crate.
//!
//! These tests fail loudly when an upstream version bump changes a value the
//! adapter intentionally overrides. A failure here is not a bug in the
//! adapter — it is a prompt for a human to re-evaluate whether our override
//! is still correct against the new upstream baseline.

/// Adapter pins `X-GitHub-Api-Version: 2025-05-01` to match
/// `microsoft/vscode-copilot-chat@9e668cb12144`
/// (`src/platform/networking/common/networking.ts:283`). Upstream
/// `codex-copilot` rev `26ff8f1d` ships the older `2025-04-01` and stamps
/// it unconditionally in `CopilotHeaders::into_header_map`
/// (`codex-copilot/src/client.rs:139`).
///
/// If upstream bumps its constant, this test fails. When that happens:
///   1. Check what value `vscode-copilot-chat` is currently shipping.
///   2. If they match, simplify by removing the adapter overrides in
///      `codex-copilot-adapter::wire::build_headers` and
///      `codex-core::client::stamp_copilot_shared_headers`.
///   3. If they still diverge, update `codex_copilot_adapter::GITHUB_API_VERSION`
///      and the expected upstream value in this test.
#[test]
fn upstream_codex_copilot_still_pins_stale_api_version() {
    assert_eq!(
        codex_copilot::auth::GITHUB_API_VERSION,
        "2025-04-01",
        "upstream codex-copilot bumped GITHUB_API_VERSION; re-evaluate the \
         adapter override in codex_copilot_adapter::GITHUB_API_VERSION and \
         core::client::stamp_copilot_shared_headers"
    );
}
