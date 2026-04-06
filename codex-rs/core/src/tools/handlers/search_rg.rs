//! Shared ripgrep search helpers used by `analyze_symbol_source` (and
//! any future tool that needs manifest-filtered searches).
//!
//! This module owns the two primitives that avoid directory-traversal /
//! stat storms on NFS:
//!
//! * [`make_scope_tempfile`] — filters the workspace manifest to the
//!   requested scope and writes the result to a [`NamedTempFile`] that can
//!   be passed to `rg --files-from`.
//! * [`run_rg_lines_from_manifest`] — runs `rg` with `--no-heading -n
//!   --files-from <tmp>` and returns `(file, line, content)` triples.
//! * [`run_rg_lines_direct`] — same output shape but falls back to a
//!   plain directory search (used when the index is not yet ready).
//! * [`parse_rg_lines`] — parses the raw bytes from either of the above
//!   `rg` runs into structured triples.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use tempfile::NamedTempFile;
use tokio::process::Command;
use tokio::time::timeout;

use super::manifest_builder::filter_manifest;

pub(super) const COMMAND_TIMEOUT: Duration = Duration::from_secs(200);

/// Filter the workspace manifest to files under `scope_path` and write
/// them to a temporary file suitable for passing to `rg --files-from`.
///
/// Returns `None` when the manifest is unavailable or the filtered list is
/// empty — callers should fall back to a direct directory search in that case.
pub(super) fn make_scope_tempfile(
    manifest_path: &Path,
    scope_path: &Path,
) -> Option<NamedTempFile> {
    // Canonicalize scope_path so it matches the canonicalized absolute paths
    // written by build_manifest. Without this, macOS /var -> /private/var
    // symlinks cause starts_with mismatches.
    let canonical_scope = scope_path.canonicalize().unwrap_or_else(|_| scope_path.to_path_buf());
    let files = filter_manifest(manifest_path, &canonical_scope).ok()?;
    if files.is_empty() {
        return None;
    }
    let list_content = files.join("\n");
    let mut tmp = tempfile::Builder::new()
        .prefix("codex-search-")
        .suffix(".txt")
        .tempfile()
        .ok()?;
    tmp.write_all(list_content.as_bytes()).ok()?;
    tmp.flush().ok()?;
    Some(tmp)
}

