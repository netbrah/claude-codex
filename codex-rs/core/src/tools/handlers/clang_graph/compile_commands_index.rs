//! Indexed lookup for compile_commands.json.
//!
//! Builds a lightweight in-memory index mapping file paths to their
//! array positions in compile_commands.json.  The index phase only
//! deserializes the `"file"` field of each entry (`MinimalEntry`),
//! keeping peak memory proportional to the number of source files
//! rather than the size of each entry's `arguments` array.
//!
//! Lookups re-read and fully-parse the file on demand; the OS page-cache
//! makes this fast for the common case where compile_commands.json fits
//! in memory.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;

/// Compile arguments for a single translation unit.
#[derive(Debug, Clone)]
pub struct CompileArgs {
    pub directory: PathBuf,
    pub file: PathBuf,
    pub arguments: Vec<String>,
}

/// A single entry in compile_commands.json.
#[derive(Deserialize)]
struct CompileCommandEntry {
    directory: String,
    file: String,
    #[serde(default)]
    arguments: Vec<String>,
    /// Some generators use `command` instead of `arguments`.
    #[serde(default)]
    command: Option<String>,
}

impl CompileCommandEntry {
    fn into_compile_args(self) -> CompileArgs {
        let arguments = if self.arguments.is_empty() {
            // Fall back to shell-tokenizing `command` to handle quoted paths.
            self.command
                .and_then(|cmd| shlex::split(&cmd))
                .unwrap_or_default()
        } else {
            self.arguments
        };
        CompileArgs {
            directory: PathBuf::from(self.directory),
            file: PathBuf::from(self.file),
            arguments,
        }
    }
}

/// Array-index lookup into compile_commands.json.
///
/// Maps canonicalized file paths to their array index in the
/// compile_commands.json array.  During build, only the minimal
/// `"file"` field is deserialized per entry so memory use scales
/// with the number of entries, not the size of their argument lists.
pub struct CompileCommandsIndex {
    db_path: PathBuf,
    /// Canonicalized file path → array index in the JSON array.
    index: HashMap<PathBuf, u64>,
}

impl CompileCommandsIndex {
    /// Build an index by scanning the compile_commands.json file.
    ///
    /// Deserializes only the `"file"` field of each entry during indexing.
    /// The full `arguments`/`command` fields are parsed on demand in
    /// [`Self::get_args`].
    pub fn build(compile_commands_path: &Path) -> Result<Self, String> {
        let data = std::fs::read_to_string(compile_commands_path)
            .map_err(|e| format!("failed to read {}: {e}", compile_commands_path.display()))?;

        let entries: Vec<MinimalEntry> = serde_json::from_str(&data)
            .map_err(|e| format!("failed to parse compile_commands.json: {e}"))?;

        let mut index = HashMap::with_capacity(entries.len());
        for (i, entry) in entries.iter().enumerate() {
            let canonical = normalize_path(Path::new(&entry.file));
            index.insert(canonical, i as u64);
        }

        Ok(Self {
            db_path: compile_commands_path.to_path_buf(),
            index,
        })
    }

    /// Look up compile args for a single file.
    ///
    /// Re-reads and parses the compile_commands.json file; relies on the OS
    /// page-cache for performance on repeated lookups.
    pub fn get_args(&self, file: &Path) -> Option<CompileArgs> {
        let canonical = normalize_path(file);
        let &entry_index = self.index.get(&canonical)?;

        let data = std::fs::read_to_string(&self.db_path).ok()?;
        let entries: Vec<CompileCommandEntry> = serde_json::from_str(&data).ok()?;
        let entry = entries.into_iter().nth(entry_index as usize)?;
        Some(entry.into_compile_args())
    }

    /// Number of files indexed.
    pub fn file_count(&self) -> usize {
        self.index.len()
    }

    /// Check if a file is present in the compilation database.
    pub fn has_file(&self, file: &Path) -> bool {
        let canonical = normalize_path(file);
        self.index.contains_key(&canonical)
    }

    /// Iterate over all indexed file paths.
    pub fn files(&self) -> impl Iterator<Item = &Path> {
        self.index.keys().map(|p| p.as_path())
    }
}

/// Minimal entry: only deserializes the `file` field to minimize memory during indexing.
#[derive(Deserialize)]
struct MinimalEntry {
    file: String,
}

/// Normalize a path for consistent lookup by resolving `.` and `..` via
/// [`std::fs::canonicalize`].  Falls back to the path as-is when the file
/// does not exist on disk (common during indexing of compile DB entries
/// that reference generated files).
///
/// Note: this does **not** perform case-folding.  On case-insensitive
/// filesystems (macOS HFS+/APFS, Windows NTFS) `canonicalize` already
/// returns the on-disk casing which is sufficient for dedup within a
/// single compile DB, but cross-DB lookups with differing casing may
/// miss.
fn normalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_build_and_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let cc_path = dir.path().join("compile_commands.json");

        let content = serde_json::json!([
            {
                "directory": "/build",
                "file": "/src/main.cpp",
                "arguments": ["clang++", "-std=c++17", "-I/include", "-c", "/src/main.cpp"]
            },
            {
                "directory": "/build",
                "file": "/src/foo.cpp",
                "command": "clang++ -std=c++17 -I/include -c /src/foo.cpp"
            }
        ]);
        std::fs::write(&cc_path, content.to_string()).unwrap();

        let index = CompileCommandsIndex::build(&cc_path).unwrap();
        assert_eq!(index.file_count(), 2);

        let args = index.get_args(Path::new("/src/main.cpp")).unwrap();
        assert_eq!(args.directory, PathBuf::from("/build"));
        assert!(args.arguments.contains(&"-std=c++17".to_string()));

        let args2 = index.get_args(Path::new("/src/foo.cpp")).unwrap();
        assert!(!args2.arguments.is_empty());

        assert!(index.get_args(Path::new("/src/nonexistent.cpp")).is_none());
    }

    #[test]
    fn test_command_string_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let cc_path = dir.path().join("compile_commands.json");

        let content = serde_json::json!([
            {
                "directory": "/build",
                "file": "/src/bar.cpp",
                "command": "g++ -O2 -c /src/bar.cpp"
            }
        ]);
        std::fs::write(&cc_path, content.to_string()).unwrap();

        let index = CompileCommandsIndex::build(&cc_path).unwrap();
        let args = index.get_args(Path::new("/src/bar.cpp")).unwrap();
        assert_eq!(args.arguments, vec!["g++", "-O2", "-c", "/src/bar.cpp"]);
    }
}
