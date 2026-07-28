//! GitHub Copilot LLM-plane provider for XLI.
//!
//! This crate owns the wire-agnostic URL/header plumbing that the Copilot
//! dispatch in `codex-core::client.rs` used to carry inline, plus the
//! [`CopilotModelProvider`] type that will implement
//! `codex_model_provider::ModelProvider`. It is the second of two
//! sibling provider crates being extracted under Sortie 1 (the other is
//! `codex-provider-anthropic`).
//!
//! # Status — Sortie 1 complete
//!
//! The provider owns all Copilot-specific dispatch:
//!
//! * URL normalization (`ensure_v1_prefix`) and shared headers
//!   (`stamp_copilot_shared_headers`).
//! * [`CopilotModelProvider`] implements `ModelProvider` with full
//!   ownership of auth lifecycle (`api_provider`, `api_auth`,
//!   `try_refresh_auth`, `note_request_succeeded`, `ensure_session_ctx`),
//!   wire routing (`effective_wire_api`), and streaming (`stream` —
//!   Messages wire for Claude slugs, chat-completions adapter for
//!   legacy GPT slugs).
//! * `core/src/client.rs` has zero inlined Copilot dispatch; the
//!   `messages_dispatch` module delegates through the provider trait.

mod headers;
mod provider;
mod url;

pub use headers::stamp_copilot_shared_headers;
pub use provider::CopilotModelProvider;
pub use url::ensure_v1_prefix;
