//! Gemini model metadata: output token caps and safety settings.

/// Returns the max output token cap for a Gemini model slug.
///
/// Gemini 2.5 Pro/Flash support up to 65K output tokens; older 2.0 models
/// cap at 8K. Falls back to 8192 for unknown slugs.
pub fn gemini_max_output_tokens(slug: &str) -> u32 {
    let normalized = slug.to_lowercase();
    if normalized.starts_with("gemini-2.5") || normalized.starts_with("gemini-2-5") {
        65_536
    } else {
        8_192
    }
}

/// Returns true if the model supports Gemini's thinking (thinkingConfig).
///
/// Currently Gemini 2.5 Pro and Flash models support thinking.
pub fn supports_thinking(slug: &str) -> bool {
    let normalized = slug.to_lowercase();
    normalized.starts_with("gemini-2.5") || normalized.starts_with("gemini-2-5")
}

/// Returns true if the model supports the `googleSearch` grounding tool.
///
/// Google Search grounding (GS-3) is supported by Gemini 2.5+ models and
/// the entire Gemini 3 family (`gemini-3-*`, `gemini-3.1-*`). The
/// `googleSearch: {}` tool entry is silently dropped for older slugs —
/// the API rejects it with 400 on pre-2.5 models.
///
/// This is a static slug check; update when new generations are
/// confirmed to support grounding.
pub fn supports_grounding(slug: &str) -> bool {
    let normalized = slug.to_lowercase();
    normalized.starts_with("gemini-2.5")
        || normalized.starts_with("gemini-2-5")
        || normalized.starts_with("gemini-3-")
        || normalized.starts_with("gemini-3.")
}

/// Default safety settings that lower Gemini's aggressive content filter.
///
/// Gemini blocks legitimate coding prompts (shell commands, security
/// discussions) at the default threshold. We set all categories to
/// `BLOCK_ONLY_HIGH` so only clearly harmful content is blocked.
pub fn default_gemini_safety_settings() -> Vec<codex_api::SafetySetting> {
    const CATEGORIES: &[&str] = &[
        "HARM_CATEGORY_HARASSMENT",
        "HARM_CATEGORY_HATE_SPEECH",
        "HARM_CATEGORY_SEXUALLY_EXPLICIT",
        "HARM_CATEGORY_DANGEROUS_CONTENT",
        "HARM_CATEGORY_CIVIC_INTEGRITY",
    ];
    CATEGORIES
        .iter()
        .map(|cat| codex_api::SafetySetting {
            category: (*cat).to_string(),
            threshold: "BLOCK_ONLY_HIGH".to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_25_gets_65k_cap() {
        assert_eq!(gemini_max_output_tokens("gemini-2.5-pro"), 65_536);
        assert_eq!(gemini_max_output_tokens("gemini-2.5-flash"), 65_536);
    }

    #[test]
    fn unknown_slug_gets_8k_cap() {
        assert_eq!(gemini_max_output_tokens("gemini-3-flash-preview"), 8_192);
    }

    #[test]
    fn default_safety_settings_cover_all_categories() {
        let settings = default_gemini_safety_settings();
        assert_eq!(settings.len(), 5);
        assert!(settings.iter().all(|s| s.threshold == "BLOCK_ONLY_HIGH"));
    }

    #[test]
    fn supports_thinking_gemini_25() {
        assert!(supports_thinking("gemini-2.5-pro"));
        assert!(supports_thinking("gemini-2.5-flash"));
        assert!(supports_thinking("Gemini-2.5-Pro-Preview"));
        assert!(supports_thinking("gemini-2-5-flash"));
    }

    #[test]
    fn no_thinking_for_older_models() {
        assert!(!supports_thinking("gemini-2.0-flash"));
        assert!(!supports_thinking("gemini-1.5-pro"));
        assert!(!supports_thinking("gpt-4"));
    }

    #[test]
    fn supports_grounding_gemini_25() {
        assert!(supports_grounding("gemini-2.5-pro"));
        assert!(supports_grounding("gemini-2.5-flash"));
        assert!(supports_grounding("Gemini-2.5-Pro-Preview"));
        assert!(supports_grounding("gemini-2-5-flash"));
    }

    #[test]
    fn no_grounding_for_older_models() {
        assert!(!supports_grounding("gemini-2.0-flash"));
        assert!(!supports_grounding("gemini-1.5-pro"));
        assert!(!supports_grounding("gpt-4"));
    }

    #[test]
    fn supports_grounding_gemini_3_family() {
        // Gemini 3 family (flash + pro previews) all support grounding
        // per the live proxy. Without this, --search is a no-op for the
        // default profiles (`gemini-3-flash-preview`, `gemini-3.1-pro-preview`).
        assert!(supports_grounding("gemini-3-flash-preview"));
        assert!(supports_grounding("gemini-3-pro-preview"));
        assert!(supports_grounding("gemini-3.1-pro-preview"));
        assert!(supports_grounding("gemini-3.1-flash-lite-preview"));
        assert!(supports_grounding("Gemini-3-Flash-Preview"));
    }
}
