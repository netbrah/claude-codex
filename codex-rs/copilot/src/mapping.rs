//! Event mapper: `crate::wire::WireEvent` → `codex_api::ResponseEvent`.
//!
//! The wire layer (`crate::wire::stream_chat`) returns `Vec<WireEvent>`. Each
//! `WireEvent` is one of:
//!
//! * `Copilot(..)` — an event emitted by upstream's frozen `SseReassembler`
//!   (`codex_copilot::ResponseEvent`).
//! * `FinishReasonExtended(String)` — a `finish_reason` the upstream reassembler
//!   drops (`content_filter`, `function_call`, `error`, or any future value
//!   outside upstream's `{tool_calls, stop, length}` allow-list). See
//!   `netbrah/codex-agent@26ff8f1d4967360960d0886aee8462bc56b558ff`,
//!   `codex-copilot/src/sse.rs:88` for the allow-list.
//! * `Usage(WireUsage)` — the final chunk's `usage` object, emitted when
//!   `stream_options.include_usage=true` is set on the request (NB1).
//!
//! # State model
//!
//! The Mapper walks the wire stream in order. Content and tool-call events are
//! translated eagerly. The terminal `Completed { stop_reason, token_usage }`
//! event is built at the end of the walk (see `finalize`) so we can merge
//! information that may arrive in any of three events: upstream's `Done`,
//! the side-channel's `FinishReasonExtended`, and the side-channel's `Usage`.
//!
//! # Finish-reason precedence (F1-revised)
//!
//! If the side-channel saw an extended finish reason, it wins — because that
//! path only fires for values upstream cannot surface. Otherwise we use the
//! reason carried in upstream's `Done(raw)`. Otherwise (no terminal reason
//! seen — should not happen on a well-formed stream) we fall back to
//! `had_tool_call ? tool_use : end_turn` to preserve the pre-F1 heuristic.
//!
//! Reason translation (`OpenAI` → Anthropic, matching claude-codex downstream):
//! * `stop`           → `end_turn`
//! * `tool_calls`     → `tool_use`
//! * `length`         → `max_tokens`
//! * `content_filter` → `content_filter` (pass-through; downstream handles)
//! * `function_call`  → `tool_use` (legacy alias for `tool_calls`)
//! * `error`          → `error`
//! * anything else    → pass-through (unknown extensions bubble up)
//!
//! # Token-usage projection (NB1)
//!
//! `OpenAI`'s `APIUsage` shape is projected onto
//! `codex_protocol::protocol::TokenUsage`:
//! * `input_tokens              ← prompt_tokens`
//! * `output_tokens             ← completion_tokens`
//! * `total_tokens              ← total_tokens`
//! * `cached_input_tokens       ← prompt_tokens_details.cached_tokens (or 0)`
//! * `reasoning_output_tokens   ← completion_tokens_details.reasoning_tokens (or 0)`
//! * `cache_creation_input_tokens ← 0` (`OpenAI` usage object has no equivalent)
//!
//! See `docs/integration/02-design.md` §4.4 in `netbrah/copilot-codex`.

use codex_api::ResponseEvent as ClaudeCodexResponseEvent;
use codex_copilot::ResponseEvent as CopilotResponseEvent;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_api::RawUsage;
use codex_api::normalize_token_usage;
use codex_protocol::protocol::TokenUsage;
use smallvec::SmallVec;

use crate::wire::WireEvent;
use crate::wire::WireUsage;

/// Parse a flat wire tool name back into a structured
/// `(namespace, bare_name)` pair so the tool-router `HashMap` lookup
/// (which keys on the structured `ToolName` shape used at
/// registration time) succeeds.
///
/// Inlined here (not imported from `codex-mcp`) because this crate
/// is the upstream-vendored Copilot adapter and does not take a
/// `codex-mcp` dependency. Keep in sync with
/// `codex_mcp::parse_flat_mcp_tool_name`.
fn parse_flat_mcp_tool_name(name: &str) -> (Option<String>, String) {
    const MCP_PREFIX: &str = "mcp__";
    const DELIM: &str = "__";
    let Some(rest) = name.strip_prefix(MCP_PREFIX) else {
        return (None, name.to_string());
    };
    let Some(idx) = rest.find(DELIM) else {
        return (None, name.to_string());
    };
    let (server, after) = rest.split_at(idx);
    let tool = &after[DELIM.len()..];
    if server.is_empty() || tool.is_empty() {
        return (None, name.to_string());
    }
    (Some(format!("{MCP_PREFIX}{server}")), tool.to_string())
}

