//! Wire-layer translation with full VS Code Copilot Chat header parity.
//!
//! # Why this module exists
//!
//! Upstream `codex_copilot::CopilotHttpClient::chat_stream_with_auth` is
//! excellent for the happy path and owns invariant 10 (401 retry), but the
//! headers it emits are hardcoded via `CopilotHeaders::new(bearer).into_header_map()`
//! with default values:
//!
//! - `x-initiator: user` (we want `agent` for Codex turns)
//! - `openai-intent: conversation-panel` (we want `conversation-agent`)
//! - *no* `x-interaction-id` (Copilot groups a logical interaction under one ID
//!    for billing/telemetry; omitting it works but is a visible delta vs
//!    `microsoft/vscode-copilot-chat`)
//!
//! To close those three null-space items while remaining ROE-compliant (no
//! edits to `codex-agent`), this module:
//!
//! 1. Builds `CopilotHeaders` via the PUBLIC builder (`with_initiator_agent`,
//!    `with_request_id`) — same struct upstream uses.
//! 2. Adds our two missing headers (`x-interaction-id`, `openai-intent` override)
//!    to the resulting `HeaderMap`.
//! 3. POSTs directly via the already-configured `reqwest::Client`.
//! 4. Runs the response `bytes_stream()` through upstream's public
//!    `SseReassembler` (same parser `chat_stream_inner` uses).
//! 5. Re-implements 401-retry-once to preserve invariant 10. Mirrors
//!    `client.rs::chat_stream_with_auth` line-for-line.
//!
//! # Retry-After honoring (Gap 7 from parity review)
//!
//! On HTTP 402 or 429 we read the `Retry-After` response header and return a
//! typed `WireError::RateLimited { retry_after }`. Callers decide whether to
//! surface it immediately or sleep + retry; the adapter currently surfaces
//! it because our single-turn budget matches the upstream pattern (one
//! 401-retry, no other automatic retries).
//!
//! # Finish-reason fidelity + `token_usage` (F1 + NB1 from parity report v2)
//!
//! Upstream `SseReassembler` (frozen at `26ff8f1d`, `sse.rs:88`) only emits
//! `Done(reason)` when `reason ∈ {tool_calls, stop, length}`. Four legitimate
//! `OpenAI` values are silently dropped: `content_filter`, `function_call`,
//! `error`, and anything else a server-side extension might emit. Worse, when
//! the server emits `finish_reason: content_filter`, the reassembler returns
//! `[]` for that chunk — callers get no terminal event at all, so the turn
//! loop would hang waiting for `Completed`.
//!
//! Upstream `SseReassembler` also ignores the `usage` field entirely (see
//! `sse.rs`'s `StreamChunk` struct — no `usage` member).
//!
//! To close both gaps *without* editing the frozen crate, we run a
//! **side-channel** pass over every raw chunk we feed to upstream's parser.
//! The side-channel looks at each `data: {...}` JSON line for:
//!
//! - `choices[*].finish_reason` in `{content_filter, function_call, error, ..}`
//!   (anything upstream drops) — emits `WireEvent::FinishReasonExtended(..)`.
//! - `usage: { prompt_tokens, completion_tokens, total_tokens, ... }` on the
//!   final chunk (requires `stream_options.include_usage: true` in the
//!   request body) — emits `WireEvent::Usage(..)`.
//!
//! Upstream events are wrapped in `WireEvent::Copilot(..)`. The adapter's
//! `Mapper` merges both side-channels into a single claude-codex
//! `ResponseEvent::Completed { stop_reason, token_usage, .. }`.
//!
//! # What's still in the null-space (intentionally deferred)
//!
//! - **Messages API / Responses API routing**: not yet implemented for
//!   Claude-on-Copilot reasoning workloads. Everything routes through
//!   `/chat/completions`.
//! - **Vision**: image-bearing messages are dropped in `request.rs`; if they
//!   weren't, we'd need `CopilotHeaders::with_vision(true)`.
//!
//! # Tool forwarding (v2+)
//!
//! Tool schemas ARE forwarded in the request body as `OpenAI` chat-completions
//! `tools: [{type:"function", function:{...}}]` plus
//! `tool_choice: "auto"` and `parallel_tool_calls: true`. Shape matches
//! `vscode-copilot-chat`'s `src/platform/endpoint/node/openAIEndpoint.ts`
//! request builder. Translation happens in
//! `request::tools_to_openai_chat(&[ToolSpec])`; the adapter threads the
//! resulting `Vec<serde_json::Value>` into `WireCallOptions::tools`. Tool
//! calls flow back through upstream's `SseReassembler` → our Mapper as
//! `CopilotResponseEvent::ToolCall` → `ResponseItem::FunctionCall`.

