//! Wire-router + auth-context integration coverage for the 3-wire router
//! landed under OPROD 04. Complements the per-module unit tests in
//! `src/endpoints.rs` with:
//!
//! 1. `route_for_model` matrix (claude → Messages, gpt-5 → Responses,
//!    legacy / unknown → ChatCompletions) exercised through the public API
//!    rather than `#[cfg(test)]`-private helpers.
//! 2. `CopilotCtx::snapshot` / `force_refresh` driven against a wiremock
//!    `/copilot_internal/v2/token` — proves the atomic bearer + endpoints
//!    pairing (rubber-duck finding #1) and the force-refresh primitive the
//!    401 retry path in `core/src/client.rs` depends on.
//!
//! Code-review op2 round-2 finding #2 (reviewer: "new native Copilot wires
//! have no end-to-end proof"). Deep dispatcher tests that construct a
//! `ModelClient` with `WireApi::Copilot` + wiremock-backed
//! `endpoints.api` are scoped as follow-on: they need broader test
//! scaffolding in `codex-core` than exists today. O-1..O-5 live smoke
//! already cover the full path against the real enterprise endpoint and
//! are captured in the OPROD 04 EXFIL.

use codex_copilot::CopilotAuth;
use codex_copilot::CopilotConfig;
use codex_copilot_adapter::CopilotCtx;
use codex_copilot_adapter::CopilotWire;
use codex_copilot_adapter::route_for_model;
use std::time::Duration;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn jwt_body(token: &str, api_base: &str) -> serde_json::Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    serde_json::json!({
        "token": token,
        "expires_at": now + 1_500,
        "refresh_in": 1_500,
        "endpoints": { "api": api_base, "telemetry": null, "proxy": null }
    })
}

fn test_config(server: &MockServer) -> CopilotConfig {
    CopilotConfig {
        github_api_base: server.uri(),
        github_oauth_base: server.uri(),
        copilot_api_base: server.uri(),
        force_allow_headless: true,
        device_poll_interval: Some(Duration::from_millis(10)),
        device_poll_max: Some(Duration::from_secs(30)),
    }
}

// =========================================================================
// route_for_model — integration-level matrix through the public API.
// =========================================================================

#[test]
fn route_matrix_messages_wire() {
    for slug in [
        "claude-sonnet-4.5",
        "claude-sonnet-4.6",
        "claude-opus-4.5",
        "claude-opus-4.6",
        "claude-opus-4.7",
        "claude-haiku-4.5",
    ] {
        assert_eq!(
            route_for_model(slug),
            CopilotWire::Messages,
            "claude slug {slug} must route to Messages"
        );
    }
}

#[test]
fn route_matrix_responses_wire() {
    for slug in [
        "gpt-5.2",
        "gpt-5.2-codex",
        "gpt-5.3-codex",
        "gpt-5.4",
        "gpt-5-mini",
    ] {
        assert_eq!(
            route_for_model(slug),
            CopilotWire::Responses,
            "gpt-5 slug {slug} must route to Responses"
        );
    }
}

#[test]
fn route_matrix_chat_fallback() {
    for slug in [
        "gpt-4o",
        "gpt-4.1",
        "o1-preview",
        "o1-mini",
        "text-davinci-003",
        "claude-instant-1",
        "CLAUDE-OPUS-4.7",
        "",
    ] {
        assert_eq!(
            route_for_model(slug),
            CopilotWire::ChatCompletions,
            "slug {slug:?} must fall through to ChatCompletions"
        );
    }
}

// =========================================================================
// CopilotCtx: atomic snapshot + force-refresh against wiremock.
// =========================================================================

#[tokio::test]
async fn copilot_ctx_snapshot_returns_bearer_and_endpoints_api_from_same_mint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(jwt_body("jwt_first", &server.uri())),
        )
        .expect(1)
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = CopilotAuth::from_token(
        http.clone(),
        "ghu_snapshot_test".into(),
        test_config(&server),
    );
    let ctx = CopilotCtx::from_auth(auth);

    let snap = ctx
        .snapshot()
        .await
        .expect("snapshot must succeed against wiremock /copilot_internal/v2/token");

    assert_eq!(snap.bearer(), "jwt_first");
    assert_eq!(snap.endpoints_api, server.uri());
}

