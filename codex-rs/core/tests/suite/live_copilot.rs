#![expect(clippy::expect_used)]

//! Live smoke tests for the GitHub Copilot wire (`WireApi::Copilot`).
//!
//! These exercise the Copilot adapter's three-wire router end-to-end against
//! the real proxy + Copilot upstream:
//!   - `claude-*` → CopilotWire::Messages   (native /v1/messages)
//!   - `gpt-5*`   → CopilotWire::Responses  (native /responses)
//!   - others     → CopilotWire::ChatCompletions (chat-completions fallback)
//!
//! `#[ignore]` by default — run locally with:
//! ```bash
//!   CODEX_COPILOT_LIVE=1 \
//!     cargo test -p codex-core --test all -- live_copilot \
//!     --ignored --test-threads=1
//! ```
//!
//! Skip semantics:
//!   - If `CODEX_COPILOT_LIVE` is not set, the test prints a notice and returns
//!     (does NOT fail). This keeps the suite green when run by accident with
//!     `--ignored` on a box without Copilot creds.
//!   - If no Copilot token is discoverable (no `GITHUB_TOKEN` env var AND no
//!     `~/.config/github-copilot/hosts.json`), the test skips with a notice.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Test helpers (local-only; no source under src/ touched).
// ---------------------------------------------------------------------------

fn resolve_xli_binary() -> PathBuf {
    for var in ["XLI_BINARY", "CODEX_BINARY"] {
        if let Ok(path) = std::env::var(var) {
            let p = PathBuf::from(&path);
            if p.is_relative() {
                let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
                let workspace_root = manifest_dir.parent().unwrap();
                let resolved = workspace_root.join(&p);
                if resolved.exists() {
                    return resolved;
                }
            } else if p.exists() {
                return p;
            }
        }
    }
    codex_utils_cargo_bin::cargo_bin("xli").expect("xli binary not found in target/debug")
}

/// Best-effort Copilot token discovery without touching the adapter source.
/// The real adapter inspects more locations (VSCode OAuth, device-flow cache,
/// etc.); this is a conservative lower bound — if either the env var or the
/// hosts.json file is present, we assume `discover_github_token()` will too.
fn copilot_token_available() -> bool {
    if std::env::var("GITHUB_TOKEN")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return true;
    }
    if let Some(home) = std::env::var_os("HOME") {
        let hosts = Path::new(&home).join(".config/github-copilot/hosts.json");
        if hosts.exists() {
            return true;
        }
    }
    false
}