use bytes::Bytes;
use codex_copilot::ChatMessage;
use codex_copilot::CopilotAuth;
use codex_copilot::CopilotAuthError;
use codex_copilot::CopilotHeaders;
use codex_copilot::ResponseEvent as CopilotResponseEvent;
use codex_copilot::SseReassembler;
use futures::StreamExt;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderName;
use reqwest::header::HeaderValue;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

/// Intent value written into the `openai-intent` header. Matches
/// `vscode-copilot-chat`'s `locationToIntent(ChatLocation)` mapping. We use
/// `conversation-agent` for every Codex turn because Codex is inherently an
/// agentic CLI.
const OPENAI_INTENT_AGENT: &str = "conversation-agent";

/// Header name for the per-interaction ID. Case-insensitive on the wire; we
/// use lowercase to match upstream `CopilotHeaders::into_header_map`.
const X_INTERACTION_ID: &str = "x-interaction-id";

/// Header name for `X-GitHub-Api-Version`. Lowercase to match upstream's
/// `CopilotHeaders::into_header_map` and HTTP/2 wire convention.
const X_GITHUB_API_VERSION: &str = crate::X_GITHUB_API_VERSION;

/// Value for `X-GitHub-Api-Version` on Copilot LLM-plane requests. See
/// `crate::GITHUB_API_VERSION` for the source-of-truth definition.
const GITHUB_API_VERSION_VSCODE: &str = crate::GITHUB_API_VERSION;

/// Errors from the wire layer. Mapped to `CopilotAdapterError` at the call
/// site in `adapter.rs`.
#[derive(Debug, Error)]
pub enum WireError {
    #[error("copilot auth: {0}")]
    Auth(#[from] CopilotAuthError),

    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("upstream returned {status} for /chat/completions")]
    UpstreamStatus { status: u16 },

    #[error("rate limited by copilot (status {status}); retry after {retry_after:?}")]
    RateLimited {
        status: u16,
        retry_after: Option<Duration>,
    },

    #[error("sse parse: {0}")]
    Sse(String),

    #[error("header build: {0}")]
    HeaderBuild(String),
}

/// Session-scoped identifier for the `x-interaction-id` header. Computed once
/// per process so every turn in a single claude-codex session groups under
/// the same interaction ID (matches `vscode-copilot-chat`'s behavior where
/// one chat view = one interaction).
pub fn session_interaction_id() -> String {
    use std::sync::OnceLock;
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| Uuid::new_v4().to_string()).clone()
}

/// Options for a single chat-completions call. All fields are required —
/// defaults live in the adapter layer.
#[derive(Debug)]
pub struct WireCallOptions<'a> {
    pub model: &'a str,
    pub messages: &'a [ChatMessage],
    pub max_tokens: Option<u32>,
    /// Per-request UUID. Becomes `x-request-id` on the wire.
    pub request_id: String,
    /// Session-scoped UUID. Becomes `x-interaction-id` on the wire.
    pub interaction_id: String,
    /// `OpenAI` chat-completions `tools[]` shape. Built by the adapter via
    /// `request::tools_to_openai_chat(&[ToolSpec])`. Empty slice ⇒ no
    /// `tools` / `tool_choice` / `parallel_tool_calls` emitted on the wire
    /// (matches vscode-copilot-chat's behavior when a turn has no tools).
    pub tools: &'a [serde_json::Value],
}

/// Token-usage snapshot parsed from the final chunk's `usage` object. Mirrors
/// `OpenAI`'s `APIUsage` shape (see `vscode-copilot-chat`'s
/// `src/platform/endpoint/node/openai.ts:27-67`). The adapter's Mapper projects
/// this onto `codex_protocol::protocol::TokenUsage` when emitting `Completed`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct WireUsage {
    /// Tokens in the prompt (including cached).
    #[serde(default)]
    pub prompt_tokens: i64,
    /// Tokens generated in the completion (including reasoning).
    #[serde(default)]
    pub completion_tokens: i64,
    /// `prompt_tokens + completion_tokens`. Server-computed; we trust it.
    #[serde(default)]
    pub total_tokens: i64,
    /// Breakdown of cached tokens in the prompt. Optional — Copilot Proxy
    /// emits it; CAPI may not.
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
    /// Breakdown of reasoning tokens in the completion. Optional.
    #[serde(default)]
    pub completion_tokens_details: Option<CompletionTokensDetails>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct PromptTokensDetails {
    #[serde(default)]
    pub cached_tokens: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CompletionTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: i64,
}

