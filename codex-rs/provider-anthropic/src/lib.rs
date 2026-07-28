//! Anthropic Messages wire provider for XLI.
//!
//! This crate owns the Anthropic `/messages` translator (internal
//! `ResponseItem[]` ↔ Anthropic JSON) and the provider type that
//! implements `codex_model_provider::ModelProvider`. It is the first of
//! two sibling provider crates being extracted under Sortie 1 (the
//! other is `codex-provider-copilot`).
//!
//! # Status (Sortie 1, through Commit 5b)
//!
//! * Commit 3a `git mv`'d the translator and its tests out of
//!   `codex-core` with no logic changes.
//! * Commit 3b moved the three pure Anthropic helpers
//!   (`is_anthropic_model`, `anthropic_thinking_param`,
//!   `anthropic_max_output_tokens`) into this crate.
//! * Commit 3c added [`AnthropicMessagesProvider`], the provider type
//!   that overrides
//!   [`codex_model_provider::ModelProvider::effective_wire_api`] to
//!   auto-upgrade non-Anthropic slugs to the Responses wire.
//! * Commit 5a wired `AnthropicMessagesProvider` into the
//!   `codex-provider-registry` factory for `WireApi::Messages`, but no
//!   production code called `provider.stream()` yet.
//! * Commit 5b (this commit) lifts the **pure request-build** portion
//!   of `core::client::stream_messages_api` into [`request`] and
//!   overrides [`AnthropicMessagesProvider::stream`] to build the
//!   request, then delegate to core's orchestration via the
//!   [`codex_model_provider::MessagesBackend`] trait. The 401 retry
//!   loop, Copilot base-url splice, telemetry composition, and
//!   `ApiMessagesClient` dispatch all stay in `codex-core`.
//!
//! Commit 5c will do the equivalent lift for the Copilot wire;
//! Commits 6/7 delete the legacy shims and run the end-to-end test
//! gate.

mod model;
mod provider;
pub mod request;
pub mod stream_accumulator;
pub mod stream_invariants;
mod wire;

pub use model::anthropic_effort_param;
pub use model::anthropic_max_output_tokens;
pub use model::anthropic_thinking_always_on;
pub use model::anthropic_thinking_param;
pub use model::is_anthropic_model;
pub use provider::AnthropicMessagesProvider;
pub use request::Sampling;
pub use request::build_messages_extra_headers;
pub use request::build_messages_extra_headers_with_retention;
pub use request::cache_control_value;
pub use request::build_messages_request;
pub use stream_accumulator::StreamAccumulator;
pub use stream_accumulator::StreamEvent;
pub use stream_accumulator::WireError;
pub use wire::conversation_to_anthropic_messages;
pub use wire::extract_developer_blocks;
pub use wire::tools_to_anthropic_format;

#[cfg(test)]
#[path = "regression_tests.rs"]
mod regression_tests;
