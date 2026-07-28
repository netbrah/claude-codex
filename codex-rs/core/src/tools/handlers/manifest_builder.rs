use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use ignore::WalkBuilder;

use super::dir_stats::DirStats;
use super::dir_stats::build_dir_stats;

/// Directory names that are always excluded from the workspace index.
pub const DENYLIST: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".cache",
];

/// Builds a manifest file containing absolute paths to all indexed files.
///
/// Uses a two-pass approach:
/// 1. Count files per directory via [`build_dir_stats`].
/// 2. Walk again, skipping directories whose recursive count exceeds
///    `prune_threshold`.
///
/// The manifest is written atomically (temp file + rename).
pub fn build_manifest(
    workspace_root: &Path,
    output_path: &Path,
    prune_threshold: usize,
) -> anyhow::Result<()> {
    let stats = build_dir_stats(workspace_root, prune_threshold);
    build_manifest_with_stats(workspace_root, output_path, prune_threshold, &stats)
}

pub(super) fn build_manifest_with_stats(
    workspace_root: &Path,
    output_path: &Path,
    prune_threshold: usize,
    stats: &DirStats,
) -> anyhow::Result<()> {
    // Clone stats into an Arc so the 'static filter_entry closure can own it.
    let stats_arc: Arc<DirStats> = Arc::new(stats.clone());
    let workspace_root_buf = workspace_root.to_path_buf();

    let walker = {
        let stats_arc = Arc::clone(&stats_arc);
        let root_buf = workspace_root_buf;
        WalkBuilder::new(workspace_root)
            .git_ignore(true)
            .hidden(false)
            .filter_entry(move |e| {
                let path = e.path();
                let is_dir = e.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                if !is_dir {
                    return true;
                }
                // Never prune the workspace root itself.
                if path == root_buf.as_path() {
                    return true;
                }
                if let Some(name) = e.file_name().to_str()
                    && DENYLIST.contains(&name)
                {
                    return false;
                }
                // Prune directories that exceed the size threshold.
                stats_arc
                    .get(path)
                    .map(|&count| count <= prune_threshold)
                    .unwrap_or(true)
            })
            .build()
    };

    let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
    let tmp_path = parent.join(format!(".manifest.tmp.{}", std::process::id()));

    let mut file = std::fs::File::create(&tmp_path)?;

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        let is_file = entry.file_type().map(|ft| ft.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }
        let abs_path = entry
            .path()
            .canonicalize()
            .unwrap_or_else(|_| entry.path().to_path_buf());
        writeln!(file, "{}", abs_path.display())?;
    }

    file.flush()?;
    drop(file);

    std::fs::rename(&tmp_path, output_path)?;
    Ok(())
}

/// Returns the lines from the manifest that start with the given `scope_prefix`.
pub fn filter_manifest(manifest_path: &Path, scope_prefix: &Path) -> anyhow::Result<Vec<String>> {
    let content = std::fs::read_to_string(manifest_path)?;

    let results = content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }

            let manifest_path = Path::new(trimmed);
            if manifest_path == scope_prefix || manifest_path.starts_with(scope_prefix) {
                Some(line.to_owned())
            } else {
                None
            }
        })
        .collect();

    Ok(results)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn filter_manifest_by_subdir() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("manifest.txt");
        let content = "/workspace/src/foo.rs\n/workspace/src/bar.rs\n/workspace/tests/baz.rs\n/workspace/README.md\n";
        std::fs::write(&manifest_path, content).unwrap();

        let scope = Path::new("/workspace/src");
        let results = filter_manifest(&manifest_path, scope).unwrap();
        assert_eq!(
            results,
            vec![
                "/workspace/src/foo.rs".to_string(),
                "/workspace/src/bar.rs".to_string(),
            ]
        );
    }

    #[test]
    fn pruning_threshold() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let large_dir = root.join("large");
        std::fs::create_dir(&large_dir).unwrap();
        let threshold = 5_usize;
        for i in 0..=threshold {
            std::fs::write(large_dir.join(format!("file{i}.txt")), "").unwrap();
        }

        let small_dir = root.join("small");
        std::fs::create_dir(&small_dir).unwrap();
        std::fs::write(small_dir.join("file.txt"), "").unwrap();

        let manifest_path = root.join("manifest.txt");
        build_manifest(root, &manifest_path, threshold).unwrap();

        let content = std::fs::read_to_string(&manifest_path).unwrap();
        // Files under large_dir (6 files > threshold 5) should be excluded.
        assert!(
            !content.contains(large_dir.to_str().unwrap()),
            "large_dir files should be pruned from manifest"
        );
        // small_dir files should be included.
        assert!(
            content.contains(small_dir.to_str().unwrap()),
            "small_dir files should be in manifest"
        );
    }
}
