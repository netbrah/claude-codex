use std::env;
use std::ffi::OsStr;
use std::path::Path;

use serial_test::serial;
use tempfile::TempDir;

use super::*;

// ── Env var guard ──────────────────────────────────────────────────────

struct EnvVarGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &OsStr) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::set_var(key, value);
        }
        Self { key, original }
    }

    fn unset(key: &'static str) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::remove_var(key);
        }
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.original.take() {
            Some(value) => unsafe {
                env::set_var(self.key, value);
            },
            None => unsafe {
                env::remove_var(self.key);
            },
        }
    }
}

// ── Helper: create a minimal skill on disk ─────────────────────────────

fn create_skill(skills_root: &Path, name: &str, content: &str) {
    let dir = skills_root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), content).unwrap();
}

fn create_skill_with_refs(
    skills_root: &Path,
    name: &str,
    content: &str,
    refs: &[(&str, &str)],
) {
    let dir = skills_root.join(name);
    let refs_dir = dir.join("references");
    std::fs::create_dir_all(&refs_dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), content).unwrap();
    for (ref_name, ref_content) in refs {
        std::fs::write(refs_dir.join(format!("{ref_name}.md")), ref_content).unwrap();
    }
}

fn make_ontap_workspace(tmp: &TempDir) {
    std::fs::write(tmp.path().join("compile_commands.json"), "[]").unwrap();
}

// ── Tests ──────────────────────────────────────────────────────────────

#[test]
fn is_ontap_workspace_true_when_compile_commands_exists() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("compile_commands.json"), "[]").unwrap();
    assert!(is_ontap_workspace(tmp.path()));
}

#[test]
fn is_ontap_workspace_false_when_missing() {
    let tmp = TempDir::new().unwrap();
    assert!(!is_ontap_workspace(tmp.path()));
}

#[tokio::test]
#[serial]
async fn returns_none_when_not_ontap_workspace() {
    let _guard = EnvVarGuard::set("XLI_ONTAP_HARNESS", OsStr::new("auto"));
    let tmp = TempDir::new().unwrap();
    // No compile_commands.json.
    let result = assemble_harness(tmp.path(), None).await;
    assert!(result.is_none());
}

#[tokio::test]
#[serial]
async fn returns_none_when_env_var_is_zero() {
    let _guard = EnvVarGuard::set("XLI_ONTAP_HARNESS", OsStr::new("0"));
    let tmp = TempDir::new().unwrap();
    make_ontap_workspace(&tmp);
    let result = assemble_harness(tmp.path(), None).await;
    assert!(result.is_none());
}

#[tokio::test]
#[serial]
async fn returns_none_when_env_var_is_false() {
    let _guard = EnvVarGuard::set("XLI_ONTAP_HARNESS", OsStr::new("false"));
    let tmp = TempDir::new().unwrap();
    make_ontap_workspace(&tmp);
    let result = assemble_harness(tmp.path(), None).await;
    assert!(result.is_none());
}

#[tokio::test]
#[serial]
async fn returns_none_when_env_var_unset() {
    let _guard = EnvVarGuard::unset("XLI_ONTAP_HARNESS");
    let tmp = TempDir::new().unwrap();
    make_ontap_workspace(&tmp);
    let result = assemble_harness(tmp.path(), None).await;
    assert!(result.is_none());
}

#[tokio::test]
#[serial]
async fn assembles_with_skills_directory() {
    let _guard = EnvVarGuard::set("XLI_ONTAP_HARNESS", OsStr::new("auto"));
    let tmp = TempDir::new().unwrap();
    make_ontap_workspace(&tmp);

    let skills_root = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_root).unwrap();

    // Create two skills that are in the INCLUDE list.
    create_skill(&skills_root, "ontap-rca", "# RCA\nRoot cause analysis.");
    create_skill(&skills_root, "tdd", "# TDD\nTest driven development.");

    let result = assemble_harness(tmp.path(), Some(&skills_root)).await;
    let harness = result.expect("harness should be assembled");

    // Header present.
    assert!(harness.contains("# ONTAP Operational Harness"));
    assert!(harness.contains("## APEX Protocol"));

    // Skills wrapped in XML tags.
    assert!(harness.contains("<skill name=\"ontap-rca\">"));
    assert!(harness.contains("Root cause analysis."));
    assert!(harness.contains("</skill>"));
    assert!(harness.contains("<skill name=\"tdd\">"));
    assert!(harness.contains("Test driven development."));

    // TOC entries.
    assert!(harness.contains("- ontap-rca"));
    assert!(harness.contains("- tdd"));
}

