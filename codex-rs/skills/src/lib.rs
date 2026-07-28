use codex_utils_absolute_path::AbsolutePathBuf;
use include_dir::Dir;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::Hash;
use std::hash::Hasher;

use thiserror::Error;

const SYSTEM_SKILLS_DIR: Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/src/assets/samples");

const SYSTEM_SKILLS_DIR_NAME: &str = ".system";
const SKILLS_DIR_NAME: &str = "skills";
const SYSTEM_SKILLS_MARKER_FILENAME: &str = ".codex-system-skills.marker";
const SYSTEM_SKILLS_MARKER_SALT: &str = "v1";

/// Returns the on-disk cache location for embedded system skills from an absolute CODEX_HOME.
pub fn system_cache_root_dir(codex_home: &AbsolutePathBuf) -> AbsolutePathBuf {
    codex_home
        .join(SKILLS_DIR_NAME)
        .join(SYSTEM_SKILLS_DIR_NAME)
}

/// Installs embedded system skills into `CODEX_HOME/skills/.system`.
///
/// Clears any existing system skills directory first and then writes the embedded
/// skills directory into place.
///
/// To avoid doing unnecessary work on every startup, a marker file is written
/// with a fingerprint of the embedded directory. When the marker matches, the
/// install is skipped.
pub fn install_system_skills(codex_home: &AbsolutePathBuf) -> Result<(), SystemSkillsError> {
    install_embedded_skill_bundle(
        codex_home,
        &SYSTEM_SKILLS_DIR,
        SYSTEM_SKILLS_MARKER_FILENAME,
        SYSTEM_SKILLS_MARKER_SALT,
        InstallStrategy::ReplaceDestination,
    )?;

    Ok(())
}

/// How `install_embedded_skill_bundle` treats an existing destination directory.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum InstallStrategy {
    /// Wipe the destination before writing. Used for the upstream system-skills
    /// bundle, which owns the entire `.system/` directory.
    ReplaceDestination,
    /// Leave the destination directory in place and write embedded entries on
    /// top. Used for additive bundles that share the destination with another
    /// bundle (e.g. the Apex overlay sharing `.system/` with system skills).
    OverlayOntoDestination,
}

fn install_embedded_skill_bundle(
    codex_home: &AbsolutePathBuf,
    embedded: &Dir<'_>,
    marker_filename: &str,
    marker_salt: &str,
    strategy: InstallStrategy,
) -> Result<(), SystemSkillsError> {
    let skills_root_dir = codex_home.join(SKILLS_DIR_NAME);
    fs::create_dir_all(skills_root_dir.as_path())
        .map_err(|source| SystemSkillsError::io("create skills root dir", source))?;

    let dest_system = system_cache_root_dir(codex_home);

    let marker_path = dest_system.join(marker_filename);
    let expected_fingerprint = embedded_skills_fingerprint(embedded, marker_salt);
    if dest_system.as_path().is_dir()
        && read_marker(&marker_path).is_ok_and(|marker| marker == expected_fingerprint)
    {
        return Ok(());
    }

    match strategy {
        InstallStrategy::ReplaceDestination => {
            if dest_system.as_path().exists() {
                fs::remove_dir_all(dest_system.as_path()).map_err(|source| {
                    SystemSkillsError::io("remove existing system skills dir", source)
                })?;
            }
        }
        InstallStrategy::OverlayOntoDestination => {
            // Only remove the directories owned by this bundle so a co-tenant
            // bundle (e.g. the cross-platform system skills) isn't clobbered.
            if dest_system.as_path().is_dir() {
                for entry in embedded.entries() {
                    if let include_dir::DirEntry::Dir(subdir) = entry {
                        let path = dest_system.join(subdir.path());
                        if path.as_path().exists() {
                            fs::remove_dir_all(path.as_path()).map_err(|source| {
                                SystemSkillsError::io("remove existing overlay skill dir", source)
                            })?;
                        }
                    }
                }
            }
        }
    }

    write_embedded_dir(embedded, &dest_system)?;
    fs::write(marker_path.as_path(), format!("{expected_fingerprint}\n"))
        .map_err(|source| SystemSkillsError::io("write system skills marker", source))?;
    Ok(())
}

fn read_marker(path: &AbsolutePathBuf) -> Result<String, SystemSkillsError> {
    Ok(fs::read_to_string(path.as_path())
        .map_err(|source| SystemSkillsError::io("read system skills marker", source))?
        .trim()
        .to_string())
}

fn embedded_skills_fingerprint(dir: &Dir<'_>, salt: &str) -> String {
    let mut items = Vec::new();
    collect_fingerprint_items(dir, &mut items);
    items.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));

    let mut hasher = DefaultHasher::new();
    salt.hash(&mut hasher);
    for (path, contents_hash) in items {
        path.hash(&mut hasher);
        contents_hash.hash(&mut hasher);
    }
    format!("{:x}", hasher.finish())
}

fn collect_fingerprint_items(dir: &Dir<'_>, items: &mut Vec<(String, Option<u64>)>) {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(subdir) => {
                items.push((subdir.path().to_string_lossy().to_string(), None));
                collect_fingerprint_items(subdir, items);
            }
            include_dir::DirEntry::File(file) => {
                let mut file_hasher = DefaultHasher::new();
                file.contents().hash(&mut file_hasher);
                items.push((
                    file.path().to_string_lossy().to_string(),
                    Some(file_hasher.finish()),
                ));
            }
        }
    }
}

/// Writes the embedded `include_dir::Dir` to disk under `dest`.
///
/// Preserves the embedded directory structure.
fn write_embedded_dir(dir: &Dir<'_>, dest: &AbsolutePathBuf) -> Result<(), SystemSkillsError> {
    fs::create_dir_all(dest.as_path())
        .map_err(|source| SystemSkillsError::io("create system skills dir", source))?;

    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(subdir) => {
                let subdir_dest = dest.join(subdir.path());
                fs::create_dir_all(subdir_dest.as_path()).map_err(|source| {
                    SystemSkillsError::io("create system skills subdir", source)
                })?;
                write_embedded_dir(subdir, dest)?;
            }
            include_dir::DirEntry::File(file) => {
                let path = dest.join(file.path());
                if let Some(parent) = path.as_path().parent() {
                    fs::create_dir_all(parent).map_err(|source| {
                        SystemSkillsError::io("create system skills file parent", source)
                    })?;
                }
                fs::write(path.as_path(), file.contents())
                    .map_err(|source| SystemSkillsError::io("write system skill file", source))?;
            }
        }
    }

    Ok(())
}

#[derive(Debug, Error)]
pub enum SystemSkillsError {
    #[error("io error while {action}: {source}")]
    Io {
        action: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl SystemSkillsError {
    fn io(action: &'static str, source: std::io::Error) -> Self {
        Self::Io { action, source }
    }
}

#[cfg(test)]
mod tests {
    use super::SYSTEM_SKILLS_DIR;
    use super::collect_fingerprint_items;

    #[test]
    fn fingerprint_traverses_nested_entries() {
        let mut items = Vec::new();
        collect_fingerprint_items(&SYSTEM_SKILLS_DIR, &mut items);
        let mut paths: Vec<String> = items.into_iter().map(|(path, _)| path).collect();
        paths.sort_unstable();

        assert!(
            paths
                .binary_search_by(|probe| probe.as_str().cmp("skill-creator/SKILL.md"))
                .is_ok()
        );
        assert!(
            paths
                .binary_search_by(|probe| probe.as_str().cmp("skill-creator/scripts/init_skill.py"))
                .is_ok()
        );
    }
}
