use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;
use tokio::time::timeout;

use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::parse_arguments;
use crate::tools::handlers::test_sync_spec::create_grep_files_tool;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSpec;

pub struct GrepFilesHandler;

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 2000;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

fn default_limit() -> usize {
    DEFAULT_LIMIT
}

#[derive(Deserialize)]
struct GrepFilesArgs {
    pattern: String,
    #[serde(default)]
    include: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for GrepFilesHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("grep_files")
    }

    fn spec(&self) -> ToolSpec {
        create_grep_files_tool()
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
                    "grep_files handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: GrepFilesArgs = parse_arguments(&arguments)?;

        let pattern = args.pattern.trim();
        if pattern.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "pattern must not be empty".to_string(),
            ));
        }

        if args.limit == 0 {
            return Err(FunctionCallError::RespondToModel(
                "limit must be greater than zero".to_string(),
            ));
        }

        let limit = args.limit.min(MAX_LIMIT);
        let search_path = turn.resolve_path(args.path.clone());

        verify_path_exists(&search_path).await?;

        let include = args.include.as_deref().map(str::trim).and_then(|val| {
            if val.is_empty() {
                None
            } else {
                Some(val.to_string())
            }
        });

        let search_results =
            run_rg_search(pattern, include.as_deref(), &search_path, limit, &turn.cwd).await?;

        if search_results.is_empty() {
            Ok(boxed_tool_output(FunctionToolOutput::from_text(
                "No matches found.".to_string(),
                Some(false),
            )))
        } else {
            Ok(boxed_tool_output(FunctionToolOutput::from_text(
                search_results.join("\n"),
                Some(true),
            )))
        }
    }
}

impl CoreToolRuntime for GrepFilesHandler {}

async fn verify_path_exists(path: &Path) -> Result<(), FunctionCallError> {
    tokio::fs::metadata(path).await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("unable to access `{}`: {err}", path.display()))
    })?;
    Ok(())
}

async fn run_rg_search(
    pattern: &str,
    include: Option<&str>,
    search_path: &Path,
    limit: usize,
    cwd: &Path,
) -> Result<Vec<String>, FunctionCallError> {
    let mut command = Command::new("rg");
    command
        .current_dir(cwd)
        .arg("--files-with-matches")
        .arg("--sortr=modified")
        .arg("--regexp")
        .arg(pattern)
        .arg("--no-messages");

    if let Some(glob) = include {
        command.arg("--glob").arg(glob);
    }

    command.arg("--").arg(search_path);

    let output = timeout(COMMAND_TIMEOUT, command.output())
        .await
        .map_err(|_| {
            FunctionCallError::RespondToModel("rg timed out after 30 seconds".to_string())
        })?
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to launch rg: {err}. Ensure ripgrep is installed and on PATH."
            ))
        })?;

    match output.status.code() {
        Some(0) => Ok(parse_results(&output.stdout, limit)),
        Some(1) => Ok(Vec::new()),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(FunctionCallError::RespondToModel(format!(
                "rg failed: {stderr}"
            )))
        }
    }
}

fn parse_results(stdout: &[u8], limit: usize) -> Vec<String> {
    let mut results = Vec::new();
    for line in stdout.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(text) = std::str::from_utf8(line) {
            if text.is_empty() {
                continue;
            }
            results.push(text.to_string());
            if results.len() == limit {
                break;
            }
        }
    }
    results
}