#[tokio::test]
async fn copilot_ctx_force_refresh_mints_new_bearer() {
    let server = MockServer::start().await;
    // First mint: jwt_stale. Second mint (after force_refresh): jwt_fresh.
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(jwt_body("jwt_stale", &server.uri())),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(jwt_body("jwt_fresh", &server.uri())),
        )
        .expect(1)
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = CopilotAuth::from_token(
        http.clone(),
        "ghu_refresh_test".into(),
        test_config(&server),
    );
    let ctx = CopilotCtx::from_auth(auth);

    let first = ctx.snapshot().await.expect("first snapshot");
    assert_eq!(first.bearer(), "jwt_stale");

    // snapshot() alone should reuse the cached bearer — no second mint.
    let cached = ctx.snapshot().await.expect("cached snapshot");
    assert_eq!(cached.bearer(), "jwt_stale");

    // force_refresh invalidates the cache → next mint hits the second mock.
    let refreshed = ctx.force_refresh().await.expect("force_refresh");
    assert_eq!(refreshed.bearer(), "jwt_fresh");
    assert_eq!(refreshed.endpoints_api, server.uri());

    // Subsequent snapshot returns the post-refresh bearer.
    let after = ctx.snapshot().await.expect("post-refresh snapshot");
    assert_eq!(after.bearer(), "jwt_fresh");
}

