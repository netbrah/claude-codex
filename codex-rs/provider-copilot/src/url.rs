//! URL normalization helpers for the GitHub Copilot LLM plane.
//!
//! These are pure, side-effect-free string utilities. They do not depend
//! on any runtime state, which is why they can live in the leaf provider
//! crate rather than in `codex-core` alongside the orchestration.

/// Ensures `url` ends with the canonical `/v1` path segment.
///
/// Idempotent: calling this repeatedly on the same URL yields a single
/// `/v1` suffix, not `/v1/v1/v1`. Required because the 401-retry loop in
/// the Messages-wire orchestration re-splices the URL on every iteration
/// and some CAPI envelopes return `endpoints.api` already terminated
/// with `/v1`, which would produce `/v1/v1/messages` under naive append.
///
/// Also normalizes a single trailing slash (`host/v1/` → `host/v1`) so
/// both forms compare equal. Does not strip multiple slashes — a
/// genuinely malformed URL like `host//v1//` would still get an extra
/// `/v1` appended, which surfaces visibly in error logs rather than
/// silently mutating bytes.
pub fn ensure_v1_prefix(url: &mut String) {
    let trimmed = url.trim_end_matches('/');
    if !trimmed.ends_with("/v1") {
        *url = format!("{trimmed}/v1");
    } else if trimmed.len() != url.len() {
        // Normalize: strip the single trailing slash so all callers see the
        // same canonical form `<host>/v1`.
        *url = trimmed.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_when_absent() {
        let mut url = "https://api.githubcopilot.com".to_string();
        ensure_v1_prefix(&mut url);
        assert_eq!(url, "https://api.githubcopilot.com/v1");
    }

    #[test]
    fn strips_trailing_slash_then_appends() {
        let mut url = "https://api.githubcopilot.com/".to_string();
        ensure_v1_prefix(&mut url);
        assert_eq!(url, "https://api.githubcopilot.com/v1");
    }

    #[test]
    fn idempotent_on_already_prefixed_url() {
        let mut url = "https://api.githubcopilot.com/v1".to_string();
        ensure_v1_prefix(&mut url);
        assert_eq!(
            url, "https://api.githubcopilot.com/v1",
            "must not produce /v1/v1 — the F3 regression"
        );
    }

    #[test]
    fn normalizes_v1_with_trailing_slash() {
        let mut url = "https://api.githubcopilot.com/v1/".to_string();
        ensure_v1_prefix(&mut url);
        assert_eq!(
            url, "https://api.githubcopilot.com/v1",
            "trailing slash on already-prefixed URL must be normalized"
        );
    }

    #[test]
    fn survives_n_iterations() {
        // The 401-retry loop calls this on every retry iteration. After 8
        // calls the URL must still be a single canonical /v1 — no growth.
        let mut url = "https://api.githubcopilot.com".to_string();
        for _ in 0..8 {
            ensure_v1_prefix(&mut url);
        }
        assert_eq!(url, "https://api.githubcopilot.com/v1");
    }

    #[test]
    fn handles_enterprise_host() {
        let mut url = "https://api.business.githubcopilot.com".to_string();
        ensure_v1_prefix(&mut url);
        assert_eq!(url, "https://api.business.githubcopilot.com/v1");

        // And on retry:
        ensure_v1_prefix(&mut url);
        assert_eq!(url, "https://api.business.githubcopilot.com/v1");
    }
}
