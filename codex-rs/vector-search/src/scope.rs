//! Scope inference — find a project's component root from a file path.
//!
//! Walks up from a file to find the nearest directory containing both
//! `Component.py` and `src/`, a common convention for component root
//! directories.

use std::path::Path;
use std::path::PathBuf;

/// Walk up from `file` to find the nearest dir with both `Component.py` and `src/`.
///
/// Returns `None` if no such directory is found before reaching `worktree_root`.
pub fn find_component_root(file: &Path, worktree_root: &Path) -> Option<PathBuf> {
    let mut dir = if file.is_dir() { file } else { file.parent()? };

    while dir.starts_with(worktree_root) {
        if dir.join("Component.py").is_file() && dir.join("src").is_dir() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
    None
}

/// Given a component root and the worktree root, return the relative scope path.
pub fn scope_from_component_root(component_root: &Path, worktree_root: &Path) -> Option<String> {
    component_root
        .strip_prefix(worktree_root)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

/// Find the git root by walking up from `start` looking for `.git`.
///
/// Returns `None` if no `.git` is found before reaching the filesystem root.
pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_dir() {
        start
    } else {
        start.parent()?
    };
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn finds_component_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let comp = root.join("security/keymanager");
        std::fs::create_dir_all(comp.join("src")).unwrap();
        std::fs::write(comp.join("Component.py"), "").unwrap();

        let file = comp.join("src/foo.cc");
        std::fs::write(&file, "").unwrap();

        let found = find_component_root(&file, root);
        assert_eq!(found.unwrap(), comp);
    }

    #[test]
    fn returns_none_when_no_component() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("random/file.cc");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "").unwrap();

        let found = find_component_root(&file, tmp.path());
        assert!(found.is_none());
    }

    #[test]
    fn scope_from_root() {
        let root = Path::new("/src/project");
        let comp = Path::new("/src/project/security/keymanager");
        let scope = scope_from_component_root(comp, root);
        assert_eq!(scope.unwrap(), "security/keymanager");
    }

    #[test]
    fn finds_git_root_from_subdir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Create a .git directory at the root
        std::fs::create_dir(root.join(".git")).unwrap();
        // Create a nested subdirectory
        let subdir = root.join("src/deep/nested");
        std::fs::create_dir_all(&subdir).unwrap();

        let found = find_git_root(&subdir);
        assert_eq!(found.unwrap(), root);
    }

    #[test]
    fn finds_git_root_from_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join(".git")).unwrap();
        let file = root.join("src/main.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "").unwrap();

        let found = find_git_root(&file);
        assert_eq!(found.unwrap(), root);
    }

    /// Build a fixture: <root>/.git, <root>/security/keymanager/{Component.py,src/}, and
    /// <root>/random/dir for non-component nested cwd tests.
    fn fixture() -> (TempDir, PathBuf, PathBuf, PathBuf) {
        let tmp = TempDir::new().unwrap();
        // Canonicalize to handle macOS /var -> /private/var symlinks; otherwise
        // starts_with() inside find_component_root() can fail spuriously.
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        let comp = root.join("security/keymanager");
        std::fs::create_dir_all(comp.join("src")).unwrap();
        std::fs::write(comp.join("Component.py"), "").unwrap();
        let nested = comp.join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();
        let non_comp = root.join("random/dir");
        std::fs::create_dir_all(&non_comp).unwrap();
        (tmp, root, comp, nested)
    }

    #[test]
    fn auto_scope_from_repo_root_cwd() {
        // cwd == git root: should resolve to no component (None) since the repo
        // root is not itself a component, but the call must not panic and must
        // succeed when given the real git_root as the worktree boundary.
        let (_tmp, root, _comp, _nested) = fixture();
        let git_root = find_git_root(&root).unwrap();
        assert_eq!(git_root, root);
        let found = find_component_root(&root, &git_root);
        assert!(found.is_none(), "repo root is not a component");
    }

    #[test]
    fn auto_scope_from_nested_cwd_inside_component() {
        // cwd is several levels below the component root; with the real git
        // root threaded through, find_component_root must climb to the
        // component and produce the relative scope.
        let (_tmp, root, comp, nested) = fixture();
        let git_root = find_git_root(&nested).unwrap();
        assert_eq!(git_root, root);
        let found = find_component_root(&nested, &git_root).unwrap();
        assert_eq!(found, comp);
        let scope = scope_from_component_root(&found, &git_root).unwrap();
        assert_eq!(scope, "security/keymanager");
    }

    #[test]
    fn auto_scope_from_non_component_nested_cwd() {
        // cwd is nested but not under any component: must return None.
        let (_tmp, root, _comp, _nested) = fixture();
        let probe = root.join("random/dir");
        let git_root = find_git_root(&probe).unwrap();
        assert_eq!(git_root, root);
        let found = find_component_root(&probe, &git_root);
        assert!(found.is_none());
    }

    #[test]
    fn auto_scope_when_cwd_is_component_root() {
        // Regression for the original behavior: cwd == component root.
        let (_tmp, root, comp, _nested) = fixture();
        let git_root = find_git_root(&comp).unwrap();
        assert_eq!(git_root, root);
        let found = find_component_root(&comp, &git_root).unwrap();
        assert_eq!(found, comp);
        let scope = scope_from_component_root(&found, &git_root).unwrap();
        assert_eq!(scope, "security/keymanager");
    }

    #[test]
    fn returns_none_when_no_git_root() {
        let tmp = TempDir::new().unwrap();
        let subdir = tmp.path().join("some/dir");
        std::fs::create_dir_all(&subdir).unwrap();

        let found = find_git_root(&subdir);
        assert!(found.is_none());
    }
}