/// Union of (a) events emitted by upstream's frozen `SseReassembler` and (b)
/// the side-channel events we parse ourselves to close F1 / NB1 without
/// editing the frozen crate.
///
/// The `Mapper` (see `mapping.rs`) consumes this stream and produces
/// claude-codex `ResponseEvent`s.
#[derive(Debug, Clone)]
pub enum WireEvent {
    /// Wrapped upstream event (`ContentDelta` / `ToolCall` / Done).
    Copilot(CopilotResponseEvent),
    /// `finish_reason` value that upstream's reassembler drops. Known values
    /// include `content_filter`, `function_call`, `error`. Emitted exactly
    /// once per stream, immediately before or after upstream's `Done` (or
    /// alone, if upstream emits nothing because the reason wasn't in its
    /// allow-list).
    FinishReasonExtended(String),
    /// Parsed `usage` object from the final chunk. Emitted at most once per
    /// stream, typically as the last event.
    Usage(WireUsage),
}

/// Open a streaming chat-completions request with full VS Code Copilot Chat
/// header parity and 401-retry-once semantics. Returns the materialized event
/// vector (same shape as `chat_stream_with_auth`, extended with side-channel
/// events — see `WireEvent`).
///
/// # Invariants preserved
///
/// - Invariant 10 (401 retry): one `force_refresh` + retry; second 401 errors out.
/// - Invariant 9 (session cache): caller owns the `CopilotAuth` + `reqwest::Client`.
///
/// # Invariants NEW in this layer
///
/// - Invariant 11 (x-initiator=agent): Codex turns always set initiator=agent.
/// - Invariant 12 (x-interaction-id): session-scoped UUID emitted on every turn.
/// - Invariant 13 (openai-intent=conversation-agent): overrides upstream's
///   hardcoded `conversation-panel`.
/// - Invariant 14 (Retry-After honoring): 402/429 return a typed error with
///   the parsed retry-after duration; adapter surfaces it to the caller.
/// - Invariant 15 (extended `finish_reason` fidelity): `finish_reason` values
///   upstream's `SseReassembler` drops (`content_filter`, `function_call`,
///   `error`, ...) are surfaced via `WireEvent::FinishReasonExtended`.
/// - Invariant 16 (`token_usage` surfaced): `usage` object on the final chunk
///   is parsed and emitted as `WireEvent::Usage`. Requires
///   `stream_options.include_usage: true` in the request body (set by
///   `build_body`).
pub async fn stream_chat(
    http: &reqwest::Client,
    auth: &mut CopilotAuth,
    opts: WireCallOptions<'_>,
) -> Result<Vec<WireEvent>, WireError> {
    // Retained as a convenience for tests / callers that prefer a
    // materialized vector. Production paths should use
    // `stream_chat_events` to preserve progressive delivery.
    use futures::TryStreamExt;
    let stream = stream_chat_events(http, auth, opts).await?;
    let events: Vec<WireEvent> = stream.try_collect().await?;
    Ok(events)
}

/// Progressive streaming variant of `stream_chat`.
///
/// Returns a stream that yields `WireEvent`s as they arrive on the wire.
/// The 401 retry contract is preserved — the POST is retried with a
/// force-refreshed bearer before the stream begins. Once bytes start
/// flowing, we do not retry (matches `chat_stream_with_auth` semantics).
///
/// The returned stream is `Unpin + Send + 'static` so callers can spawn it
/// into a task and forward through their own channel.
pub async fn stream_chat_events(
    http: &reqwest::Client,
    auth: &mut CopilotAuth,
    opts: WireCallOptions<'_>,
) -> Result<
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<WireEvent, WireError>> + Send + 'static>>,
    WireError,