fn opted_in() -> bool {
    std::env::var("CODEX_COPILOT_LIVE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Returns true if the test should be skipped (with a printed reason).
fn skip_unless_configured(test_name: &str) -> bool {
    if !opted_in() {
        eprintln!(
            "Skipping {test_name} — set CODEX_COPILOT_LIVE=1 to opt in to live Copilot tests"
        );
        return true;
    }
    if !copilot_token_available() {
        eprintln!(
            "Skipping {test_name} — no Copilot token discoverable (GITHUB_TOKEN unset and ~/.config/github-copilot/hosts.json missing)"
        );
        return true;
    }
    false
}

struct RunResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// Spin up xli with a temp CODEX_HOME pointing at a config that uses the
/// `copilot` provider (`wire_api = "copilot"`) and run `xli exec` once.
fn run_copilot_exec(model: &str, prompt: &str) -> RunResult {
    #![expect(clippy::unwrap_used)]
    let dir = TempDir::new().unwrap();
    let codex_home = dir.path().join(".xli");
    std::fs::create_dir_all(&codex_home).unwrap();

    // Re-use the developer's Copilot hosts.json so the adapter's token
    // discovery path inside the spawned binary works without us having to
    // recreate the OAuth dance.
    if let Some(home) = std::env::var_os("HOME") {
        let src = Path::new(&home).join(".config/github-copilot");
        let dst_root = codex_home.parent().unwrap().join(".config");
        let dst = dst_root.join("github-copilot");
        if src.exists() {
            std::fs::create_dir_all(&dst_root).unwrap();
            // Symlink dir so we don't duplicate state. Best-effort.
            #[cfg(unix)]
            {
                let _ = std::os::unix::fs::symlink(&src, &dst);
            }
        }
    }

    let config = format!(
        r#"model = "{model}"
model_provider = "copilot"
approval_policy = "never"

[model_providers.copilot]
name = "GitHub Copilot"
wire_api = "copilot"

[projects."{workdir}"]
trust_level = "trusted"
"#,
        model = model,
        workdir = dir.path().display(),
    );
    std::fs::write(codex_home.join("config.toml"), config).unwrap();

    let binary = resolve_xli_binary();

    // Point HOME at our temp dir so the spawned binary's adapter sees only
    // the symlinked Copilot creds (and can't pick up other state).
    let fake_home = codex_home.parent().unwrap().to_path_buf();

    let output = Command::new(&binary)
        .arg("exec")
        .arg("--json")
        .arg("--skip-git-repo-check")
        .arg(prompt)
        .env("CODEX_HOME", codex_home.to_str().unwrap())
        .env("HOME", fake_home.to_str().unwrap())
        // Allow the adapter to run headless from a test process.
        .env("CODEX_COPILOT_ALLOW_HEADLESS", "1")
        .env("CODEX_SANDBOX_NETWORK_DISABLED", "")
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn xli");

    RunResult {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    }
}

/// Sanity-check that the Copilot wire router classifies the model the way
/// this test expects. Cheap — purely a deterministic table lookup, no
/// network. Run inside the test so a future routing-table change forces us
/// to update the test rather than producing a silently-misleading pass.
fn assert_route(model: &str, expected: codex_copilot_adapter::CopilotWire) {
    let actual = codex_copilot_adapter::route_for_model(model);
    assert_eq!(
        actual, expected,
        "Copilot router classified `{model}` as {actual:?}, expected {expected:?}"
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[ignore]
#[test]
fn live_copilot_native_messages_basic() {
    if skip_unless_configured("live_copilot_native_messages_basic") {
        return;
    }
    assert_route(
        "claude-sonnet-4.6",
        codex_copilot_adapter::CopilotWire::Messages,
    );

    let result = run_copilot_exec(
        "claude-sonnet-4.6",
        "Reply with exactly the word 'pong' and nothing else.",
    );
    assert_eq!(
        result.exit_code, 0,
        "exit code should be 0\nstderr: {}\nstdout: {}",
        result.stderr, result.stdout
    );
    assert!(
        result.stdout.to_lowercase().contains("pong"),
        "response should contain 'pong'\nstdout: {}",
        result.stdout
    );
}

#[ignore]
#[test]
fn live_copilot_native_messages_thinking() {
    if skip_unless_configured("live_copilot_native_messages_thinking") {
        return;
    }
    assert_route(
        "claude-opus-4.7",
        codex_copilot_adapter::CopilotWire::Messages,
    );

    // claude-opus-4.7 supports extended thinking; we don't explicitly enable
    // it via flags here (the profile default suffices), but the prompt asks
    // the model to reason briefly. The point is to prove the Messages wire
    // round-trips a thinking-capable model without crashing the adapter.
    let result = run_copilot_exec(
        "claude-opus-4.7",
        "What is 7 * 8? Think briefly then answer with just the number.",
    );
    assert_eq!(
        result.exit_code, 0,
        "exit code should be 0\nstderr: {}\nstdout: {}",
        result.stderr, result.stdout
    );
    assert!(
        result.stdout.contains("56"),
        "should produce correct answer\nstdout: {}",
        result.stdout
    );
}

#[ignore]
#[test]
fn live_copilot_native_responses() {
    if skip_unless_configured("live_copilot_native_responses") {
        return;
    }
    assert_route("gpt-5.4", codex_copilot_adapter::CopilotWire::Responses);

    let result = run_copilot_exec(
        "gpt-5.4",
        "Reply with exactly the word 'pong' and nothing else.",
    );
    assert_eq!(
        result.exit_code, 0,
        "exit code should be 0\nstderr: {}\nstdout: {}",
        result.stderr, result.stdout
    );
    assert!(
        result.stdout.to_lowercase().contains("pong"),
        "response should contain 'pong'\nstdout: {}",
        result.stdout
    );
}

#[ignore]
#[test]
fn live_copilot_chat_fallback() {
    if skip_unless_configured("live_copilot_chat_fallback") {
        return;
    }
    // Opt-in for the chat-completions fallback path: many Copilot accounts
    // don't have gpt-4o entitlements, so this requires a second opt-in env
    // var to avoid a noisy 403/404 by default.
    if std::env::var("CODEX_COPILOT_LIVE_CHAT").ok().as_deref() != Some("1") {
        eprintln!(
            "Skipping live_copilot_chat_fallback — set CODEX_COPILOT_LIVE_CHAT=1 to also exercise the chat-completions fallback path"
        );
        return;
    }
    assert_route(
        "gpt-4o",
        codex_copilot_adapter::CopilotWire::ChatCompletions,
    );

    let result = run_copilot_exec(
        "gpt-4o",
        "Reply with exactly the word 'pong' and nothing else.",
    );
    assert_eq!(
        result.exit_code, 0,
        "exit code should be 0\nstderr: {}\nstdout: {}",
        result.stderr, result.stdout
    );
    assert!(
        result.stdout.to_lowercase().contains("pong"),
        "response should contain 'pong'\nstdout: {}",
        result.stdout
    );
}
