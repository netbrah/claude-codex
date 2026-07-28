//! Shared prompt + response-stream primitives that sit below `codex-core`.
//!
//! Carved out in Sortie 1 (Commit 2a) so that downstream provider crates
//! (`codex-provider-anthropic`, `codex-provider-copilot`, future Gemini) can
//! implement [`codex_model_provider::ModelProvider::stream`] without taking a
//! dependency on `codex-core`. The types here are protocol-shaped — they
//! describe a single model turn and the streamed response back from a wire,
//! independent of orchestration.
//!
//! Public surface re-exported from `codex_core` so existing call sites keep
//! working without churn.

pub use codex_api::ResponseEvent;
use codex_config::types::Personality;
use codex_protocol::error::Result;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::ResponseItem;
use codex_tools::ToolSpec;
use futures::Stream;
use serde::Deserialize;
use std::collections::HashSet;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use tokio::sync::mpsc;

/// API request payload for a single model turn.
#[derive(Debug, Clone)]
pub struct Prompt {
    /// Conversation context input items.
    pub input: Vec<ResponseItem>,

    /// Tools available to the model, including additional tools sourced from
    /// external MCP servers.
    pub tools: Vec<ToolSpec>,

    /// Whether parallel tool calls are permitted for this prompt.
    pub parallel_tool_calls: bool,

    pub base_instructions: BaseInstructions,

    /// Optionally specify the personality of the model.
    pub personality: Option<Personality>,

    /// Optional output schema for the model's response.
    pub output_schema: Option<serde_json::Value>,

    /// Whether the Responses API should strictly validate `output_schema`.
    pub output_schema_strict: bool,
}

impl Default for Prompt {
    fn default() -> Self {
        Self {
            input: Vec::new(),
            tools: Vec::new(),
            parallel_tool_calls: false,
            base_instructions: BaseInstructions::default(),
            personality: None,
            output_schema: None,
            output_schema_strict: true,
        }
    }
}

impl Prompt {
    /// Returns the conversation input adapted for the wire layer.
    ///
    /// When the *Freeform* `apply_patch` tool is present, shell tool outputs
    /// are reserialized as structured text (the Freeform tool expects text,
    /// not JSON). Other tool variants leave the input untouched.
    pub fn get_formatted_input(&self) -> Vec<ResponseItem> {
        let mut input = self.input.clone();

        let is_freeform_apply_patch_tool_present = self.tools.iter().any(|tool| match tool {
            ToolSpec::Freeform(f) => f.name == "apply_patch",
            _ => false,
        });
        if is_freeform_apply_patch_tool_present {
            reserialize_shell_outputs(&mut input);
        }

        input
    }
}

fn reserialize_shell_outputs(items: &mut [ResponseItem]) {
    let mut shell_call_ids: HashSet<String> = HashSet::new();

    items.iter_mut().for_each(|item| match item {
        ResponseItem::LocalShellCall { call_id, id, .. } => {
            if let Some(identifier) = call_id.clone().or_else(|| id.clone()) {
                shell_call_ids.insert(identifier);
            }
        }
        ResponseItem::CustomToolCall {
            id: _,
            status: _,
            call_id,
            name,
            input: _,
        } => {
            if name == "apply_patch" {
                shell_call_ids.insert(call_id.clone());
            }
        }
        ResponseItem::FunctionCall { name, call_id, .. }
            if is_shell_tool_name(name) || name == "apply_patch" =>
        {
            shell_call_ids.insert(call_id.clone());
        }
        ResponseItem::FunctionCallOutput {
            call_id, output, ..
        }
        | ResponseItem::CustomToolCallOutput {
            call_id, output, ..
        } => {
            if shell_call_ids.remove(call_id)
                && let Some(structured) = output
                    .text_content()
                    .and_then(parse_structured_shell_output)
            {
                output.body = FunctionCallOutputBody::Text(structured);
            }
        }
        _ => {}
    })
}

fn is_shell_tool_name(name: &str) -> bool {
    matches!(name, "shell" | "container.exec")
}

#[derive(Deserialize)]
struct ExecOutputJson {
    output: String,
    metadata: ExecOutputMetadataJson,
}

#[derive(Deserialize)]
struct ExecOutputMetadataJson {
    exit_code: i32,
    duration_seconds: f32,
}

fn parse_structured_shell_output(raw: &str) -> Option<String> {
    let parsed: ExecOutputJson = serde_json::from_str(raw).ok()?;
    Some(build_structured_output(&parsed))
}

fn build_structured_output(parsed: &ExecOutputJson) -> String {
    let mut sections = Vec::new();
    sections.push(format!("Exit code: {}", parsed.metadata.exit_code));
    sections.push(format!(
        "Wall time: {} seconds",
        parsed.metadata.duration_seconds
    ));

    let mut output = parsed.output.clone();
    if let Some((stripped, total_lines)) = strip_total_output_header(&parsed.output) {
        sections.push(format!("Total output lines: {total_lines}"));
        output = stripped.to_string();
    }

    sections.push("Output:".to_string());
    sections.push(output);

    sections.join("\n")
}

fn strip_total_output_header(output: &str) -> Option<(&str, u32)> {
    let after_prefix = output.strip_prefix("Total output lines: ")?;
    let (total_segment, remainder) = after_prefix.split_once('\n')?;
    let total_lines = total_segment.parse::<u32>().ok()?;
    let remainder = remainder.strip_prefix('\n').unwrap_or(remainder);
    Some((remainder, total_lines))
}

/// Stream of [`ResponseEvent`]s pumped from a model wire.
pub struct ResponseStream {
    rx_event: mpsc::Receiver<Result<ResponseEvent>>,
}

impl ResponseStream {
    /// Constructs a `ResponseStream` from the receiving half of a channel.
    ///
    /// The transmitting half is owned by the wire-specific stream task that
    /// pumps decoded events into the channel.
    pub fn new(rx_event: mpsc::Receiver<Result<ResponseEvent>>) -> Self {
        Self { rx_event }
    }

    /// Consumes self and returns the inner channel receiver.
    pub fn into_receiver(self) -> mpsc::Receiver<Result<ResponseEvent>> {
        self.rx_event
    }
}

impl Stream for ResponseStream {
    type Item = Result<ResponseEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx_event.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests;
