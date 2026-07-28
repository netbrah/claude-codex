//! File discovery — walk a scope directory and collect indexable source files.

use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

/// File extensions eligible for indexing (code, structured data, docs).
const EXTENSIONS: &[&str] = &[
    // C/C++
    "c", "cc", "cpp", "cxx", "h", "hh", "hpp",  // Python
    "py",   // Rust
    "rs",   // Go
    "go",   // Java
    "java", // JavaScript / TypeScript
    "js", "jsx", "ts", "tsx", // Shell
    "sh", "bash", // Structured data
    "json", "yaml", "yml", "toml", // Markdown
    "md", "mdx",
];

/// Directories to skip during discovery.
const SKIP_DIRS: &[&str] = &["build", ".git", "node_modules", "bedrock", "third_party"];

/// Maximum directory depth to walk.
const MAX_DEPTH: usize = 8;

/// Maximum number of files to discover per scope.
const MAX_FILES: usize = 10_000;

/// Maximum individual file size to read (1 MB).
pub const MAX_FILE_SIZE: u64 = 1_024 * 1_024;

/// Discover C/C++ source files under a scope directory.
///
/// Returns paths relative to `worktree_root`.
pub fn discover(worktree_root: &Path, scope: &str) -> Vec<String> {
    let scope_dir = worktree_root.join(scope);
    if !scope_dir.is_dir() {
        return Vec::new();
    }

    let exts: HashSet<&str> = EXTENSIONS.iter().copied().collect();
    let skip: HashSet<&str> = SKIP_DIRS.iter().copied().collect();
    let mut found = Vec::new();

    walk_dir(&scope_dir, worktree_root, &exts, &skip, 0, &mut found);

    found
}

fn walk_dir(
    dir: &Path,
    root: &Path,
    exts: &HashSet<&str>,
    skip: &HashSet<&str>,
    depth: usize,
    found: &mut Vec<String>,
) {
    if depth > MAX_DEPTH || found.len() >= MAX_FILES {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_dir() {
            if !skip.contains(name_str.as_ref()) {
                walk_dir(&path, root, exts, skip, depth + 1, found);
            }
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if exts.contains(ext) {
                if let Ok(rel) = path.strip_prefix(root) {
                    found.push(rel.to_string_lossy().to_string());
                    if found.len() >= MAX_FILES {
                        tracing::warn!(
                            max = MAX_FILES,
                            "vector_search: file discovery cap reached, results may be incomplete"
                        );
                        return;
                    }
                }
            }
        }
    }
}

/// Discover files asynchronously using parallel I/O.
///
/// Wraps the synchronous `discover()` in a blocking task.
pub async fn discover_async(worktree_root: PathBuf, scope: String) -> Vec<String> {
    tokio::task::spawn_blocking(move || discover(&worktree_root, &scope))
        .await
        .unwrap_or_default()
}