/// Pending-state tracker used while walking a wire event vector.
///
/// The `response_id` comes from an external source (adapter-synthesized value);
/// Copilot's SSE payload does not include a stable id we can surface.
#[derive(Debug)]
pub struct Mapper {
    response_id: String,
    had_tool_call: bool,
    /// Set when upstream emits `Done(raw)`. Holds the raw `OpenAI` `finish_reason`
    /// (`stop`, `tool_calls`, `length`).
    pending_upstream_reason: Option<String>,
    /// Set when the side-channel captures a `finish_reason` outside upstream's
    /// allow-list (`content_filter`, `function_call`, `error`, ...).
    pending_extended_reason: Option<String>,
    /// Set when the side-channel captures the final `usage` object.
    pending_usage: Option<WireUsage>,
    /// Guard: we only emit one `Completed` per mapper; finalize is idempotent.
    finalized: bool,
    /// Accumulates assistant text across `ContentDelta`s so we can emit a
    /// committed `OutputItemDone(Message)` at finalize. claude-codex's turn
    /// loop (`core/src/codex.rs`) only persists / renders assistant text from
    /// `OutputItemDone(Message)` — deltas alone are not enough.
    assistant_text: String,
    /// `true` once we've emitted `OutputItemAdded(Message)` for the assistant
    /// message. claude-codex's turn loop `panic!`s on `OutputTextDelta without
    /// active item` (debug) or silently drops the delta (release) unless we
    /// open the item first.
    message_opened: bool,
}

impl Mapper {
    #[must_use]
    pub fn new(response_id: impl Into<String>) -> Self {
        Self {
            response_id: response_id.into(),
            had_tool_call: false,
            pending_upstream_reason: None,
            pending_extended_reason: None,
            pending_usage: None,
            finalized: false,
            assistant_text: String::new(),
            message_opened: false,
        }
    }

    /// Consume one wire event and produce zero-or-more claude-codex events.
    ///
    /// Terminal `Completed` is not emitted here — call `finalize()` at
    /// end-of-stream to flush it. This lets us merge `Done`,
    /// `FinishReasonExtended`, and `Usage` (which can arrive in any order but
    /// typically in that order).
    pub fn step_wire(&mut self, ev: WireEvent) -> SmallVec<[ClaudeCodexResponseEvent; 2]> {
        match ev {
            WireEvent::Copilot(CopilotResponseEvent::ContentDelta(s)) => {
                let mut out = SmallVec::new();
                if !self.message_opened {
                    // First content chunk. Open the assistant message item so
                    // claude-codex's turn loop has an `active_item` before it
                    // tries to route the delta. Content is filled in at
                    // `OutputItemDone` time; `OutputItemAdded` carries the
                    // item shell only.
                    self.message_opened = true;
                    let shell = ResponseItem::Message {
                        id: Some(self.response_id.clone()),
                        role: "assistant".to_string(),
                        content: Vec::new(),
                        phase: None,
                    };
                    out.push(ClaudeCodexResponseEvent::OutputItemAdded(shell));
                }
                self.assistant_text.push_str(&s);
                out.push(ClaudeCodexResponseEvent::OutputTextDelta(s));
                out
            }
            WireEvent::Copilot(CopilotResponseEvent::ToolCall {
                id,
                name,
                arguments,
            }) => {
                self.had_tool_call = true;
                let (namespace, bare_name) = parse_flat_mcp_tool_name(&name);
                let item = ResponseItem::FunctionCall {
                    id: Some(id.clone()),
                    name: bare_name,
                    namespace,
                    arguments,
                    call_id: id,
                };
                let mut out = SmallVec::new();
                out.push(ClaudeCodexResponseEvent::OutputItemAdded(item.clone()));
                out.push(ClaudeCodexResponseEvent::OutputItemDone(item));
                out
            }
            WireEvent::Copilot(CopilotResponseEvent::Done(raw)) => {
                // Buffer; do not emit Completed yet — Usage / extended reason
                // may follow.
                self.pending_upstream_reason = Some(raw);
                SmallVec::new()
            }
            WireEvent::FinishReasonExtended(reason) => {
                // Buffer. Note: the side-channel only surfaces reasons upstream
                // drops (i.e. content_filter / function_call / error), so this
                // arm signals "upstream may emit no Done at all — we own the
                // terminal event".
                self.pending_extended_reason = Some(reason);
                SmallVec::new()
            }
            WireEvent::Usage(u) => {
                self.pending_usage = Some(u);
                SmallVec::new()
            }
        }
    }

