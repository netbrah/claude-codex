use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use ignore::WalkBuilder;

use super::manifest_builder::DENYLIST;

/// Maps directory path -> recursive file count.
pub type DirStats = HashMap<PathBuf, usize>;

/// Walks `workspace_root` and computes the recursive file count for every
/// directory encountered. Directories listed in [`DENYLIST`] are skipped.
///
/// When a directory's running count exceeds `prune_threshold` the upward
/// propagation to ancestor directories is stopped, preventing extremely large
/// subtrees from dominating the parent counts.
pub fn build_dir_stats(workspace_root: &Path, prune_threshold: usize) -> DirStats {
    let mut stats: DirStats = HashMap::new();

    let walker = WalkBuilder::new(workspace_root)
        .git_ignore(true)
        .hidden(false)
        .filter_entry(|e| {
            if e.file_type().map(|ft| ft.is_dir()).unwrap_or(false)
                && let Some(name) = e.file_name().to_str()
            {
                return !DENYLIST.contains(&name);
            }
            true
        })
        .build();

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        let is_file = entry.file_type().map(|ft| ft.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }

        let path = entry.path();
        let mut current = path.parent();
        while let Some(dir) = current {
            let count = stats.entry(dir.to_path_buf()).or_insert(0);
            *count += 1;
            if dir == workspace_root {
                break;
            }
            if *count > prune_threshold {
                // Stop propagating to avoid skewing ancestor counts.
                break;
            }
            current = dir.parent();
        }
    }

    stats
}

/// Serializes `stats` to JSON and writes it atomically to `path`.
pub fn save_dir_stats(stats: &DirStats, path: &Path) -> anyhow::Result<()> {
    let json_map: HashMap<String, usize> = stats
        .iter()
        .filter_map(|(k, v)| k.to_str().map(|s| (s.to_string(), *v)))
        .collect();
    let json = serde_json::to_string(&json_map)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp_path = parent.join(format!(".dirstats.tmp.{}", std::process::id()));
    std::fs::write(&tmp_path, &json)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Deserializes [`DirStats`] from a JSON file previously written by
/// [`save_dir_stats`].
pub fn load_dir_stats(path: &Path) -> anyhow::Result<DirStats> {
    let content = std::fs::read_to_string(path)?;
    let json_map: HashMap<String, usize> = serde_json::from_str(&content)?;
    Ok(json_map
        .into_iter()
        .map(|(k, v)| (PathBuf::from(k), v))
        .collect())
}

/// Returns the recursive file count for `scope`, or `0` if not tracked.
pub fn estimate_scope_file_count(stats: &DirStats, scope: &Path) -> usize {
    stats.get(scope).copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn dir_stats_aggregate_counts() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // root/
        //   a.txt
        //   sub/
        //     b.txt
        //     c.txt
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), "").unwrap();
        std::fs::write(root.join("sub").join("b.txt"), "").unwrap();
        std::fs::write(root.join("sub").join("c.txt"), "").unwrap();

        let stats = build_dir_stats(root, usize::MAX);

        assert_eq!(stats.get(root), Some(&3));
        assert_eq!(stats.get(&root.join("sub")), Some(&2));
    }

    #[test]
    fn save_and_load_dir_stats_roundtrip() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("f.txt"), "").unwrap();

        let stats = build_dir_stats(root, usize::MAX);
        let stats_path = root.join("dirstats.json");
        save_dir_stats(&stats, &stats_path).unwrap();

        let loaded = load_dir_stats(&stats_path).unwrap();
        assert_eq!(stats, loaded);
    }
}
