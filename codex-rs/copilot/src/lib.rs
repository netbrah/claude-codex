//! codex-copilot-adapter — adapter that plugs `codex-copilot` (GitHub Copilot
//! piggyback) into claude-codex's `WireApi::Copilot` dispatch path.
//!
//! # Scope
//!
//! This crate owns **only** the bridging layer:
//!
//! 1. Build an OpenAI-style chat-completions body from a claude-codex `Prompt`.
//! 2. Drive the chat-completions POST via `crate::wire::stream_chat` — which
//!    reuses upstream's public primitives (`CopilotHeaders`, `SseReassembler`,
//!    `CopilotAuth::{token, cache_mut, endpoints}`) and adds three headers
//!    the upstream default builder omits for our agentic context
//!    (`x-initiator: agent`, `x-interaction-id`, `openai-intent:
//!    conversation-agent`) plus `Retry-After` honoring on 402/429.
//!    Re-implements the 401-retry pattern line-for-line from upstream so
//!    invariant 10 is preserved even though we don't call
//!    `CopilotHttpClient::chat_stream_with_auth` directly.
//! 3. Run a side-channel pass over the raw SSE bytes to capture the
//!    `finish_reason` values upstream's reassembler drops (invariant 15) and
//!    the `usage` object it doesn't parse (invariant 16), without editing
//!    the frozen crate.
//! 4. Reassemble the SSE response into claude-codex `ResponseEvent` values so
//!    the existing turn loop in `codex-rs/core/src/codex.rs` is unchanged.
//!
//! The Copilot-specific 66-test baseline lives upstream in
//! `netbrah/codex-agent`. This crate deliberately does not vendor those tests;
//! it is the adapter only.
//!
//! # Non-negotiable invariants (inherited by reference)
//!
//! See `netbrah/copilot-codex/docs/integration/02-design.md` §8 for the full
//! checklist. Every rule from `codex-agent/AGENTS.md` §"No-regression
//! invariants" applies here too:
//!
//! - No `anyhow` bleed: this crate uses `thiserror` only.
//! - `rustls-tls` on reqwest.
//! - TTY guard + TOS banner fire on the adapter's first stream call, not on
//!   `ModelClient::new`.
//! - `chat_stream_with_auth` owns 401 retry; the adapter never calls
//!   `chat_stream_raw` directly.

#![deny(missing_debug_implementations)]
#![warn(clippy::pedantic)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::module_name_repetitions)]

use thiserror::Error;

pub mod adapter;
pub mod auth_client;
pub mod capi_models;
pub mod ctx;
pub mod endpoints;
pub mod mapping;
pub mod request;
pub mod wire;

#[doc(hidden)]
pub use adapter::__banner_was_printed_for_tests;
#[doc(hidden)]
pub use adapter::__reset_banner_for_tests;
pub use adapter::CopilotAdapter;
pub use adapter::enforce_copilot_first_turn_guards;
pub use adapter::stream;
pub use adapter::stream_inner;
pub use auth_client::build_copilot_auth_http_client;
pub use capi_models::CapiModelCatalog;
pub use capi_models::CapiModelInfo;
pub use capi_models::CapiModelLimits;
pub use capi_models::CapiModelSupports;
pub use capi_models::fetch_capi_models;
pub use ctx::CopilotAuthSnapshot;
pub use ctx::CopilotCtx;
pub use ctx::SharedCopilotCtx;
pub use endpoints::CopilotWire;
pub use endpoints::route_for_model;
pub use wire::CompletionTokensDetails;
pub use wire::PromptTokensDetails;
pub use wire::WireCallOptions;
pub use wire::WireError;
pub use wire::WireEvent;
pub use wire::WireUsage;

/// Upstream Copilot header constants re-exported for use by `codex-core`
/// without needing a direct dep on the pinned `codex-copilot` crate. See
/// `netbrah/codex-agent/codex-copilot/src/auth.rs` for canonical values.
pub use codex_copilot::auth::COPILOT_INTEGRATION_ID;
/// Upstream Copilot header constants re-exported for use by `codex-core`
/// without needing a direct dep on the pinned `codex-copilot` crate. See
/// `netbrah/codex-agent/codex-copilot/src/auth.rs` for canonical values.
pub use codex_copilot::auth::EDITOR_VERSION;

/// Value for `X-GitHub-Api-Version` on every Copilot LLM-plane request
/// (chat-completions / responses / messages). Matches what real
/// `microsoft/vscode-copilot-chat@9e668cb12144` ships on every CAPI POST
/// (`src/platform/networking/common/networking.ts:283`).
///
/// Upstream `codex-copilot::auth::GITHUB_API_VERSION` is one month stale at
/// `2025-04-01` and is unconditionally stamped by
/// `CopilotHeaders::into_header_map`. We override with this value at every
/// known wire seam:
/// - `/chat/completions`: `wire::build_headers()` (codex-copilot-adapter)
/// - `/v1/messages` and `/responses`: `stamp_copilot_shared_headers()` in
///   `codex-core::client`
///
/// A drift canary in `tests/upstream_drift.rs` asserts upstream still pins
/// `2025-04-01`; if it bumps, the test fails and we re-evaluate this pin.
pub const GITHUB_API_VERSION: &str = "2025-05-01";

/// Lowercase header name for `X-GitHub-Api-Version`. Lowercase to match
/// upstream `CopilotHeaders::into_header_map` so our overrides replace
/// (not duplicate) the upstream value.
pub const X_GITHUB_API_VERSION: &str = "x-github-api-version";

/// Test-only re-export: probe whether a `ghu_` token is discoverable in the
/// current process environment (env or on-disk Copilot auth files). Used by
/// the F4 regression suite to detect when the test host already has prior
/// consent and the fail-closed assertion cannot be verified.
#[doc(hidden)]
pub use codex_copilot::auth::discover_github_token as __discover_github_token_for_tests;

#[derive(Debug, Error)]
pub enum CopilotAdapterError {
    #[error("copilot auth: {0}")]
    CopilotAuth(#[from] codex_copilot::CopilotAuthError),

    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("not an interactive tty; set CODEX_COPILOT_ALLOW_HEADLESS=1 to override")]
    NotATty,

    #[error("copilot token discovery failed: {0}")]
    TokenDiscovery(String),

    #[error("sse parse: {0}")]
    Sse(String),

    /// Wire-layer failure (upstream non-2xx, rate-limit, header build). See
    /// `crate::wire::WireError`.
    #[error("wire: {0}")]
    Wire(#[from] wire::WireError),
}

pub type Result<T> = std::result::Result<T, CopilotAdapterError>;
