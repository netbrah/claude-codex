//! Responses / Gemini wire tool emission helpers (S-LITELLM-BRIDGE-HARDENING).

use crate::ResponsesApiNamespaceTool;
use crate::ResponsesApiTool;
use crate::ToolSpec;
use codex_api::ToolNameMapping;
use codex_api::ensure_openai_strict;
use codex_api::tool_name_mapping;
use codex_api::truncate_tool_name;
use serde_json::Value;

/// Collect tool names as emitted on the OpenAI Responses wire.
pub fn responses_wire_tool_names(tools: &[ToolSpec]) -> Vec<String> {
    let mut names = Vec::new();
    for tool in tools {
        match tool {
            ToolSpec::Function(f) => names.push(f.name.clone()),
            ToolSpec::Namespace(ns) => {
                for inner in &ns.tools {
                    if let ResponsesApiNamespaceTool::Function(f) = inner {
                        names.push(f.name.clone());
                    }
                }
            }
            _ => {}
        }
    }
    names
}

/// Collect tool names as emitted on the Gemini generateContent wire.
pub fn gemini_wire_tool_names(tools: &[ToolSpec]) -> Vec<String> {
    let mut names = Vec::new();
    for tool in tools {
        match tool {
            ToolSpec::Function(f) => names.push(f.name.clone()),
            ToolSpec::Namespace(ns) => {
                for inner in &ns.tools {
                    if let ResponsesApiNamespaceTool::Function(f) = inner {
                        names.push(format!("{}{}", ns.name, f.name));
                    }
                }
            }
            ToolSpec::Freeform(f) => names.push(f.name.clone()),
            _ => {}
        }
    }
    names
}

fn prepare_responses_api_tool(mut tool: ResponsesApiTool) -> Result<ResponsesApiTool, serde_json::Error> {
    tool.name = truncate_tool_name(&tool.name).into_owned();
    let mut params = serde_json::to_value(&tool.parameters)?;
    ensure_openai_strict(&mut params);
    tool.parameters = serde_json::from_value(params)?;
    tool.strict = true;
    Ok(tool)
}

/// Prepare tool JSON for the Responses wire: truncate names, strict-mode schemas.
pub fn prepare_responses_wire_tools(
    tools: &[ToolSpec],
) -> Result<(Vec<Value>, ToolNameMapping), serde_json::Error> {
    let mapping = tool_name_mapping(responses_wire_tool_names(tools).iter().map(String::as_str));
    let mut out = Vec::new();
    for tool in tools {
        let json = match tool {
            ToolSpec::Function(f) => {
                serde_json::to_value(ToolSpec::Function(prepare_responses_api_tool(f.clone())?))?
            }
            ToolSpec::Namespace(ns) => {
                let mut ns = ns.clone();
                for inner in &mut ns.tools {
                    if let ResponsesApiNamespaceTool::Function(f) = inner {
                        *f = prepare_responses_api_tool(f.clone())?;
                    }
                }
                serde_json::to_value(ToolSpec::Namespace(ns))?
            }
            other => serde_json::to_value(other)?,
        };
        out.push(json);
    }
    Ok((out, mapping))
}

#[cfg(test)]
#[path = "responses_wire_tests.rs"]
mod tests;
