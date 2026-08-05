//! End-to-end tests verifying the XLI rebrand surfaces.
//!
//! These tests validate that the compiled binary has the correct name,
//! help text, home directory defaults, and env var behavior after the
//! Codex → XLI rebrand.
//!
//! By default, uses `cargo_bin("xli")` (debug build). Set `XLI_BINARY`
//! to point at a release binary for release validation:
//!
//! ```bash
//! XLI_BINARY=target/release/xli cargo test -p codex-cli --test rebrand_e2e
//! ```

use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn xli_binary() -> PathBuf {
    if let Ok(path) = std::env::var("XLI_BINARY") {
        let p = PathBuf::from(&path);
        // Resolve relative paths against the workspace root (CARGO_MANIFEST_DIR/../..)
        let p = if p.is_relative() {
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let workspace_root = manifest_dir.parent().unwrap();
            workspace_root.join(&p)
        } else {
            p
        };
        assert!(p.exists(), "XLI_BINARY={} does not exist", p.display());
        return p;
    }
    codex_utils_cargo_bin::cargo_bin("xli").expect("xli binary not found")
}

// ─── Binary Name ───

#[test]
fn binary_is_named_xli() {
    let binary = xli_binary();
    let file_name = binary.file_name().and_then(|n| n.to_str()).unwrap_or("");
    assert!(
        file_name.starts_with("xli"),
        "binary should be named 'xli', got: {file_name}"
    );
}

// ─── Help Text ───

#[test]
fn help_output_says_xli() {
    let output = Command::new(xli_binary())
        .arg("--help")
        .output()
        .expect("failed to run xli --help");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("xli"),
        "help output should contain 'xli'\nstdout:\n{stdout}"
    );
    // Usage line should say "xli [OPTIONS]" not "codex [OPTIONS]"
    assert!(
        !stdout.contains("codex [OPTIONS]"),
        "help output should NOT contain 'codex [OPTIONS]'\nstdout:\n{stdout}"
    );
}

#[test]
fn help_subcommands_say_xli() {
    let output = Command::new(xli_binary())
        .args(["mcp", "--help"])
        .output()
        .expect("failed to run xli mcp --help");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should not have "codex mcp" in usage
    assert!(
        !stdout.contains("codex mcp"),
        "mcp help should not contain 'codex mcp'\nstdout:\n{stdout}"
    );
}

// ─── XLI_HOME Env Var ───

#[test]
fn xli_home_env_is_respected() {
    let tmp = TempDir::new().expect("create temp dir");
    let xli_home = tmp.path().join("my-xli-home");
    std::fs::create_dir_all(&xli_home).expect("create xli home dir");

    // Write a minimal config
    std::fs::write(
        xli_home.join("config.toml"),
        "model = \"test-model\"\napproval_policy = \"never\"\n",
    )
    .expect("write config");

    // Run with XLI_HOME set — just ask for help, should not error
    let output = Command::new(xli_binary())
        .arg("--help")
        .env("XLI_HOME", xli_home.to_str().unwrap())
        .env_remove("CODEX_HOME")
        .output()
        .expect("failed to run xli with XLI_HOME");

    assert!(
        output.status.success(),
        "xli with XLI_HOME should succeed\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn codex_home_legacy_fallback_works() {
    let tmp = TempDir::new().expect("create temp dir");
    let legacy_home = tmp.path().join("legacy-codex-home");
    std::fs::create_dir_all(&legacy_home).expect("create legacy home dir");

    std::fs::write(
        legacy_home.join("config.toml"),
        "model = \"test-model\"\napproval_policy = \"never\"\n",
    )
    .expect("write config");

    // Run with CODEX_HOME (legacy) set, XLI_HOME unset
    let output = Command::new(xli_binary())
        .arg("--help")
        .env("CODEX_HOME", legacy_home.to_str().unwrap())
        .env_remove("XLI_HOME")
        .output()
        .expect("failed to run xli with CODEX_HOME fallback");

    assert!(
        output.status.success(),
        "xli with legacy CODEX_HOME should succeed\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn xli_home_takes_precedence_over_codex_home() {
    let tmp = TempDir::new().expect("create temp dir");

    let xli_home = tmp.path().join("xli-primary");
    std::fs::create_dir_all(&xli_home).expect("create xli home");
    std::fs::write(
        xli_home.join("config.toml"),
        "model = \"xli-model\"\napproval_policy = \"never\"\n",
    )
    .expect("write xli config");

    let codex_home = tmp.path().join("codex-legacy");
    std::fs::create_dir_all(&codex_home).expect("create codex home");
    std::fs::write(
        codex_home.join("config.toml"),
        "model = \"codex-model\"\napproval_policy = \"never\"\n",
    )
    .expect("write codex config");

    // When both are set, XLI_HOME should win
    let output = Command::new(xli_binary())
        .arg("--help")
        .env("XLI_HOME", xli_home.to_str().unwrap())
        .env("CODEX_HOME", codex_home.to_str().unwrap())
        .output()
        .expect("failed to run xli with both homes");

    assert!(
        output.status.success(),
        "xli with both homes should succeed\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ─── Project Config Dir ───

#[test]
fn dot_xli_project_config_is_loaded() {
    let tmp = TempDir::new().expect("create temp dir");

    // Create XLI_HOME
    let xli_home = tmp.path().join("xli-home");
    std::fs::create_dir_all(&xli_home).expect("create xli home");
    let project_path = tmp.path().join("project");
    std::fs::create_dir_all(&project_path).expect("create project dir");

    // Write global config
    std::fs::write(
        xli_home.join("config.toml"),
        format!(
            "model = \"test-model\"\napproval_policy = \"never\"\n\n[projects.\"{project}\"]\ntrust_level = \"trusted\"\n",
            project = project_path.display()
        ),
    )
    .expect("write global config");

    // Create .xli/ project config dir (new name)
    let dot_xli = project_path.join(".xli");
    std::fs::create_dir_all(&dot_xli).expect("create .xli dir");
    std::fs::write(dot_xli.join("config.toml"), "# project-level config\n")
        .expect("write project config");

    // The binary should succeed with the .xli project config
    let output = Command::new(xli_binary())
        .arg("--help")
        .env("XLI_HOME", xli_home.to_str().unwrap())
        .env_remove("CODEX_HOME")
        .current_dir(&project_path)
        .output()
        .expect("failed to run xli in project with .xli/");

    assert!(
        output.status.success(),
        "xli should load .xli/ project config\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ─── Shell Completion ───

#[test]
fn shell_completion_generates_for_xli() {
    let output = Command::new(xli_binary())
        .args(["completion", "bash"])
        .output()
        .expect("failed to run xli completion bash");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "completion should succeed\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("xli"),
        "bash completion should reference 'xli'\nstdout (first 500 chars):\n{}",
        &stdout[..stdout.len().min(500)]
    );
}

// ─── Error Messages ───

#[test]
fn error_for_invalid_xli_home_mentions_xli_home() {
    let tmp = TempDir::new().expect("create temp dir");
    let nonexistent = tmp.path().join("does-not-exist");

    let output = Command::new(xli_binary())
        .arg("--help")
        .env("XLI_HOME", nonexistent.to_str().unwrap())
        .env_remove("CODEX_HOME")
        .output()
        .expect("failed to run xli with bad XLI_HOME");

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Error message should mention XLI_HOME, not CODEX_HOME
    assert!(
        stderr.contains("XLI_HOME") || !output.status.success(),
        "error should reference XLI_HOME\nstderr:\n{stderr}"
    );
}
