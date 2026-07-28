//! High-level engine wrapping libclang parsing and the call graph.
//!
//! `ClangEngine` provides the operational interface needed by the BFS
//! traversal: parse files on demand, find symbols, extract callees/callers,
//! and validate caller→callee relationships via USR matching.

use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use super::compile_db::CompileDbLoader;
use super::edge_extractor::TuParser;
use super::graph::CallGraph;
use super::graph::FunctionNode;

/// High-level engine for on-demand TU parsing and call graph queries.
///
/// Files are parsed lazily (on first access) and their edges are ingested
/// into a shared `CallGraph`.  This avoids parsing the entire codebase
/// upfront while still building a progressively richer graph as the BFS
/// explores deeper.
pub struct ClangEngine {
    loader: CompileDbLoader,
    graph: CallGraph,
    /// Files whose TU has already been parsed.
    parsed_files: HashSet<PathBuf>,
}

impl ClangEngine {
    /// Create a new engine pointing at the directory containing
    /// `compile_commands.json`.
    pub fn new(compile_db_dir: &Path) -> Result<Self, String> {
        let loader = CompileDbLoader::new(compile_db_dir)?;
        Ok(Self {
            loader,
            graph: CallGraph::new(),
            parsed_files: HashSet::new(),
        })
    }

    /// Parse a file's translation unit and ingest its edges into the graph.
    ///
    /// No-op if the file has already been parsed.
    /// Returns `Ok(true)` if parsing happened, `Ok(false)` if already parsed,
    /// `Err` on parse failure.
    pub fn parse_file(&mut self, file: &Path) -> Result<bool, String> {
        let canonical = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());

        if self.parsed_files.contains(&canonical) {
            return Ok(false);
        }

        let args = self
            .loader
            .get_compile_args(file)
            .ok_or_else(|| format!("no compile command found for {}", file.display()))?;

        let (edges, functions) = TuParser::extract_edges_and_functions(self.loader.clang(), &args)?;

        self.graph.ingest_edges(&edges);
        self.graph.ingest_functions(&functions);
        self.parsed_files.insert(canonical);

        Ok(true)
    }

    /// Parse a file, silently ignoring errors (e.g., missing compile args).
    pub fn try_parse_file(&mut self, file: &Path) -> bool {
        self.parse_file(file).unwrap_or(false)
    }

    /// Find all function nodes whose display name contains the given substring.
    pub fn find_symbol(&self, name: &str) -> Vec<&FunctionNode> {
        self.graph.find_by_name(name)
    }

    /// Find the best matching node for a symbol name.
    ///
    /// Prefers definitions over declarations, and exact matches over substrings.
    pub fn find_best_match(&self, name: &str) -> Option<&FunctionNode> {
        let matches = self.graph.find_by_name(name);
        if matches.is_empty() {
            return None;
        }
        // Exact display name match + is_definition
        matches
            .iter()
            .find(|n| n.display_name == name && n.is_definition)
            .or_else(|| {
                // Exact name, any definition status
                matches.iter().find(|n| n.display_name == name)
            })
            .or_else(|| {
                // Any match that's a definition
                matches.iter().find(|n| n.is_definition)
            })
            .or_else(|| matches.first())
            .copied()
    }

    /// Get direct callees of a function by USR.
    pub fn direct_callees(&self, usr: &str) -> Vec<&FunctionNode> {
        self.graph.direct_callees(usr)
    }

    /// Get direct callers of a function by USR (from already-parsed TUs).
    pub fn direct_callers(&self, usr: &str) -> Vec<&FunctionNode> {
        self.graph.direct_callers(usr)
    }

    /// Validate whether a file contains a call to the function with the
    /// given USR.
    ///
    /// Parses the file's TU if not already parsed, then checks the graph
    /// for an edge from any function in that file to the target USR.
    pub fn validate_caller(
        &mut self,
        caller_file: &Path,
        target_usr: &str,
    ) -> Result<bool, String> {
        // Parse the caller file if needed.
        self.parse_file(caller_file)?;

        // Check: does any function defined in caller_file have an edge to target_usr?
        let callers = self.graph.direct_callers(target_usr);
        let canonical_caller =
            std::fs::canonicalize(caller_file).unwrap_or_else(|_| caller_file.to_path_buf());
        let canonical_str = canonical_caller.to_string_lossy();
        Ok(callers.iter().any(|node| {
            // Compare canonicalized paths to avoid false positives from suffix matching.
            let node_canonical = std::fs::canonicalize(Path::new(&node.file))
                .unwrap_or_else(|_| Path::new(&node.file).to_path_buf());
            node_canonical.to_string_lossy() == canonical_str.as_ref()
        }))
    }

    /// Number of parsed files.
    pub fn parsed_count(&self) -> usize {
        self.parsed_files.len()
    }

    /// Number of function nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of call edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Read-only access to the underlying call graph.
    pub fn graph(&self) -> &CallGraph {
        &self.graph
    }
}
