//! Gemini `streamGenerateContent` wire provider for XLI.
//!
//! This crate owns the Gemini request translator (ResponseItem[] ->
//! Gemini contents[]), model metadata, and the `GeminiModelProvider`
//! type that implements `codex_model_provider::ModelProvider`.
//!
//! The HTTP endpoint client (`GenerateContentClient`) and SSE parser
//! (`spawn_generate_content_stream`) live in `codex-api` alongside
//! the Anthropic `MessagesClient` — same architectural layer.

mod error;
mod model;
mod provider;
pub mod request;
pub mod schema_sanitize;

pub use model::default_gemini_safety_settings;
pub use model::gemini_max_output_tokens;
pub use model::supports_grounding;
pub use provider::GeminiModelProvider;
pub use request::conversation_to_gemini_contents;
pub use request::SYNTHETIC_THOUGHT_SIGNATURE;
pub use request::extract_system_instruction;
pub use request::tool_choice_to_gemini;
pub use request::tools_to_gemini_format;
