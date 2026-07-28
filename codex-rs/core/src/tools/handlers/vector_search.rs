//! Handler for `vector_search` — generic structural similarity search.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::Mutex;

use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use codex_tools::create_vector_search_tool;

use codex_vector_search::AutoChunker;
use codex_vector_search::IndexManager;
use codex_vector_search::SearchMode;
use codex_vector_search::SearchParams;
use codex_vector_search::SearchResult;

pub struct VectorSearchHandler {
    cache_dir: Option<PathBuf>,
    model_cache_dir: Option<PathBuf>,
    index_manager: Arc<Mutex<Option<IndexManager>>>,
}

#[derive(Deserialize)]
struct VectorSearchArgs {
    query: String,
    path: Option<String>,
    limit: Option<usize>,
    mode: Option<String>,
}

impl VectorSearchHandler {
    pub fn new(cache_dir: Option<PathBuf>, model_cache_dir: Option<PathBuf>) -> Self {
        Self {
            cache_dir,
            model_cache_dir,
            index_manager: Arc::new(Mutex::new(None)),
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for VectorSearchHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("vector_search")
    }

    fn spec(&self) -> ToolSpec {
        create_vector_search_tool()
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let ToolInvocation { payload, turn, .. } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "vector_search handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: VectorSearchArgs = parse_arguments(&arguments)?;

        if args.query.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "query must not be empty".to_string(),
            ));
        }

        let mode = match args.mode.as_deref() {
            Some("vector") => SearchMode::Vector,
            Some("fulltext") => SearchMode::Fulltext,
            Some("hybrid") | None => SearchMode::Hybrid,
            Some(other) => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "invalid mode: {other}. Must be one of: hybrid, fulltext, vector"
                )));
            }
        };

        // Determine search directory
        let search_dir = match &args.path {
            Some(p) => {
                let pb = PathBuf::from(p);
                if !pb.is_absolute() {
                    return Err(FunctionCallError::RespondToModel(
                        "path must be an absolute path".to_string(),
                    ));
                }
                if !pb.is_dir() {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "path is not a directory: {p}"
                    )));
                }
                pb
            }
            None => {
                // Try git root for natural project boundary
                codex_vector_search::scope::find_git_root(&turn.cwd)
                    .unwrap_or_else(|| turn.cwd.to_path_buf())
            }
        };

        // Use git root (if available) as the worktree root for scope computation
        let worktree_root = codex_vector_search::scope::find_git_root(&turn.cwd)
            .unwrap_or_else(|| turn.cwd.to_path_buf());
        let scope = normalize_search_scope(&search_dir, &worktree_root)
            .map_err(FunctionCallError::RespondToModel)?;

        let params = SearchParams {
            query: args.query.clone(),
            scope: Some(scope),
            limit: args.limit.unwrap_or(10),
            mode,
        };

        let mut guard = self.index_manager.lock().await;
        let mgr = guard.get_or_insert_with(|| {
            IndexManager::new(
                worktree_root,
                self.cache_dir.clone(),
                self.model_cache_dir.clone(),
                Box::new(AutoChunker::new()),
            )
        });
        let results = mgr
            .search(params)
            .await
            .map_err(|e| FunctionCallError::RespondToModel(format!("search failed: {e}")))?;

        let output = format_results(&args.query, &results);
        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            output,
            Some(true),
        )))
    }
}

impl CoreToolRuntime for VectorSearchHandler {}

fn format_results(query: &str, results: &[SearchResult]) -> String {
    if results.is_empty() {
        return format!("No results found for query: \"{query}\"");
    }

    let mut out = format!("Found {} result(s) for \"{query}\":\n\n", results.len());

    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!(
            "{}. **{}** in `{}` (L{}-L{}, score: {:.3})\n```\n{}\n```\n\n",
            i + 1,
            r.symbol,
            r.file,
            r.start_line,
            r.end_line,
            r.score,
            r.snippet,
        ));
    }

    out
}

/// Compute a search scope (relative to `git_root`) for `requested`.
///
/// - Relative paths are passed through unchanged.
/// - Absolute paths inside `git_root` are stripped to a relative scope
///   (an empty strip — i.e. the root itself — becomes `"."`).
/// - Absolute paths OUTSIDE `git_root` return `Err`, naming both paths,
///   instead of silently collapsing to the current worktree.
fn normalize_search_scope(
    requested: &std::path::Path,
    git_root: &std::path::Path,
) -> Result<String, String> {
    if requested.is_relative() {
        return Ok(requested.to_string_lossy().to_string());
    }
    match requested.strip_prefix(git_root) {
        Ok(rel) => {
            let s = rel.to_string_lossy().to_string();
            Ok(if s.is_empty() { ".".to_string() } else { s })
        }
        Err(_) => Err(format!(
            "requested path {} is outside the resolved git root {};              refusing to silently fall back to the current worktree",
            requested.display(),
            git_root.display(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_search_scope;
    use std::path::Path;
    use std::path::PathBuf;

    #[test]
    fn relative_path_passes_through_unchanged() {
        let scope = normalize_search_scope(Path::new("src/foo"), Path::new("/repo/a"))
            .expect("relative path must succeed");
        assert_eq!(scope, "src/foo");
    }

    #[test]
    fn absolute_path_inside_git_root_strips_to_relative() {
        let root = PathBuf::from("/repo/a");
        let scope = normalize_search_scope(&root.join("src/foo"), &root)
            .expect("inside-root path must succeed");
        assert_eq!(scope, "src/foo");

        let dot = normalize_search_scope(&root, &root).expect("root itself must succeed");
        assert_eq!(dot, ".");
    }

    #[test]
    fn absolute_path_outside_git_root_errors_with_both_paths() {
        let requested = PathBuf::from("/repo/b/src");
        let root = PathBuf::from("/repo/a");
        let err = normalize_search_scope(&requested, &root)
            .expect_err("outside-root path must fail loudly");
        assert!(
            err.contains("/repo/b/src"),
            "error must name requested path; got: {err}"
        );
        assert!(
            err.contains("/repo/a"),
            "error must name git root; got: {err}"
        );
    }
}