> {
    // First attempt with the currently-cached JWT.
    let token = auth.token().await?;
    let endpoint_override = auth.endpoints().map(|e| e.api.clone());
    let resp = match post_once(http, &token, endpoint_override.as_deref(), &opts).await {
        Ok(resp) => resp,
        Err(WireError::UpstreamStatus { status: 401 }) => {
            // Stale JWT — force a single refresh and retry exactly once.
            // Mirrors `client.rs::chat_stream_with_auth` line 297.
            auth.cache_mut().force_refresh().await?;
            let token = auth.token().await?;
            let endpoint_override = auth.endpoints().map(|e| e.api.clone());
            match post_once(http, &token, endpoint_override.as_deref(), &opts).await {
                Ok(resp) => resp,
                // Second 401 means the refreshed JWT is also bad.
                Err(WireError::UpstreamStatus { status: 401 }) => {
                    return Err(WireError::Auth(CopilotAuthError::TokenRejected(401)));
                }
                Err(other) => return Err(other),
            }
        }
        Err(other) => return Err(other),
    };

    Ok(Box::pin(wire_event_stream(resp)))
}

/// Perform one POST to `/chat/completions` and return the streaming response
/// if the status is success. Maps 402/429 to `WireError::RateLimited` (with
/// any `Retry-After` parsed) before the body is consumed. Non-success /
/// non-rate-limit statuses surface as `UpstreamStatus`.
async fn post_once(
    http: &reqwest::Client,
    bearer: &str,
    endpoint_override: Option<&str>,
    opts: &WireCallOptions<'_>,
) -> Result<reqwest::Response, WireError> {
    let headers = build_headers(bearer, &opts.request_id, &opts.interaction_id)?;
    let url = chat_url(endpoint_override);
    let body = build_body(opts);

    let resp = http.post(&url).headers(headers).json(&body).send().await?;
    let status = resp.status();

    // Rate-limit / quota paths — surface Retry-After before consuming body.
    // Matches `vscode-copilot-chat::chatMLFetcher.ts:1533` (402 handler) and
    // the common-case 429 path.
    if status.as_u16() == 402 || status.as_u16() == 429 {
        let retry_after = parse_retry_after(resp.headers().get(reqwest::header::RETRY_AFTER));
        return Err(WireError::RateLimited {
            status: status.as_u16(),
            retry_after,
        });
    }

    if !status.is_success() {
        return Err(WireError::UpstreamStatus {
            status: status.as_u16(),
        });
    }

    Ok(resp)
}

/// Build a stream of `WireEvent`s from a successful HTTP response body.
///
/// Events are yielded as soon as `SseReassembler::process_body_chunked`
/// produces them. The side-channel (extended `finish_reason` + usage) is
/// drained after the upstream reassembler's final flush so the Mapper sees
/// events in natural order: `Copilot::Done → FinishReasonExtended → Usage`.
///
/// Read-timeout semantics are owned by the `reqwest::Client` builder (see
/// `adapter::ensure_session`) — a stalled upstream surfaces here as an
/// `Err(reqwest::Error)` wrapped in `WireError::Transport`.
fn wire_event_stream(
    resp: reqwest::Response,
) -> impl futures::Stream<Item = Result<WireEvent, WireError>> + Send + 'static {
    async_stream::try_stream! {
        let mut stream = resp.bytes_stream();
        let mut reassembler = SseReassembler::new();
        let mut side = SideChannel::default();

        while let Some(chunk) = stream.next().await {
            let bytes: Bytes = chunk?;
            let text = std::str::from_utf8(&bytes).unwrap_or_default();

            // Upstream path — emit events AS they are parsed.
            for ev in reassembler.process_body_chunked(text) {
                yield WireEvent::Copilot(ev);
            }

            // Side-channel accumulates state across chunks; no events emitted
            // mid-stream. It fires on `drain()` after the body completes.
            side.ingest(text);
        }

        // End-of-stream: flush upstream's reassembler before the side-channel
        // so downstream Mapper sees terminal events in the expected order.
        for ev in reassembler.flush() {
            yield WireEvent::Copilot(ev);
        }
        for ev in side.drain() {
            yield ev;
        }
    }
}