#[tokio::test]
#[serial]
async fn missing_skills_are_skipped_silently() {
    let _guard = EnvVarGuard::set("XLI_ONTAP_HARNESS", OsStr::new("auto"));
    let tmp = TempDir::new().unwrap();
    make_ontap_workspace(&tmp);

    let skills_root = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_root).unwrap();

    // Only create one skill out of the full INCLUDE list.
    create_skill(&skills_root, "git", "# Git\nGit workflow.");

    let result = assemble_harness(tmp.path(), Some(&skills_root)).await;
    let harness = result.expect("harness should be assembled");

    // The one present skill is included.
    assert!(harness.contains("<skill name=\"git\">"));
    // Missing skills are not present.
    assert!(!harness.contains("<skill name=\"ontap-rca\">"));
    assert!(!harness.contains("<skill name=\"tdd\">"));
}

#[tokio::test]
#[serial]
async fn skills_appear_in_kill_chain_order() {
    let _guard = EnvVarGuard::set("XLI_ONTAP_HARNESS", OsStr::new("auto"));
    let tmp = TempDir::new().unwrap();
    make_ontap_workspace(&tmp);

    let skills_root = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_root).unwrap();

    // Create skills in reverse order of how they should appear.
    create_skill(&skills_root, "verification", "# Verification");
    create_skill(&skills_root, "ontap-rca", "# RCA");
    create_skill(&skills_root, "tdd", "# TDD");

    let result = assemble_harness(tmp.path(), Some(&skills_root)).await;
    let harness = result.unwrap();

    // ontap-rca is in Investigation, tdd is in Implementation, verification in Validation.
    let rca_pos = harness.find("<skill name=\"ontap-rca\">").unwrap();
    let tdd_pos = harness.find("<skill name=\"tdd\">").unwrap();
    let ver_pos = harness.find("<skill name=\"verification\">").unwrap();

    assert!(rca_pos < tdd_pos, "ontap-rca should come before tdd");
    assert!(tdd_pos < ver_pos, "tdd should come before verification");
}

#[tokio::test]
#[serial]
async fn references_are_not_injected_into_harness() {
    let _guard = EnvVarGuard::set("XLI_ONTAP_HARNESS", OsStr::new("auto"));
    let tmp = TempDir::new().unwrap();
    make_ontap_workspace(&tmp);

    let skills_root = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_root).unwrap();

    create_skill_with_refs(
        &skills_root,
        "ontap-rca",
        "# RCA skill",
        &[("checklist", "Step 1: gather logs")],
    );

    let result = assemble_harness(tmp.path(), Some(&skills_root)).await;
    let harness = result.unwrap();

    // SKILL.md content is included.
    assert!(harness.contains("# RCA skill"));

    // References are NOT injected — they're available on-demand via skill tool.
    assert!(!harness.contains("<reference"));
    assert!(!harness.contains("Step 1: gather logs"));
    assert!(!harness.contains("ref: checklist"));
}

#[tokio::test]
#[serial]
async fn returns_none_when_no_skills_found() {
    let _guard = EnvVarGuard::set("XLI_ONTAP_HARNESS", OsStr::new("auto"));
    let tmp = TempDir::new().unwrap();
    make_ontap_workspace(&tmp);

    let skills_root = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_root).unwrap();

    // Skills directory exists but has no matching skill names.
    create_skill(&skills_root, "nonexistent-skill", "# Nothing");

    let result = assemble_harness(tmp.path(), Some(&skills_root)).await;
    assert!(result.is_none());
}

#[tokio::test]
#[serial]
async fn env_var_path_overrides_skills_root() {
    let tmp = TempDir::new().unwrap();
    make_ontap_workspace(&tmp);

    let custom_root = tmp.path().join("custom-skills");
    std::fs::create_dir_all(&custom_root).unwrap();
    create_skill(&custom_root, "git", "# Git from custom path");

    // Set env var to the custom path.
    let _guard = EnvVarGuard::set(
        "XLI_ONTAP_HARNESS",
        OsStr::new(custom_root.to_str().unwrap()),
    );

    // Even though we pass a different skills_root, the env var path wins.
    let other_root = tmp.path().join("other-skills");
    std::fs::create_dir_all(&other_root).unwrap();

    let result = assemble_harness(tmp.path(), Some(&other_root)).await;
    let harness = result.unwrap();
    assert!(harness.contains("Git from custom path"));
}