/// Run `rg --no-heading -n [--max-count N] --files-from <manifest_path>` and
/// return `(file, line, content)` triples.
///
/// `max_count_per_file` caps rg's `--max-count` flag (useful when you only
/// need one hit per file, e.g. for definition searches).
pub(super) async fn run_rg_lines_from_manifest(
    pattern: &str,
    manifest_path: &Path,
    max_results: usize,
    max_count_per_file: Option<usize>,
    cwd: &Path,
) -> Result<Vec<(String, u32, String)>, String> {
    let mut command = Command::new("rg");
    command
        .current_dir(cwd)
        .arg("--no-heading")
        .arg("-n")
        .arg("--regexp")
        .arg(pattern)
        .arg("--no-messages")
        .arg("--files-from")
        .arg(manifest_path);

    if let Some(max) = max_count_per_file {
        command.arg("--max-count").arg(max.to_string());
    }

    let output = timeout(COMMAND_TIMEOUT, command.output())
        .await
        .map_err(|_| "rg timed out after 30 seconds".to_string())?
        .map_err(|e| format!("failed to launch rg: {e}"))?;

    match output.status.code() {
        Some(0) => Ok(parse_rg_lines(&output.stdout, max_results)),
        Some(1) => Ok(vec![]),
        _ => Err(format!(
            "rg failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )),
    }
}

/// Run `rg --no-heading -n [--max-count N]` directly on `search_path` (no
/// manifest) and return `(file, line, content)` triples.
///
/// This is the fallback path used when the workspace index is not yet ready.
pub(super) async fn run_rg_lines_direct(
    pattern: &str,
    search_path: &Path,
    max_results: usize,
    max_count_per_file: Option<usize>,
) -> Result<Vec<(String, u32, String)>, String> {
    let mut command = Command::new("rg");
    command
        .arg("--no-heading")
        .arg("-n")
        .arg("--regexp")
        .arg(pattern)
        .arg("--no-messages");

    if let Some(max) = max_count_per_file {
        command.arg("--max-count").arg(max.to_string());
    }

    command.arg("--").arg(search_path);

    let output = timeout(COMMAND_TIMEOUT, command.output())
        .await
        .map_err(|_| "rg timed out after 30 seconds".to_string())?
        .map_err(|e| format!("failed to launch rg: {e}"))?;

    match output.status.code() {
        Some(0) => Ok(parse_rg_lines(&output.stdout, max_results)),
        Some(1) => Ok(vec![]),
        _ => Err(format!(
            "rg failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )),
    }
}

/// Parse `filename:lineno:content` lines from ripgrep `--no-heading -n` output.
///
/// Handles Windows drive-letter paths (e.g. `C:\src\file.cpp:12:content`)
/// by detecting the `X:` prefix before splitting on `:`.
pub(super) fn parse_rg_lines(stdout: &[u8], max_results: usize) -> Vec<(String, u32, String)> {
    let mut results = Vec::new();
    for raw in stdout.split(|&b| b == b'\n') {
        if raw.is_empty() {
            continue;
        }
        let Ok(line) = std::str::from_utf8(raw) else {
            continue;
        };

        // On Windows, rg output can start with a drive letter like `C:\...`.
        // Detect that and skip past the drive prefix before splitting on `:`.
        let (file, rest) = if line.len() >= 3
            && line.as_bytes()[0].is_ascii_alphabetic()
            && line.as_bytes()[1] == b':'
            && (line.as_bytes()[2] == b'\\' || line.as_bytes()[2] == b'/')
        {
            // Drive-letter path: split the remainder after the drive prefix.
            let after_drive = &line[2..];
            match after_drive.find(':') {
                Some(pos) => (&line[..2 + pos], &line[2 + pos + 1..]),
                None => continue,
            }
        } else {
            match line.find(':') {
                Some(pos) => (&line[..pos], &line[pos + 1..]),
                None => continue,
            }
        };

        // `rest` is now `lineno:content`.
        let mut parts = rest.splitn(2, ':');
        let (Some(lineno_str), Some(content)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(lineno) = lineno_str.parse::<u32>() else {
            continue;
        };
        results.push((file.to_string(), lineno, content.to_string()));
        if results.len() >= max_results {
            break;
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn parse_rg_lines_basic() {
        let stdout = b"src/lib.rs:10:fn my_func(x: u32) {\nsrc/main.rs:5:my_func(42);\n";
        let results = parse_rg_lines(stdout, 100);
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0],
            (
                "src/lib.rs".to_string(),
                10,
                "fn my_func(x: u32) {".to_string()
            )
        );
        assert_eq!(
            results[1],
            ("src/main.rs".to_string(), 5, "my_func(42);".to_string())
        );
    }

    #[test]
    fn parse_rg_lines_truncates_at_max() {
        let stdout = b"a.rs:1:foo\nb.rs:2:bar\nc.rs:3:baz\n";
        let results = parse_rg_lines(stdout, 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn parse_rg_lines_skips_malformed() {
        // Lines without a valid `file:lineno:content` format should be skipped.
        let stdout = b"only_two_parts\na.rs:not_a_number:content\nb.rs:5:ok\n";
        let results = parse_rg_lines(stdout, 100);
        assert_eq!(results.len(), 1, "only the valid line should be parsed");
        assert_eq!(results[0].0, "b.rs");
    }

    #[test]
    fn parse_rg_lines_empty_input() {
        let results = parse_rg_lines(b"", 100);
        assert_eq!(results, vec![]);
    }

    #[test]
    fn parse_rg_lines_windows_drive_letter() {
        let stdout = b"C:\\src\\file.cpp:12:int main() {\n";
        let results = parse_rg_lines(stdout, 100);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "C:\\src\\file.cpp");
        assert_eq!(results[0].1, 12);
        assert_eq!(results[0].2, "int main() {");
    }
}