/// Construct the full header block. Starts from upstream's `CopilotHeaders`
/// builder (so we stay in lockstep with every pinned header it emits) then
/// layers the three null-space additions on top.
fn build_headers(
    bearer: &str,
    request_id: &str,
    interaction_id: &str,
) -> Result<HeaderMap, WireError> {
    let mut map = CopilotHeaders::new(bearer)
        .with_initiator_agent(true)
        .with_request_id(request_id.to_string())
        .into_header_map()
        .map_err(|e| WireError::HeaderBuild(e.to_string()))?;

    // Override `openai-intent` — upstream hardcodes `conversation-panel`.
    map.insert(
        HeaderName::from_static("openai-intent"),
        HeaderValue::from_static(OPENAI_INTENT_AGENT),
    );

    // Override `x-github-api-version` — upstream `codex-copilot` rev
    // `26ff8f1d` pins `2025-04-01`, but real `vscode-copilot-chat` ships
    // `2025-05-01` on every CAPI POST. Match vscode to keep wire parity.
    // See `microsoft/vscode-copilot-chat@9e668cb12144`
    // (`src/platform/networking/common/networking.ts:283`).
    map.insert(
        HeaderName::from_static(X_GITHUB_API_VERSION),
        HeaderValue::from_static(GITHUB_API_VERSION_VSCODE),
    );

    // Add `x-interaction-id` — absent in upstream default headers.
    let iv = HeaderValue::from_str(interaction_id)
        .map_err(|e| WireError::HeaderBuild(format!("{X_INTERACTION_ID}: {e}")))?;
    map.insert(HeaderName::from_static(X_INTERACTION_ID), iv);

    Ok(map)
}

/// Build the `/chat/completions` endpoint URL. Prefers the token-envelope's
/// explicit `endpoints.api` (business/enterprise SKUs), else falls back to the
/// well-known individual endpoint (`api.githubcopilot.com`) — same fallback
/// `CopilotHttpClient::chat_url` uses.
fn chat_url(endpoint_override: Option<&str>) -> String {
    let base = endpoint_override.unwrap_or("https://api.githubcopilot.com");
    format!("{base}/chat/completions")
}

/// Serialize the request body. Shape matches upstream `ChatRequest` (crate-
/// private, reconstructed via shape). Fields:
/// - `model`, `messages`, `stream: true` — always.
/// - `max_tokens` — Some(n) emitted; None omitted (matches upstream's
///   `#[serde(skip_serializing_if = "Option::is_none")]`).
/// - `temperature: 0.0` — always, matches upstream's deterministic default.
/// - `stream_options: { include_usage: true }` — NB1 / invariant 16. Asks
///   the server to emit a final `usage` chunk so we can populate
///   `token_usage` on `Completed`. Zero cost when unused; the side-channel
///   parser quietly ignores streams that don't contain it.
/// - `tools`, `tool_choice`, `parallel_tool_calls` — emitted only when
///   `opts.tools` is non-empty. Matches `vscode-copilot-chat`'s behavior
///   (no tools ⇒ no keys). `tool_choice: "auto"` lets the model decide;
///   `parallel_tool_calls: true` permits multi-tool turns.
fn build_body(opts: &WireCallOptions<'_>) -> serde_json::Value {
    let mut body = json!({
        "model": opts.model,
        "messages": opts.messages,
        "stream": true,
        "temperature": 0.0,
        "stream_options": { "include_usage": true },
    });
    if let Some(max) = opts.max_tokens {
        body["max_tokens"] = json!(max);
    }
    if !opts.tools.is_empty() {
        body["tools"] = json!(opts.tools);
        body["tool_choice"] = json!("auto");
        body["parallel_tool_calls"] = json!(true);
    }
    body
}

/// Parse a `Retry-After` header value. Accepts:
///   - integer seconds (e.g. `"60"`)
///   - HTTP-date (RFC 7231 §7.1.3, e.g. `"Wed, 21 Oct 2015 07:28:00 GMT"`)
///     — returns duration from *now*; clamped to zero if the date is in the past.
///
/// Returns `None` on anything we can't parse.
fn parse_retry_after(h: Option<&HeaderValue>) -> Option<Duration> {
    let raw = h?.to_str().ok()?.trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let parsed = httpdate::parse_http_date(raw).ok()?;
    parsed
        .duration_since(std::time::SystemTime::now())
        .ok()
        .or(Some(Duration::ZERO))
}

// =========================================================================
// Side-channel: extended finish_reason + usage parsing.
// =========================================================================

/// State accumulated while scanning raw SSE chunks for things upstream's
/// frozen `SseReassembler` drops. Stateful across chunk boundaries because
/// a single `data: {...}` line can be split across `bytes_stream` windows.
#[derive(Debug, Default)]
struct SideChannel {
    /// Trailing bytes that didn't end in a newline — carried to the next chunk.
    pending: String,
    /// Captured `finish_reason` value that upstream would drop. Only the *last*
    /// extended `finish_reason` wins (streams emit at most one).
    extended_finish: Option<String>,
    /// Captured `usage` object from any chunk. Only the last one wins (usage
    /// is emitted once, on the final chunk).
    usage: Option<WireUsage>,
}

