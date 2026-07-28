use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use serde::Deserialize;
use serde::Serialize;
use sha1::Digest;
use sha1::Sha1;

use super::dir_stats::build_dir_stats;
use super::dir_stats::save_dir_stats;
use super::manifest_builder::build_manifest;

fn default_index_root() -> PathBuf {
    std::env::temp_dir().join("codex-index")
}
const DEFAULT_TTL_SECS: u64 = 1800;
const DEFAULT_PRUNE_DIR_FILES: usize = 150_000;
const DEFAULT_MAX_SCOPE_FILES: usize = 50_000;
const META_VERSION: u32 = 1;

/// Configuration for the workspace file-manifest index, sourced from
/// environment variables with sensible defaults.
#[derive(Clone)]
pub struct WorkspaceIndexConfig {
    /// Root directory where per-workspace index data is stored.
    pub index_root: PathBuf,
    /// How long (in seconds) an index remains valid before being considered stale.
    pub ttl_secs: u64,
    /// Directories with more than this many files are pruned from the manifest.
    pub prune_dir_files: usize,
    /// Maximum file count for a search scope; broader scopes return an error.
    pub max_scope_files: usize,
}

impl WorkspaceIndexConfig {
    /// Constructs a config by reading environment variables, falling back to
    /// compiled-in defaults.
    pub fn from_env() -> Self {
        let index_root = std::env::var("CODEX_INDEX_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_index_root());
        let ttl_secs = std::env::var("CODEX_INDEX_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_TTL_SECS);
        let prune_dir_files = std::env::var("CODEX_INDEX_PRUNE_DIR_FILES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_PRUNE_DIR_FILES);
        let max_scope_files = std::env::var("CODEX_GREP_MAX_SCOPE_FILES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MAX_SCOPE_FILES);
        Self {
            index_root,
            ttl_secs,
            prune_dir_files,
            max_scope_files,
        }
    }
}

/// Returns a short hex string that uniquely identifies a workspace root path.
pub fn workspace_key(workspace_root: &Path) -> String {
    let canonical = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let path_str = canonical.to_string_lossy();
    let mut hasher = Sha1::new();
    hasher.update(path_str.as_bytes());
    let hash = hasher.finalize();
    hash.iter().map(|b| format!("{b:02x}")).collect::<String>()
}

/// Resolved paths for a workspace's index data.
pub struct IndexPaths {
    /// Directory containing all index files for this workspace.
    pub root: PathBuf,
    /// Newline-delimited file containing absolute paths of all indexed files.
    pub manifest: PathBuf,
    /// JSON file mapping directory paths to recursive file counts.
    pub dirstats: PathBuf,
    /// JSON metadata file recording build timestamp and schema version.
    pub meta: PathBuf,
}

/// Returns the [`IndexPaths`] for the given workspace root.
pub fn index_paths(workspace_root: &Path, config: &WorkspaceIndexConfig) -> IndexPaths {
    let key = workspace_key(workspace_root);
    let root = config.index_root.join(&key);
    IndexPaths {
        manifest: root.join("manifest.txt"),
        dirstats: root.join("dirstats.json"),
        meta: root.join("meta.json"),
        root,
    }
}

/// The current state of the workspace index.
#[derive(Debug)]
pub enum IndexStatus {
    /// The index is up-to-date and ready to use.
    Ready {
        manifest_path: PathBuf,
        dirstats_path: PathBuf,
    },
    /// A background build is currently in progress.
    Building,
    /// The index exists but its TTL has expired.
    Stale,
    /// No index has been built yet.
    Unavailable,
}

#[derive(Serialize, Deserialize)]
struct IndexMeta {
    workspace_root: String,
    built_at_secs: u64,
    version: u32,
}

/// Inspects the meta.json file and returns the current [`IndexStatus`].
///
/// Returns [`IndexStatus::Building`] only when the caller knows a lock file
/// exists; this function only checks the meta file.
pub fn check_index_status(workspace_root: &Path, config: &WorkspaceIndexConfig) -> IndexStatus {
    let paths = index_paths(workspace_root, config);

    let content = match std::fs::read_to_string(&paths.meta) {
        Ok(c) => c,
        Err(_) => return IndexStatus::Unavailable,
    };

    let meta: IndexMeta = match serde_json::from_str(&content) {
        Ok(m) => m,
        Err(_) => return IndexStatus::Unavailable,
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if now.saturating_sub(meta.built_at_secs) > config.ttl_secs {
        IndexStatus::Stale
    } else {
        IndexStatus::Ready {
            manifest_path: paths.manifest,
            dirstats_path: paths.dirstats,
        }
    }
}

/// Kicks off a background index build without blocking.
pub fn build_index_in_background(workspace_root: PathBuf, config: WorkspaceIndexConfig) {
    tokio::spawn(async move {
        if let Err(e) = do_build_index(workspace_root, config).await {
            tracing::warn!("failed to build workspace index: {e}");
        }
    });
}

async fn do_build_index(
    workspace_root: PathBuf,
    config: WorkspaceIndexConfig,
) -> anyhow::Result<()> {
    let paths = index_paths(&workspace_root, &config);
    std::fs::create_dir_all(&paths.root)?;

    let lock_path = paths.root.join(".lock");
    // If the lock already exists, another builder is running; treat that as a no-op.
    match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(_) => {
            let result = do_build_index_inner(&workspace_root, &paths, &config).await;
            let _ = std::fs::remove_file(&lock_path);
            result
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Another index build is already in progress for this workspace.
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

async fn do_build_index_inner(
    workspace_root: &Path,
    paths: &IndexPaths,
    config: &WorkspaceIndexConfig,
) -> anyhow::Result<()> {
    let root = workspace_root.to_path_buf();
    let prune = config.prune_dir_files;
    let manifest_path = paths.manifest.clone();
    let dirstats_path = paths.dirstats.clone();

    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let stats = build_dir_stats(&root, prune);
        save_dir_stats(&stats, &dirstats_path)?;
        build_manifest(&root, &manifest_path, prune)?;
        Ok(())
    })
    .await??;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let meta = IndexMeta {
        workspace_root: workspace_root.to_string_lossy().into_owned(),
        built_at_secs: now,
        version: META_VERSION,
    };
    let meta_json = serde_json::to_string(&meta)?;
    let meta_tmp = paths.root.join(".meta.tmp");
    std::fs::write(&meta_tmp, &meta_json)?;
    std::fs::rename(&meta_tmp, &paths.meta)?;

    Ok(())
}

/// Checks the index status and, if necessary, starts a background build.
///
/// Returns [`IndexStatus::Building`] both when a build is already running and
/// when one has just been started.
pub fn ensure_index(workspace_root: &Path, config: &WorkspaceIndexConfig) -> IndexStatus {
    let paths = index_paths(workspace_root, config);
    let lock_path = paths.root.join(".lock");

    if lock_path.exists() {
        return IndexStatus::Building;
    }

    match check_index_status(workspace_root, config) {
        ready @ IndexStatus::Ready { .. } => ready,
        IndexStatus::Building | IndexStatus::Stale | IndexStatus::Unavailable => {
            build_index_in_background(workspace_root.to_path_buf(), config.clone());
            IndexStatus::Building
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn workspace_key_is_stable() {
        let dir = tempdir().unwrap();
        let path = dir.path();

        let key1 = workspace_key(path);
        let key2 = workspace_key(path);
        assert_eq!(key1, key2, "same path must produce the same key");

        let dir2 = tempdir().unwrap();
        let key3 = workspace_key(dir2.path());
        assert_ne!(key1, key3, "different paths must produce different keys");
    }

    #[test]
    fn workspace_key_length_is_40_hex_chars() {
        let dir = tempdir().unwrap();
        let key = workspace_key(dir.path());
        assert_eq!(key.len(), 40, "SHA-1 hex digest must be 40 characters");
        assert!(
            key.chars().all(|c| c.is_ascii_hexdigit()),
            "key must be hex"
        );
    }
}