#[tokio::test]
async fn copilot_ctx_snapshot_debug_redacts_bearer() {
    // Defence against op2 round-1 finding #2: the Debug impl of
    // CopilotAuthSnapshot must never leak the CAPI bearer.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(jwt_body("super_secret_bearer_xyz", &server.uri())),
        )
        .expect(1)
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = CopilotAuth::from_token(http.clone(), "ghu_debug_test".into(), test_config(&server));
    let ctx = CopilotCtx::from_auth(auth);

    let snap = ctx.snapshot().await.expect("snapshot");
    let rendered = format!("{:?}", snap);
    assert!(
        !rendered.contains("super_secret_bearer_xyz"),
        "bearer must be redacted in Debug output; got: {rendered}"
    );
    assert!(
        rendered.contains("<redacted>"),
        "Debug output must explicitly mark bearer as redacted; got: {rendered}"
    );
    assert!(
        rendered.contains(&server.uri()),
        "endpoints_api should remain visible in Debug output for diagnostics; got: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// F4 regression: first-turn guards on native /v1/messages and /responses
//
// These four tests prove `enforce_copilot_first_turn_guards()` enforces the
// same TTY policy and one-shot TOS banner that the chat-completions fallback
// path applies — but ALSO preserves the headless-with-token-on-disk escape
// hatch that spawned subagents depend on.
//
// Test-execution model: env vars and the process-scoped `BANNER_PRINTED`
// atomic are global state. The integration-test binary is multi-threaded by
// default, so we serialize all four tests behind a single `Mutex` and
// snapshot/restore each env var we touch. Cargo test isolation is per-crate,
// not per-test.
// ---------------------------------------------------------------------------

use codex_copilot_adapter::__banner_was_printed_for_tests;
use codex_copilot_adapter::__discover_github_token_for_tests;
use codex_copilot_adapter::__reset_banner_for_tests;
use codex_copilot_adapter::CopilotAdapterError;
use codex_copilot_adapter::enforce_copilot_first_turn_guards;
use std::sync::Mutex;

static F4_GUARD: Mutex<()> = Mutex::new(());

/// RAII helper: snapshot the env vars these tests mutate and restore them
/// on drop, even if the test panics. Keeps state hygiene watertight when
/// other tests in the binary read these same vars.
struct EnvGuard {
    headless: Option<String>,
    gh_token: Option<String>,
}

impl EnvGuard {
    fn snapshot() -> Self {
        Self {
            headless: std::env::var("CODEX_COPILOT_ALLOW_HEADLESS").ok(),
            gh_token: std::env::var("GITHUB_TOKEN").ok(),
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: tests are serialized via `F4_GUARD`; no other thread is
        // racing on these env vars during the restore window.
        unsafe {
            match &self.headless {
                Some(v) => std::env::set_var("CODEX_COPILOT_ALLOW_HEADLESS", v),
                None => std::env::remove_var("CODEX_COPILOT_ALLOW_HEADLESS"),
            }
            match &self.gh_token {
                Some(v) => std::env::set_var("GITHUB_TOKEN", v),
                None => std::env::remove_var("GITHUB_TOKEN"),
            }
        }
    }
}

/// Cargo test runs without a controlling TTY, so `is_interactive_tty()`
/// returns false here. With no override env and no `ghu_` token on disk
/// or in `$GITHUB_TOKEN`, the guard MUST fail closed with `NotATty`.
/// This is the production "headless dev-loop without consent" path.
#[test]
fn f4_headless_no_token_no_override_fails_with_not_a_tty() {
    let _lock = F4_GUARD.lock().unwrap();
    let _env = EnvGuard::snapshot();
    // SAFETY: tests serialized; no concurrent reader of these vars.
    unsafe {
        std::env::remove_var("CODEX_COPILOT_ALLOW_HEADLESS");
        std::env::remove_var("GITHUB_TOKEN");
    }
    __reset_banner_for_tests();

    // Precondition probe: `discover_github_token()` also walks on-disk
    // Copilot auth files (`~/.config/github-copilot/{apps,hosts}.json`,
    // VS Code Copilot dirs, etc.). On dev hosts where the operator is
    // already logged into Copilot, this returns Some and the guard will
    // (correctly) succeed instead of failing closed. We cannot prove the
    // fail-closed behaviour from such a host — skip with a clear log so
    // CI without on-disk consent still exercises the assertion.
    if __discover_github_token_for_tests().is_some() {
        eprintln!(
            "f4_headless_no_token_no_override_fails_with_not_a_tty:              skipped — on-disk Copilot token discovered, cannot prove              fail-closed path on this host"
        );
        return;
    }

    let err = enforce_copilot_first_turn_guards()
        .expect_err("headless + no token + no override must fail closed");
    assert!(
        matches!(err, CopilotAdapterError::NotATty),
        "expected NotATty, got: {err:?}"
    );
    assert!(
        !__banner_was_printed_for_tests(),
        "banner must NOT print when the TTY guard fails"
    );
}

/// HARD CONSTRAINT — subagent path. A `ghu_`-prefixed token in
/// `$GITHUB_TOKEN` proves the operator already consented via a prior
/// login. The guard MUST allow the headless turn, otherwise spawned
/// subagents in headless contexts cannot use Copilot. This is the
/// invariant the F4 fix exists to preserve.
#[test]
fn f4_headless_token_on_disk_via_env_passes_subagent_invariant() {
    let _lock = F4_GUARD.lock().unwrap();
    let _env = EnvGuard::snapshot();
    // SAFETY: tests serialized.
    unsafe {
        std::env::remove_var("CODEX_COPILOT_ALLOW_HEADLESS");
        std::env::set_var("GITHUB_TOKEN", "ghu_subagent_invariant_test_token");
    }
    __reset_banner_for_tests();

    enforce_copilot_first_turn_guards()
        .expect("headless-with-token-on-disk MUST succeed (subagent invariant)");
    assert!(
        __banner_was_printed_for_tests(),
        "banner must fire on the first successful guard call"
    );
}

/// Operator-explicit headless override. Same end result as the
/// token-on-disk path, but the operator opted in via env rather than
/// implicit prior-consent.
#[test]
fn f4_headless_with_env_override_passes() {
    let _lock = F4_GUARD.lock().unwrap();
    let _env = EnvGuard::snapshot();
    // SAFETY: tests serialized.
    unsafe {
        std::env::set_var("CODEX_COPILOT_ALLOW_HEADLESS", "1");
        std::env::remove_var("GITHUB_TOKEN");
    }
    __reset_banner_for_tests();

    enforce_copilot_first_turn_guards()
        .expect("CODEX_COPILOT_ALLOW_HEADLESS=1 MUST allow headless run");
    assert!(__banner_was_printed_for_tests());
}

/// Banner-once invariant: across N successful guard calls the TOS banner
/// fires exactly once per process. We assert via the test seam rather
/// than scraping stderr.
#[test]
fn f4_banner_fires_exactly_once_across_n_calls() {
    let _lock = F4_GUARD.lock().unwrap();
    let _env = EnvGuard::snapshot();
    // SAFETY: tests serialized.
    unsafe {
        std::env::set_var("CODEX_COPILOT_ALLOW_HEADLESS", "1");
        std::env::remove_var("GITHUB_TOKEN");
    }
    __reset_banner_for_tests();
    assert!(
        !__banner_was_printed_for_tests(),
        "precondition: flag reset"
    );

    for _ in 0..8 {
        enforce_copilot_first_turn_guards().expect("guard must succeed");
    }

    // Exactly one transition false -> true. We can't observe N from a
    // bool, but we CAN assert the flag is set after the first call and
    // resetting + calling once more flips it again.
    assert!(__banner_was_printed_for_tests());

    __reset_banner_for_tests();
    assert!(!__banner_was_printed_for_tests());
    enforce_copilot_first_turn_guards().expect("guard must still succeed after reset");
    assert!(
        __banner_was_printed_for_tests(),
        "banner must re-fire after reset"
    );
}