impl SideChannel {
    /// Feed a raw chunk; update state. Newline-oriented like upstream's parser.
    fn ingest(&mut self, chunk: &str) {
        let mut buf = std::mem::take(&mut self.pending);
        buf.push_str(chunk);
        // Process all complete lines; stash the trailing partial.
        let last_newline = buf.rfind('\n');
        match last_newline {
            Some(end) => {
                let (complete, rest) = buf.split_at(end + 1);
                for line in complete.lines() {
                    self.ingest_line(line);
                }
                self.pending = rest.to_string();
            }
            None => {
                self.pending = buf;
            }
        }
    }

    /// Process one complete SSE line. Skips non-`data:` lines and `[DONE]`.
    fn ingest_line(&mut self, line: &str) {
        let data = match line.strip_prefix("data: ") {
            Some(d) => d.trim(),
            None => return,
        };
        if data == "[DONE]" {
            return;
        }
        // Parse loosely — we only pull two fields. Unknown/extra fields are fine.
        let Ok(chunk): std::result::Result<SideChunk, _> = serde_json::from_str(data) else {
            return;
        };
        if let Some(choices) = chunk.choices {
            for c in choices {
                if let Some(reason) = c.finish_reason {
                    // Upstream's allow-list is {tool_calls, stop, length}.
                    // Any OTHER reason is our job to surface.
                    if !matches!(reason.as_str(), "tool_calls" | "stop" | "length" | "") {
                        self.extended_finish = Some(reason);
                    }
                }
            }
        }
        if let Some(usage) = chunk.usage {
            self.usage = Some(usage);
        }
    }

    /// Emit captured events. Called at end-of-stream. Order is deterministic:
    /// `extended_finish` first, then usage. Either or both may be absent.
    fn drain(&mut self) -> Vec<WireEvent> {
        // Flush any trailing partial line (e.g. a server that didn't emit a
        // final `\n` — unusual but cheap to support).
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            self.ingest_line(&pending);
        }
        let mut out = Vec::new();
        if let Some(reason) = self.extended_finish.take() {
            out.push(WireEvent::FinishReasonExtended(reason));
        }
        if let Some(usage) = self.usage.take() {
            out.push(WireEvent::Usage(usage));
        }
        out
    }
}

/// Minimal shape we need from each chunk. Untyped for forward-compat — the
/// server may add fields, we don't care. `choices` may be absent on the
/// final `usage`-only chunk.
#[derive(Debug, Deserialize)]
struct SideChunk {
    #[serde(default)]
    choices: Option<Vec<SideChoice>>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Debug, Deserialize)]
struct SideChoice {
    #[serde(default)]
    finish_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_url_falls_back_to_api_githubcopilot_com() {
        assert_eq!(
            chat_url(None),
            "https://api.githubcopilot.com/chat/completions"
        );
    }

    #[test]
    fn chat_url_honors_endpoint_override() {
        assert_eq!(
            chat_url(Some("https://api.business.githubcopilot.com")),
            "https://api.business.githubcopilot.com/chat/completions"
        );
    }

    #[test]
    fn build_headers_overrides_intent_and_adds_interaction_id() {
        let map = build_headers("jwt", "req-1", "sess-1").unwrap();
        // Upstream defaults preserved.
        assert!(map.contains_key("authorization"));
        assert!(map.contains_key("editor-version"));
        assert!(map.contains_key("editor-plugin-version"));
        assert!(map.contains_key("copilot-integration-id"));
        // Our overrides/additions.
        assert_eq!(
            map.get("openai-intent").and_then(|v| v.to_str().ok()),
            Some(OPENAI_INTENT_AGENT)
        );
        assert_eq!(
            map.get("x-interaction-id").and_then(|v| v.to_str().ok()),
            Some("sess-1")
        );
        // x-initiator should be "agent" (we called with_initiator_agent(true)).
        assert_eq!(
            map.get("x-initiator").and_then(|v| v.to_str().ok()),
            Some("agent")
        );
        // x-github-api-version must match real vscode-copilot-chat
        // (`networking.ts:283` ships `2025-05-01`), NOT the stale upstream
        // `codex-copilot` constant of `2025-04-01`.
        assert_eq!(
            map.get("x-github-api-version")
                .and_then(|v| v.to_str().ok()),
            Some("2025-05-01"),
            "must override upstream's stale 2025-04-01 to match vscode"
        );
    }

