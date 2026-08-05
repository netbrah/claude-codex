pub use codex_api::ResponseEvent;

/// Review thread system prompt. Edit `core/src/review_prompt.md` to customize.
pub const REVIEW_PROMPT: &str = include_str!("../review_prompt.md");

// Centralized templates for review-related user messages
pub const REVIEW_EXIT_SUCCESS_TMPL: &str = include_str!("../templates/review/exit_success.xml");
pub const REVIEW_EXIT_INTERRUPTED_TMPL: &str =
    include_str!("../templates/review/exit_interrupted.xml");

pub use codex_prompt::Prompt;
pub use codex_prompt::ResponseStream;

#[cfg(test)]
#[path = "client_common_tests.rs"]
mod tests;
