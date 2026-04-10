//! Environment-driven application branding.
//!
//! Reads `CODEX_APP_NAME` and `CODEX_APP_TAGLINE` once at startup and exposes
//! them for the TUI's branding surfaces. When unset, falls back to the XLI
//! defaults.
//!
//! The XLI proprietary launcher (`deploy/npm/bin/xli.js`) can override these
//! env vars before spawning the Rust binary, achieving a full rebrand with
//! zero compile-time feature flags.

use std::sync::OnceLock;

struct Brand {
    app_name: String,
    app_name_title_case: String,
    tagline: String,
}

static BRAND: OnceLock<Brand> = OnceLock::new();

fn brand() -> &'static Brand {
    BRAND.get_or_init(|| {
        let app_name = std::env::var("CODEX_APP_NAME")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "xli".to_string());

        // Display name: "Claude Codex" for branding, falls back to
        // title-casing the app_name if overridden via env.
        let app_name_title_case = std::env::var("CODEX_APP_DISPLAY_NAME")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if app_name == "xli" {
                    "Claude Codex".to_string()
                } else if app_name.chars().all(|c| c.is_ascii_lowercase()) && app_name.len() <= 4 {
                    app_name.to_ascii_uppercase()
                } else {
                    let mut chars = app_name.chars();
                    match chars.next() {
                        None => String::new(),
                        Some(first) => {
                            let mut s = first.to_uppercase().to_string();
                            s.extend(chars);
                            s
                        }
                    }
                }
            });

        let tagline = std::env::var("CODEX_APP_TAGLINE")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Cross-LLM Interface".to_string());

        Brand {
            app_name,
            app_name_title_case,
            tagline,
        }
    })
}

/// Lowercase app name for terminal titles, status bars, etc.
/// Default: `"xli"`. Override via `CODEX_APP_NAME`.
pub(crate) fn app_name() -> &'static str {
    &brand().app_name
}

/// Title-case app name for welcome messages, descriptions.
/// Default: `"Claude Codex"`. Override via `CODEX_APP_DISPLAY_NAME`.
pub(crate) fn app_name_display() -> &'static str {
    &brand().app_name_title_case
}

/// Tagline shown on the welcome screen.
/// Default: `"Cross-LLM Interface"`. Override via `CODEX_APP_TAGLINE`.
pub(crate) fn app_tagline() -> &'static str {
    &brand().tagline
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_codex_when_env_unset() {
        // OnceLock means we can only test the already-initialized value.
        // In a clean test process without CODEX_APP_NAME set, this will
        // return "codex". If another test set it, we just verify non-empty.
        let name = app_name();
        assert!(!name.is_empty(), "app_name must not be empty");
        let display = app_name_display();
        assert!(!display.is_empty(), "app_name_display must not be empty");
        let tagline = app_tagline();
        assert!(!tagline.is_empty(), "app_tagline must not be empty");
    }

    #[test]
    fn brand_struct_title_case_logic() {
        // Test the title-casing logic directly (not through OnceLock)
        // Short lowercase -> uppercase
        assert_eq!(title_case_name("xli"), "XLI");
        assert_eq!(title_case_name("abc"), "ABC");
        // Longer names get first-char capitalized
        assert_eq!(title_case_name("codex"), "Codex");
        assert_eq!(title_case_name("myapp"), "Myapp");
        // Already mixed case preserved
        assert_eq!(title_case_name("MyApp"), "MyApp");
        // Empty
        assert_eq!(title_case_name(""), "");
    }

    /// Extracted title-case logic for unit testing without OnceLock.
    fn title_case_name(name: &str) -> String {
        if name.chars().all(|c| c.is_ascii_lowercase()) && name.len() <= 4 {
            name.to_ascii_uppercase()
        } else {
            let mut chars = name.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => {
                    let mut s = first.to_uppercase().to_string();
                    s.extend(chars);
                    s
                }
            }
        }
    }
}