    #[test]
    fn build_body_emits_stream_true_and_temperature_zero() {
        let msgs = vec![ChatMessage::user("ping")];
        let opts = WireCallOptions {
            model: "gpt-4o",
            messages: &msgs,
            max_tokens: Some(2048),
            request_id: "r".into(),
            interaction_id: "i".into(),
            tools: &[],
        };
        let body = build_body(&opts);
        assert_eq!(body["stream"], true);
        assert_eq!(body["temperature"], 0.0);
        assert_eq!(body["max_tokens"], 2048);
        assert_eq!(body["model"], "gpt-4o");
    }

    #[test]
    fn build_body_requests_usage_via_stream_options() {
        // NB1 / invariant 16: stream_options.include_usage must be true so
        // the server emits a final usage chunk we can surface via the
        // side-channel.
        let msgs = vec![ChatMessage::user("ping")];
        let opts = WireCallOptions {
            model: "gpt-4o",
            messages: &msgs,
            max_tokens: None,
            request_id: "r".into(),
            interaction_id: "i".into(),
            tools: &[],
        };
        let body = build_body(&opts);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn build_body_omits_max_tokens_when_none() {
        let msgs = vec![ChatMessage::user("ping")];
        let opts = WireCallOptions {
            model: "gpt-4o",
            messages: &msgs,
            max_tokens: None,
            request_id: "r".into(),
            interaction_id: "i".into(),
            tools: &[],
        };
        let body = build_body(&opts);
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn build_body_omits_tools_when_empty() {
        let msgs = vec![ChatMessage::user("ping")];
        let opts = WireCallOptions {
            model: "gpt-4o",
            messages: &msgs,
            max_tokens: None,
            request_id: "r".into(),
            interaction_id: "i".into(),
            tools: &[],
        };
        let body = build_body(&opts);
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
        assert!(body.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn build_body_includes_tools_tool_choice_and_parallel_when_nonempty() {
        let msgs = vec![ChatMessage::user("ping")];
        let tool = json!({
            "type": "function",
            "function": {
                "name": "shell",
                "description": "run a command",
                "parameters": {"type": "object", "properties": {}}
            }
        });
        let tools = vec![tool];
        let opts = WireCallOptions {
            model: "gpt-4o",
            messages: &msgs,
            max_tokens: None,
            request_id: "r".into(),
            interaction_id: "i".into(),
            tools: &tools,
        };
        let body = build_body(&opts);
        assert!(body["tools"].is_array());
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        assert_eq!(body["tools"][0]["function"]["name"], "shell");
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["parallel_tool_calls"], true);
    }

    #[test]
    fn parse_retry_after_accepts_integer_seconds() {
        let v = HeaderValue::from_static("60");
        assert_eq!(parse_retry_after(Some(&v)), Some(Duration::from_mins(1)));
    }

    #[test]
    fn parse_retry_after_returns_none_on_garbage() {
        let v = HeaderValue::from_static("not-a-number");
        assert_eq!(parse_retry_after(Some(&v)), None);
    }

    #[test]
    fn parse_retry_after_returns_none_on_missing() {
        assert_eq!(parse_retry_after(None), None);
    }

    #[test]
    fn parse_retry_after_accepts_http_date_in_future() {
        // HTTP-date roughly one hour in the future; clamp lower bound to 30s
        // to tolerate wall-clock drift on the test runner.
        let future = std::time::SystemTime::now() + Duration::from_hours(1);
        let header = httpdate::fmt_http_date(future);
        let v = HeaderValue::from_str(&header).expect("valid header bytes");
        let parsed = parse_retry_after(Some(&v)).expect("must accept HTTP-date");
        assert!(
            parsed >= Duration::from_secs(30) && parsed <= Duration::from_secs(3600 + 30),
            "expected ~3600s, got {parsed:?}"
        );
    }

    #[test]
    fn parse_retry_after_clamps_past_http_date_to_zero() {
        let past = std::time::SystemTime::now() - Duration::from_hours(1);
        let header = httpdate::fmt_http_date(past);
        let v = HeaderValue::from_str(&header).expect("valid header bytes");
        assert_eq!(parse_retry_after(Some(&v)), Some(Duration::ZERO));
    }

    #[test]
    fn session_interaction_id_is_stable_across_calls() {
        let a = session_interaction_id();
        let b = session_interaction_id();
        assert_eq!(a, b, "must be session-scoped (OnceLock)");
        assert_eq!(a.len(), 36, "UUID v4 shape");
    }

    // ---- Side-channel tests (F1 + NB1) ----

    #[test]
    fn sidechannel_ignores_happy_path_finish_reasons() {
        // tool_calls / stop / length are upstream's job — we must NOT double-emit.
        let mut sc = SideChannel::default();
        sc.ingest(r#"data: {"choices":[{"finish_reason":"stop","delta":{}}]}"#);
        sc.ingest("\n");
        let out = sc.drain();
        assert!(
            out.is_empty(),
            "expected no side-channel events for stop, got {out:?}"
        );
    }

    #[test]
    fn sidechannel_captures_content_filter_finish() {
        let mut sc = SideChannel::default();
        sc.ingest(r#"data: {"choices":[{"finish_reason":"content_filter","delta":{}}]}"#);
        sc.ingest("\n");
        let out = sc.drain();
        assert!(
            matches!(out.first(), Some(WireEvent::FinishReasonExtended(s)) if s == "content_filter"),
            "got {out:?}"
        );
    }

    #[test]
    fn sidechannel_captures_error_finish() {
        let mut sc = SideChannel::default();
        sc.ingest(r#"data: {"choices":[{"finish_reason":"error","delta":{}}]}"#);
        sc.ingest("\n");
        let out = sc.drain();
        assert!(
            matches!(out.first(), Some(WireEvent::FinishReasonExtended(s)) if s == "error"),
            "got {out:?}"
        );
    }

    #[test]
    fn sidechannel_captures_usage_on_final_chunk() {
        let mut sc = SideChannel::default();
        sc.ingest(r#"data: {"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":34,"total_tokens":46}}"#);
        sc.ingest("\n");
        let out = sc.drain();
        match out.first() {
            Some(WireEvent::Usage(u)) => {
                assert_eq!(u.prompt_tokens, 12);
                assert_eq!(u.completion_tokens, 34);
                assert_eq!(u.total_tokens, 46);
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn sidechannel_captures_usage_with_details() {
        let mut sc = SideChannel::default();
        sc.ingest(
            r#"data: {"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150,"prompt_tokens_details":{"cached_tokens":40},"completion_tokens_details":{"reasoning_tokens":10}}}"#,
        );
        sc.ingest("\n");
        let out = sc.drain();
        match out.first() {
            Some(WireEvent::Usage(u)) => {
                assert_eq!(u.prompt_tokens, 100);
                assert_eq!(u.prompt_tokens_details.as_ref().unwrap().cached_tokens, 40);
                assert_eq!(
                    u.completion_tokens_details
                        .as_ref()
                        .unwrap()
                        .reasoning_tokens,
                    10
                );
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn sidechannel_handles_chunk_split_mid_line() {
        // Split `data: {"choices":[{"finish_reason":"content_filter",...}]}\n`
        // at an arbitrary byte offset. State must carry across.
        let full = r#"data: {"choices":[{"finish_reason":"content_filter","delta":{}}]}"#
            .to_string()
            + "\n";
        let (a, b) = full.split_at(20);
        let mut sc = SideChannel::default();
        sc.ingest(a);
        // Mid-chunk: no newline yet → should not yet have captured.
        sc.ingest(b);
        let out = sc.drain();
        assert!(
            matches!(out.first(), Some(WireEvent::FinishReasonExtended(s)) if s == "content_filter"),
            "got {out:?}"
        );
    }

    #[test]
    fn sidechannel_emits_both_finish_and_usage() {
        // Typical error stream: content_filter on the content chunk, then a
        // terminal usage chunk.
        let mut sc = SideChannel::default();
        sc.ingest(r#"data: {"choices":[{"finish_reason":"content_filter","delta":{}}]}"#);
        sc.ingest("\n");
        sc.ingest(r#"data: {"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":0,"total_tokens":10}}"#);
        sc.ingest("\n");
        sc.ingest("data: [DONE]\n");
        let out = sc.drain();
        assert_eq!(out.len(), 2, "got {out:?}");
        assert!(matches!(&out[0], WireEvent::FinishReasonExtended(s) if s == "content_filter"));
        assert!(matches!(&out[1], WireEvent::Usage(u) if u.prompt_tokens == 10));
    }

    #[test]
    fn sidechannel_ignores_malformed_json_and_non_data_lines() {
        let mut sc = SideChannel::default();
        sc.ingest(": keep-alive\n");
        sc.ingest("data: {not json}\n");
        sc.ingest("data: [DONE]\n");
        let out = sc.drain();
        assert!(out.is_empty(), "expected clean drop, got {out:?}");
    }
}
