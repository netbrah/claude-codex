//! Compilation database loader — extracts per-file compile arguments
//! from compile_commands.json using libclang's CompilationDatabase API.

use std::path::Path;
use std::path::PathBuf;

use clang::Clang;
use clang::CompilationDatabase;

/// Per-file compile command resolved from the compilation database.
#[derive(Debug, Clone)]
pub struct FileCompileArgs {
    pub file: PathBuf,
    pub directory: PathBuf,
    pub arguments: Vec<String>,
}

/// Thin wrapper around libclang's CompilationDatabase for extracting
/// individual file compile commands without loading the entire DB into memory.
pub struct CompileDbLoader {
    clang: Clang,
    db_path: PathBuf,
}

impl CompileDbLoader {
    /// Create a new loader pointing at the directory containing
    /// `compile_commands.json`.
    pub fn new(db_directory: impl Into<PathBuf>) -> Result<Self, String> {
        let clang = Clang::new().map_err(|e| format!("failed to initialize libclang: {e}"))?;
        let db_path = db_directory.into();
        // Verify the DB is loadable.
        let _db = CompilationDatabase::from_directory(&db_path)
            .map_err(|_| format!("no compile_commands.json found in {}", db_path.display()))?;
        Ok(Self { clang, db_path })
    }

    /// Returns a reference to the shared Clang instance.
    pub fn clang(&self) -> &Clang {
        &self.clang
    }

    /// Get compile arguments for a single source file.
    ///
    /// Returns `None` if the file has no entry in the compilation database.
    pub fn get_compile_args(&self, file_path: &Path) -> Option<FileCompileArgs> {
        let db = CompilationDatabase::from_directory(&self.db_path).ok()?;
        let cmds = db.get_compile_commands(file_path).ok()?;
        let commands = cmds.get_commands();
        let cmd = commands.first()?;
        Some(FileCompileArgs {
            file: cmd.get_filename(),
            directory: cmd.get_directory(),
            arguments: cmd.get_arguments(),
        })
    }

    /// List all files that have entries in the compilation database.
    ///
    /// Use sparingly on large databases — prefer `get_compile_args` for
    /// individual lookups.
    pub fn all_files(&self) -> Vec<PathBuf> {
        let db = match CompilationDatabase::from_directory(&self.db_path) {
            Ok(db) => db,
            Err(_) => return vec![],
        };
        db.get_all_compile_commands()
            .get_commands()
            .iter()
            .map(|cmd| cmd.get_filename())
            .collect()
    }
}