    /// Emit the terminal `Completed` event. Idempotent: subsequent calls
    /// return an empty vec. Call once at end-of-stream (after all `step_wire`
    /// calls).
    ///
    /// This MUST be called even if neither `Done` nor `FinishReasonExtended`
    /// was seen — otherwise the turn loop would hang. For that pathological
    /// case we fall back to the pre-F1 heuristic
    /// (`had_tool_call ? tool_use : end_turn`) and emit with
    /// `token_usage=None`.
    pub fn finalize(&mut self) -> SmallVec<[ClaudeCodexResponseEvent; 2]> {
        if self.finalized {
            return SmallVec::new();
        }
        self.finalized = true;

        // Precedence: extended > upstream > heuristic.
        let stop_reason = self
            .pending_extended_reason
            .take()
            .map(|s| translate_finish_reason(&s))
            .or_else(|| {
                self.pending_upstream_reason
                    .take()
                    .map(|s| translate_finish_reason(&s))
            })
            .unwrap_or_else(|| {
                if self.had_tool_call {
                    "tool_use".to_string()
                } else {
                    "end_turn".to_string()
                }
            });

        let token_usage = self.pending_usage.take().map(project_usage);

        let mut out = SmallVec::new();

        // If we opened an assistant message during streaming, close it with
        // the accumulated text before the terminal `Completed`. claude-codex's
        // turn loop persists / renders assistant text exclusively from the
        // `OutputItemDone(Message)` path — without this the TUI sees tokens
        // fly by as deltas but never commits them to history, and
        // `active_item` leaks past turn end.
        let end_turn = !self.had_tool_call;
        if self.message_opened {
            let text = std::mem::take(&mut self.assistant_text);
            let msg = ResponseItem::Message {
                id: Some(self.response_id.clone()),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText { text }],
                phase: None,
            };
            out.push(ClaudeCodexResponseEvent::OutputItemDone(msg));
        }

        out.push(ClaudeCodexResponseEvent::Completed {
            end_turn: if end_turn { Some(true) } else { None },
            stop_reason: Some(stop_reason),
            response_id: std::mem::take(&mut self.response_id),
            token_usage,
        });
        out
    }
}

/// `OpenAI` `finish_reason` → Anthropic `stop_reason` (what claude-codex
/// downstream expects in `ResponseEvent::Completed { stop_reason }`). Unknown
/// values pass through unchanged so new server-side extensions are visible
/// to downstream rather than silently coerced.
fn translate_finish_reason(raw: &str) -> String {
    match raw {
        "stop" => "end_turn".to_string(),
        "tool_calls" => "tool_use".to_string(),
        "length" => "max_tokens".to_string(),
        "content_filter" => "content_filter".to_string(),
        "function_call" => "tool_use".to_string(),
        "error" => "error".to_string(),
        other => other.to_string(),
    }
}

