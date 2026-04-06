//! ONTAP single-file build integration.
//!
//! After `apply_patch` modifies files in an ONTAP workspace, this module
//! automatically compiles changed source files to catch syntax/type errors
//! early — before the agent moves on.
//!
//! **Trigger conditions** (all must be true):
//! 1. The modified file has a buildable extension (`.cc`, `.cpp`, `.h`, `.ut`)
//! 2. An ancestor directory contains `Component.py` (ONTAP component root)
//! 3. `XLI_ONTAP_BUILD` is not set to `"0"` or `"false"`
//!
//! When triggered, runs:
//! ```text
//! make -f ./bedrock/Makefile.<subcomponent>.linux64.debug <stem>.o
//! ```
//! in the component directory — exactly what the legacy Python `build.py` did,
//! but integrated into the tool output so the agent sees errors immediately.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;

// ── Configuration ──────────────────────────────────────────────────────

/// Set `XLI_ONTAP_BUILD=0` or `XLI_ONTAP_BUILD=false` to disable.
fn is_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        match std::env::var("XLI_ONTAP_BUILD") {
            Ok(v) if v == "0" || v.eq_ignore_ascii_case("false") => false,
            _ => true,
        }
    })
}

// ── Extension → Make target mapping ────────────────────────────────────

/// Maps file extensions to their bedrock Makefile subcomponent.
/// `.ut` files are unit tests; everything else builds as a shared-lib object.
const EXTENSION_MAP: &[(&str, &str)] = &[
    ("ut",  "utest-l"),
    ("h",   "ulibso-l"),
    ("cc",  "ulibso-l"),
    ("cpp", "ulibso-l"),
];

fn subcomponent_for_extension(ext: &str) -> Option<&'static str> {
    EXTENSION_MAP
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, sub)| *sub)
}

// ── Component discovery ────────────────────────────────────────────────

/// Walk up from `start` looking for a directory containing `Component.py`.
/// Returns `None` if we hit the filesystem root without finding one.
fn find_component_dir(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        if cur.join("Component.py").is_file() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

// ── Build planning ─────────────────────────────────────────────────────

/// A single file that needs building.
#[derive(Debug, Clone)]
struct BuildUnit {
    /// File stem (e.g. `"keymanager_utils"` for `keymanager_utils.cc`).
    stem: String,
    /// Bedrock subcomponent (e.g. `"ulibso-l"`).
    subcomponent: &'static str,
    /// Original full path — for reporting.
    source_path: PathBuf,
}

/// Examine `paths` and group buildable files by their component directory.
/// Non-buildable files are silently skipped.
fn plan_builds(paths: &[PathBuf]) -> HashMap<PathBuf, Vec<BuildUnit>> {
    let mut plan: HashMap<PathBuf, Vec<BuildUnit>> = HashMap::new();

    for path in paths {
        // Extract extension without the dot.
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => continue,
        };

        let subcomponent = match subcomponent_for_extension(ext) {
            Some(s) => s,
            None => continue,
        };

        let parent = match path.parent() {
            Some(d) => d,
            None => continue,
        };

        let component_dir = match find_component_dir(parent) {
            Some(d) => d,
            None => continue,
        };

        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        plan.entry(component_dir).or_default().push(BuildUnit {
            stem,
            subcomponent,
            source_path: path.clone(),
        });
    }

    plan
}

// ── Build execution ────────────────────────────────────────────────────

/// Result of a single file's build attempt.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct BuildResult {
    pub source_path: PathBuf,
    pub component_dir: PathBuf,
    pub success: bool,
    pub output: String,
}

/// Maximum bytes of compiler output to keep per file.
/// Enough for a few screenfuls of errors without flooding the context window.
const MAX_OUTPUT_BYTES: usize = 4_000;

/// Compile a single object file via `make`.
async fn run_make(
    component_dir: &Path,
    unit: &BuildUnit,
) -> BuildResult {
    let makefile = format!("./bedrock/Makefile.{}.linux64.debug", unit.subcomponent);
    let object = format!("{}.o", unit.stem);

    tracing::info!(
        component = %component_dir.display(),
        source    = %unit.source_path.display(),
        makefile  = %makefile,
        object    = %object,
        "ontap_build: compiling",
    );

    let result = tokio::process::Command::new("make")
        .args(["-f", &makefile, &object])
        .current_dir(component_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut combined = String::with_capacity(stdout.len() + stderr.len() + 1);
            combined.push_str(&stdout);
            if !stderr.is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&stderr);
            }

            // Truncate to avoid context-window bloat.
            if combined.len() > MAX_OUTPUT_BYTES {
                let total = combined.len();
                combined.truncate(MAX_OUTPUT_BYTES);
                combined.push_str(&format!(
                    "\n... [truncated — {total} bytes total, showing first {MAX_OUTPUT_BYTES}]"
                ));
            }

            BuildResult {
                source_path: unit.source_path.clone(),
                component_dir: component_dir.to_path_buf(),
                success: output.status.success(),
                output: combined,
            }
        }
        Err(e) => BuildResult {
            source_path: unit.source_path.clone(),
            component_dir: component_dir.to_path_buf(),
            success: false,
            output: format!("Failed to spawn make: {e}"),
        },
    }
}

// ── Public API ─────────────────────────────────────────────────────────

/// Given the list of files just modified by `apply_patch`, compile any
/// that are buildable ONTAP sources.
///
/// Returns `None` when:
/// - The feature is disabled via `XLI_ONTAP_BUILD=0`
/// - No files have a buildable extension
/// - No files reside inside an ONTAP component (no `Component.py` ancestor)
///
/// Otherwise returns a formatted build report suitable for appending to
/// the tool-call output.
pub(crate) async fn build_modified_files(paths: &[PathBuf]) -> Option<String> {
    if !is_enabled() {
        return None;
    }

    let plan = plan_builds(paths);
    if plan.is_empty() {
        return None;
    }

    // Collect total file count for the header.
    let total_files: usize = plan.values().map(|v| v.len()).sum();

    let mut results = Vec::with_capacity(total_files);
    for (component_dir, units) in &plan {
        for unit in units {
            results.push(run_make(component_dir, unit).await);
        }
    }

    let all_passed = results.iter().all(|r| r.success);
    let mut report = String::new();
    report.push_str("\n\n─── ONTAP Build Check ───────────────────────────\n");

    for r in &results {
        let icon = if r.success { "✓" } else { "✗" };
        report.push_str(&format!("{icon} {}\n", r.source_path.display()));
        if !r.success {
            report.push_str(&r.output);
            if !r.output.ends_with('\n') {
                report.push('\n');
            }
        }
    }

    if all_passed {
        report.push_str("All files compiled successfully.\n");
    } else {
        report.push_str(
            "BUILD FAILED — fix the errors above before proceeding.\n",
        );
    }
    report.push_str("─────────────────────────────────────────────────\n");

    Some(report)
}

#[cfg(test)]
#[path = "ontap_build_tests.rs"]
mod tests;
