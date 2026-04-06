use std::num::NonZero;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use codex_file_search::FileSearchOptions;
use codex_file_search::FileSearchResults;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct FindFilesHandler;

const DEFAULT_LIMIT: usize = 50;

#[derive(Deserialize)]
struct FindFilesArgs {
    pattern: String,
    path: Option<String>,
}

impl ToolHandler for FindFilesHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            payload, turn, ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "find_files handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: FindFilesArgs = parse_arguments(&arguments)?;

        let FindFilesArgs { pattern, path } = args;

        if pattern.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "pattern must not be empty".to_string(),
            ));
        }

        let search_dir = match path {
            Some(p) => {
                let pb = PathBuf::from(&p);
                if !pb.is_absolute() {
                    return Err(FunctionCallError::RespondToModel(
                        "path must be an absolute path".to_string(),
                    ));
                }
                pb
            }
            None => turn.cwd.to_path_buf(),
        };

        if !search_dir.is_dir() {
            return Err(FunctionCallError::RespondToModel(format!(
                "path is not a directory: {}",
                search_dir.display()
            )));
        }

        #[expect(clippy::unwrap_used)]
        let options = FileSearchOptions {
            limit: NonZero::new(DEFAULT_LIMIT).unwrap(),
            exclude: Vec::new(),
            threads: NonZero::new(
                std::thread::available_parallelism()
                    .map(|p| p.get().min(12))
                    .unwrap_or(2),
            )
            .unwrap(),
            compute_indices: false,
            respect_gitignore: true,
        };

        let pattern_clone = pattern.clone();
        let search_dir_clone = search_dir.clone();
        let result = tokio::task::spawn_blocking(move || {
            codex_file_search::run(
                &pattern_clone,
                vec![search_dir_clone],
                options,
                Some(Arc::new(AtomicBool::new(false))),
            )
        })
        .await
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("file search task failed: {err}"))
        })?
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("file search error: {err}"))
        })?;

        let FileSearchResults {
            matches,
            total_match_count,
        } = result;

        let mut output = Vec::with_capacity(matches.len() + 2);
        output.push(format!(
            "Found {total_match_count} file(s) matching \"{pattern}\" in {}",
            search_dir.display()
        ));
        if matches.is_empty() {
            output.push("No matches found.".to_string());
        } else {
            let shown = matches.len();
            for m in &matches {
                let type_label = match m.match_type {
                    codex_file_search::MatchType::File => "",
                    codex_file_search::MatchType::Directory => "/",
                };
                output.push(format!("{}{type_label}", m.path.display()));
            }
            if total_match_count > shown {
                output.push(format!(
                    "... and {} more matches (showing top {shown})",
                    total_match_count - shown
                ));
            }
        }

        Ok(FunctionToolOutput::from_text(output.join("\n"), Some(true)))
    }
}

#[cfg(test)]
#[path = "find_files_tests.rs"]
mod tests;