/// Project `OpenAI` `APIUsage` onto `codex_protocol::protocol::TokenUsage`. See
/// module-level docs for the mapping rationale.
fn project_usage(u: WireUsage) -> TokenUsage {
    let cached_input_tokens = u
        .prompt_tokens_details
        .as_ref()
        .map_or(0, |p| p.cached_tokens);
    let reasoning_output_tokens = u
        .completion_tokens_details
        .as_ref()
        .map_or(0, |c| c.reasoning_tokens);
    normalize_token_usage(RawUsage::cache_inclusive_prompt(
        u.prompt_tokens,
        cached_input_tokens,
        0,
        u.completion_tokens,
        reasoning_output_tokens,
        u.total_tokens,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::CompletionTokensDetails;
    use crate::wire::PromptTokensDetails;
    use codex_api::ResponseEvent as E;

    fn copilot(ev: CopilotResponseEvent) -> WireEvent {
        WireEvent::Copilot(ev)
    }

    fn usage_full(
        prompt: i64,
        completion: i64,
        total: i64,
        cached: i64,
        reasoning: i64,
    ) -> WireUsage {
        WireUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: cached,
            }),
            completion_tokens_details: Some(CompletionTokensDetails {
                reasoning_tokens: reasoning,
            }),
        }
    }

    #[test]
    fn content_delta_passes_through() {
        let mut m = Mapper::new("resp-1");
        let out = m.step_wire(copilot(CopilotResponseEvent::ContentDelta("hello".into())));
        // First content delta emits OutputItemAdded(Message) to open an
        // active_item in the turn loop, then the OutputTextDelta itself.
        assert_eq!(out.len(), 2);
        match &out[0] {
            E::OutputItemAdded(ResponseItem::Message { role, .. }) => {
                assert_eq!(role, "assistant");
            }
            other => panic!("expected OutputItemAdded(Message), got {other:?}"),
        }
        match &out[1] {
            E::OutputTextDelta(s) => assert_eq!(s, "hello"),
            other => panic!("expected OutputTextDelta, got {other:?}"),
        }

        // Subsequent deltas do NOT re-open the item.
        let out2 = m.step_wire(copilot(CopilotResponseEvent::ContentDelta(" world".into())));
        assert_eq!(out2.len(), 1);
        match &out2[0] {
            E::OutputTextDelta(s) => assert_eq!(s, " world"),
            other => panic!("expected OutputTextDelta, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_emits_added_then_done() {
        let mut m = Mapper::new("resp-1");
        let out = m.step_wire(copilot(CopilotResponseEvent::ToolCall {
            id: "call_abc".into(),
            name: "apply_patch".into(),
            arguments: r#"{"patch":"..."}"#.into(),
        }));
        assert_eq!(out.len(), 2);
        match (&out[0], &out[1]) {
            (
                E::OutputItemAdded(ResponseItem::FunctionCall {
                    name: n0,
                    call_id: c0,
                    ..
                }),
                E::OutputItemDone(ResponseItem::FunctionCall {
                    name: n1,
                    call_id: c1,
                    arguments,
                    ..
                }),
            ) => {
                assert_eq!(n0, "apply_patch");
                assert_eq!(n1, "apply_patch");
                assert_eq!(c0, "call_abc");
                assert_eq!(c1, "call_abc");
                assert_eq!(arguments, r#"{"patch":"..."}"#);
            }
            _ => panic!("expected FunctionCall Added+Done, got {out:?}"),
        }
    }

    #[test]
    fn done_buffered_no_immediate_completed() {
        let mut m = Mapper::new("resp-1");
        m.step_wire(copilot(CopilotResponseEvent::ContentDelta("hi".into())));
        let out = m.step_wire(copilot(CopilotResponseEvent::Done("stop".into())));
        assert!(out.is_empty(), "Done must not emit Completed eagerly (F1)");
    }

    #[test]
    fn finalize_translates_stop_to_end_turn() {
        let mut m = Mapper::new("resp-1");
        m.step_wire(copilot(CopilotResponseEvent::ContentDelta("hi".into())));
        m.step_wire(copilot(CopilotResponseEvent::Done("stop".into())));
        let out = m.finalize();
        // Content was streamed, so finalize emits the committed Message
        // before Completed.
        assert_eq!(out.len(), 2);
        match &out[0] {
            E::OutputItemDone(ResponseItem::Message { role, content, .. }) => {
                assert_eq!(role, "assistant");
                match content.as_slice() {
                    [ContentItem::OutputText { text }] => assert_eq!(text, "hi"),
                    other => panic!("expected single OutputText, got {other:?}"),
                }
            }
            other => panic!("expected OutputItemDone(Message), got {other:?}"),
        }
        match &out[1] {
            E::Completed {
                stop_reason,
                response_id,
                token_usage,
                ..
            } => {
                assert_eq!(stop_reason.as_deref(), Some("end_turn"));
                assert_eq!(response_id, "resp-1");
                assert!(token_usage.is_none());
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn finalize_translates_tool_calls_to_tool_use() {
        let mut m = Mapper::new("resp-1");
        let _ = m.step_wire(copilot(CopilotResponseEvent::ToolCall {
            id: "c1".into(),
            name: "shell".into(),
            arguments: "{}".into(),
        }));
        m.step_wire(copilot(CopilotResponseEvent::Done("tool_calls".into())));
        let out = m.finalize();
        match &out[0] {
            E::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("tool_use"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn finalize_translates_length_to_max_tokens() {
        let mut m = Mapper::new("resp-1");
        m.step_wire(copilot(CopilotResponseEvent::Done("length".into())));
        let out = m.finalize();
        match &out[0] {
            E::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("max_tokens"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn extended_content_filter_surfaces_verbatim() {
        // Upstream emits no Done; side-channel surfaces the reason alone.
        let mut m = Mapper::new("resp-1");
        m.step_wire(copilot(CopilotResponseEvent::ContentDelta("flag".into())));
        m.step_wire(WireEvent::FinishReasonExtended("content_filter".into()));
        let out = m.finalize();
        // Content streamed → Message commit precedes Completed.
        let completed = out.last().expect("finalize returns at least Completed");
        match completed {
            E::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("content_filter"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn extended_error_surfaces_verbatim() {
        let mut m = Mapper::new("resp-1");
        m.step_wire(WireEvent::FinishReasonExtended("error".into()));
        let out = m.finalize();
        match &out[0] {
            E::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("error"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn extended_function_call_aliases_to_tool_use() {
        let mut m = Mapper::new("resp-1");
        m.step_wire(WireEvent::FinishReasonExtended("function_call".into()));
        let out = m.finalize();
        match &out[0] {
            E::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("tool_use"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn extended_unknown_reason_passes_through() {
        // Forward-compat: unknown OpenAI/CAPI extensions must bubble up
        // unchanged rather than be silently coerced.
        let mut m = Mapper::new("resp-1");
        m.step_wire(WireEvent::FinishReasonExtended("insufficient_quota".into()));
        let out = m.finalize();
        match &out[0] {
            E::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("insufficient_quota"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn extended_wins_over_upstream_done() {
        // Defense-in-depth: if (somehow) both paths emit, the extended reason
        // should win because upstream's allow-list is lossy.
        let mut m = Mapper::new("resp-1");
        m.step_wire(copilot(CopilotResponseEvent::Done("stop".into())));
        m.step_wire(WireEvent::FinishReasonExtended("content_filter".into()));
        let out = m.finalize();
        match &out[0] {
            E::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("content_filter"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn usage_projected_into_token_usage() {
        let mut m = Mapper::new("resp-1");
        m.step_wire(copilot(CopilotResponseEvent::ContentDelta("hi".into())));
        m.step_wire(copilot(CopilotResponseEvent::Done("stop".into())));
        m.step_wire(WireEvent::Usage(usage_full(100, 50, 150, 20, 10)));
        let out = m.finalize();
        let completed = out.last().expect("finalize returns at least Completed");
        match completed {
            E::Completed {
                stop_reason,
                token_usage,
                ..
            } => {
                assert_eq!(stop_reason.as_deref(), Some("end_turn"));
                let tu = token_usage.as_ref().expect("token_usage populated");
                assert_eq!(tu.input_tokens, 100);
                assert_eq!(tu.output_tokens, 50);
                assert_eq!(tu.total_tokens, 150);
                assert_eq!(tu.cached_input_tokens, 20);
                assert_eq!(tu.reasoning_output_tokens, 10);
                assert_eq!(tu.cache_creation_input_tokens, 0);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn usage_missing_details_defaults_to_zero() {
        let mut m = Mapper::new("resp-1");
        m.step_wire(copilot(CopilotResponseEvent::Done("stop".into())));
        m.step_wire(WireEvent::Usage(WireUsage {
            prompt_tokens: 7,
            completion_tokens: 3,
            total_tokens: 10,
            prompt_tokens_details: None,
            completion_tokens_details: None,
        }));
        let out = m.finalize();
        match &out[0] {
            E::Completed { token_usage, .. } => {
                let tu = token_usage.as_ref().expect("token_usage populated");
                assert_eq!(tu.input_tokens, 7);
                assert_eq!(tu.output_tokens, 3);
                assert_eq!(tu.cached_input_tokens, 0);
                assert_eq!(tu.reasoning_output_tokens, 0);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn finalize_without_terminal_falls_back_to_heuristic() {
        // Pathological: upstream emitted Content but no Done, side-channel
        // saw nothing. We still MUST emit Completed or the turn loop hangs.
        let mut m = Mapper::new("resp-1");
        m.step_wire(copilot(CopilotResponseEvent::ContentDelta("hi".into())));
        let out = m.finalize();
        let completed = out.last().expect("finalize returns at least Completed");
        match completed {
            E::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("end_turn"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn finalize_without_terminal_with_tool_is_tool_use() {
        let mut m = Mapper::new("resp-1");
        m.step_wire(copilot(CopilotResponseEvent::ToolCall {
            id: "c1".into(),
            name: "shell".into(),
            arguments: "{}".into(),
        }));
        let out = m.finalize();
        match &out[0] {
            E::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("tool_use"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn finalize_is_idempotent() {
        let mut m = Mapper::new("resp-1");
        m.step_wire(copilot(CopilotResponseEvent::Done("stop".into())));
        let out1 = m.finalize();
        let out2 = m.finalize();
        assert_eq!(out1.len(), 1);
        assert!(out2.is_empty());
    }
}
