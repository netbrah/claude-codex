//! Locate `extension.mjs` files the Copilot-CLI way.
//!
//! Project extensions live at `<workspace>/.github/extensions/*/extension.mjs`.
//! User-global extensions live under a path the caller provides — the
//! Copilot-CLI default is `~/.copilot/extensions/*/extension.mjs`, but XLI
//! prefers `~/.xli/extensions/` so our host is explicit about the dir.

use std::path::Path;
use std::path::PathBuf;

use thiserror::Error;

/// A discovered extension entry ready to fork.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredExtension {
    /// Logical identifier (the immediate parent directory name).
    pub id: String,
    /// Absolute path to the `extension.mjs` entry point.
    pub entry: PathBuf,
    /// The scope the entry was discovered in.
    pub source: DiscoverySource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoverySource {
    /// `<workspace>/.github/extensions/<id>/extension.mjs`.
    Project,
    /// A user-global extensions dir, typically `~/.xli/extensions`.
    User,
}

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Scan `workspace/.github/extensions` (if present) and `user_extensions_dir`
/// (if Some and present) for `extension.mjs` entries.
pub async fn discover_extensions(
    workspace: &Path,
    user_extensions_dir: Option<&Path>,
) -> Result<Vec<DiscoveredExtension>, DiscoveryError> {
    let mut out = Vec::new();

    let project_dir = workspace.join(".github").join("extensions");
    if dir_exists(&project_dir).await {
        scan_dir(&project_dir, DiscoverySource::Project, &mut out).await?;
    }

    if let Some(user_dir) = user_extensions_dir
        && dir_exists(user_dir).await
    {
        scan_dir(user_dir, DiscoverySource::User, &mut out).await?;
    }

    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

async fn dir_exists(p: &Path) -> bool {
    tokio::fs::metadata(p)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false)
}

async fn scan_dir(
    root: &Path,
    source: DiscoverySource,
    out: &mut Vec<DiscoveredExtension>,
) -> Result<(), DiscoveryError> {
    let mut rd = tokio::fs::read_dir(root)
        .await
        .map_err(|e| DiscoveryError::Io {
            path: root.to_path_buf(),
            source: e,
        })?;

    while let Some(entry) = rd.next_entry().await.map_err(|e| DiscoveryError::Io {
        path: root.to_path_buf(),
        source: e,
    })? {
        let ty = match entry.file_type().await {
            Ok(t) => t,
            Err(_) => continue,
        };
        if !ty.is_dir() {
            continue;
        }
        let entry_mjs = entry.path().join("extension.mjs");
        if tokio::fs::metadata(&entry_mjs)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            let id = entry.file_name().to_string_lossy().into_owned();
            out.push(DiscoveredExtension {
                id,
                entry: entry_mjs,
                source: source.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn finds_project_extensions_and_skips_missing_mjs() {
        let ws = tempdir().unwrap();
        let base = ws.path().join(".github/extensions");
        tokio::fs::create_dir_all(base.join("alpha")).await.unwrap();
        tokio::fs::write(base.join("alpha/extension.mjs"), b"//")
            .await
            .unwrap();
        tokio::fs::create_dir_all(base.join("no-entry"))
            .await
            .unwrap();

        let got = discover_extensions(ws.path(), None).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "alpha");
        assert_eq!(got[0].source, DiscoverySource::Project);
    }

    #[tokio::test]
    async fn merges_user_and_project_sorted() {
        let ws = tempdir().unwrap();
        let user = tempdir().unwrap();
        let pbase = ws.path().join(".github/extensions/zeta");
        tokio::fs::create_dir_all(&pbase).await.unwrap();
        tokio::fs::write(pbase.join("extension.mjs"), b"//")
            .await
            .unwrap();

        let ubase = user.path().join("beta");
        tokio::fs::create_dir_all(&ubase).await.unwrap();
        tokio::fs::write(ubase.join("extension.mjs"), b"//")
            .await
            .unwrap();

        let got = discover_extensions(ws.path(), Some(user.path()))
            .await
            .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "beta");
        assert_eq!(got[0].source, DiscoverySource::User);
        assert_eq!(got[1].id, "zeta");
        assert_eq!(got[1].source, DiscoverySource::Project);
    }

    #[tokio::test]
    async fn empty_when_no_dirs() {
        let ws = tempdir().unwrap();
        let got = discover_extensions(ws.path(), None).await.unwrap();
        assert!(got.is_empty());
    }
}
