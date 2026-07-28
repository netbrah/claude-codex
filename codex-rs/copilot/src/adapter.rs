//! Adapter glue: TTY guard, TOS banner, cached `CopilotAuth` +
//! `CopilotHttpClient`, and the public `stream(...)` entry point that
//! `codex-rs/core/src/client.rs::stream_copilot_api` calls.
//!
//! # Statefulness
//!
//! The adapter holds a process-scoped `OnceCell`-style handle to a
//! `SessionState { auth, http, banner_printed }`. Caching the `CopilotAuth`
//! + `CopilotHttpClient` across turns is required by invariant 9 —
//! `codex_copilot::client`'s `slow_down` +5s back-off arithmetic is only
//! meaningful if the same client instance sees successive turns. A fresh
//! client per turn would reset the back-off clock and regress the baseline.
//!
//! # Error model
//!
//! Every path that can fail maps to `CopilotAdapterError`. `?` with
//! `codex_copilot::CopilotAuthError` converts via the `#[from]` in `lib.rs`.
//! No `anyhow` anywhere (invariant 2).
//!
//! # TOS banner
//!
//! `codex_copilot::print_tos_banner()` is invoked at most once per process
//! (guarded by an `AtomicBool`). The banner fires on the first successful
//! `stream(...)` call, not on `ModelClient::new` — the user can create a
//! provider pointed at Copilot without seeing the banner until they actually
//! attempt a Copilot turn (invariant 3).

use std::env;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use codex_api::ResponseEvent;
use codex_copilot::CopilotAuth;
use codex_copilot::discover_github_token;
use codex_copilot::is_interactive_tty;
use codex_copilot::print_tos_banner;
use codex_protocol::models::ResponseItem;
use codex_tools::ToolSpec;
use futures::Stream;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::CopilotAdapterError;
use crate::Result;
use crate::mapping::Mapper;
use crate::request::items_to_chat_messages;
use crate::request::tools_to_openai_chat;
use crate::request::unsupported_tool_count;
use crate::wire::WireCallOptions;
use crate::wire::{self};

/// Max tokens requested from Copilot per turn. Copilot's own clients send a
/// similar cap (vscode-chat uses 4096-8192). We default mid-range; callers can
/// override via `CODEX_COPILOT_MAX_TOKENS`. Setting `0` disables the cap.
const DEFAULT_MAX_TOKENS: u32 = 8192;

/// Env var that lets users bypass `is_interactive_tty()` when running under
/// automation (CI, IDE integrations). Mirrors
/// `CopilotConfig::force_allow_headless` but on the adapter side — we want
/// the error surface at the first `stream(...)` call, not the first
/// `CopilotAuth::init`.
const HEADLESS_OVERRIDE_ENV: &str = "CODEX_COPILOT_ALLOW_HEADLESS";

/// Env var to override the max-tokens cap. Malformed values fall back to
/// `DEFAULT_MAX_TOKENS` (`tracing::warn`) rather than erroring — we never want
/// a misconfigured env var to block a turn.
const MAX_TOKENS_ENV: &str = "CODEX_COPILOT_MAX_TOKENS";

/// The session-scoped state held across turns. `auth` and `http_inner` are
/// the authoritative cached instances (invariant 9). See module docs.
///
/// Note: v2 of the adapter stopped carrying a `CopilotHttpClient` here — the
/// wire layer (`crate::wire`) POSTs through the raw `reqwest::Client`
/// directly so it can inject our parity headers (`x-initiator=agent`,
/// `x-interaction-id`, `openai-intent=conversation-agent`) without bouncing
/// through `chat_stream_with_auth`'s hardcoded header builder. Caching the
/// `reqwest::Client` still preserves TCP pool + HTTP/2 session reuse across
/// turns (the critical half of invariant 9); the `slow_down` backoff state
/// that `CopilotHttpClient` owned is not consulted on the `/chat/completions`
/// path we're replacing.
#[derive(Debug)]
struct SessionState {
    /// `CopilotAuth` holds the `CopilotTokenCache`. `token()` takes `&mut
    /// self`, so we wrap in `Mutex` to serialize cross-task access.
    auth: Mutex<CopilotAuth>,
    /// Raw reqwest client. Reused for JWT exchange (inside `CopilotAuth`)
    /// *and* for `/chat/completions` POSTs via the wire layer.
    http_inner: reqwest::Client,
}

/// Process-scoped handle. `tokio::sync::OnceCell` (not `std::sync::OnceLock`)
/// because initialization is async — `CopilotAuth::init` may walk disk and
/// drive the device-flow loop, which can take seconds. With `OnceLock` the
/// only async-friendly pattern was check-then-set, which let two concurrent
/// callers both observe `None`, both pay the full init cost, and only one
/// `set()` win — wasted device-flow + wasted CAPI mint on cold start. The
/// tokio variant serializes the first init internally so only one task
/// runs `CopilotAuth::init`; everyone else awaits the same future. F4.
static SESSION: tokio::sync::OnceCell<Arc<SessionState>> = tokio::sync::OnceCell::const_new();

/// `AtomicBool` rather than `OnceLock<()>` so the "printed?" check is a
/// single relaxed load on every subsequent call.
static BANNER_PRINTED: AtomicBool = AtomicBool::new(false);

/// Event stream returned by the adapter. Each item is a claude-codex
/// `ResponseEvent` (or a typed adapter error).
pub type AdapterStream = Pin<
    Box<
        dyn Stream<Item = std::result::Result<ResponseEvent, CopilotAdapterError>> + Send + 'static,
    >,
>;

/// Kept for API parity with the scaffold commit. No state beyond what
/// `SESSION` already holds — each `stream(...)` call synthesizes a fresh
/// `Mapper` because `response_id` is per-turn.
#[derive(Debug, Default)]
pub struct CopilotAdapter {
    _private: (),
}

impl CopilotAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self { _private: () }
    }
}

/// Open a Copilot chat-completions stream and return an adapter stream of
/// `codex_api::ResponseEvent`.
///
/// # Arguments
/// * `input` — the final flat input list (caller must have already run
///   `Prompt::get_formatted_input()`).
/// * `model` — the Copilot model slug (e.g. `"gpt-4o"`, `"claude-3.5-sonnet"`).
/// * `tools` — claude-codex tool specs. Function-shaped variants
///   (`ToolSpec::Function`, `ToolSpec::Freeform`) are translated to `OpenAI`
///   chat-completions `tools[]` and forwarded to the model. Responses-API-
///   only variants (`WebSearch`, `ImageGeneration`, `ToolSearch`,
///   `LocalShell`) are dropped with a one-shot warning.
///
/// # Errors
/// See `CopilotAdapterError`. The most common failures are `NotATty` (first
/// turn under a non-TTY without the env override) and `CopilotAuth(..)`
/// (token discovery / refresh).
pub async fn stream(
    input: &[ResponseItem],
    model: &str,
    tools: &[ToolSpec],
) -> Result<AdapterStream> {
    // 1+2. Shared first-turn guards (TTY policy + one-shot TOS banner).
    // Same function is called from the native /v1/messages and /responses
    // routes via `core/src/client.rs::ensure_copilot_ctx` (F4).
    enforce_copilot_first_turn_guards()?;

    // 3. One-time capability warning for tools we cannot forward.
    let skipped = unsupported_tool_count(tools);
    if skipped > 0 {
        tracing::warn!(
            skipped,
            "copilot adapter: {skipped} tool(s) (web_search/image_generation/tool_search) \
             are not forwarded to Copilot chat-completions; they will be unavailable this turn"
        );
    }

    // 4. Translate function-shaped tools to OpenAI chat-completions format.
    // The unsupported variants above are already filtered inside
    // `tools_to_openai_chat()`; what remains is the forwardable set.
    let openai_tools = tools_to_openai_chat(tools);

    // 5. Build or reuse the session state.
    let state = ensure_session().await?;

    // 6. Drive the inner pipeline against the cached auth + http.
    let mut auth_guard = state.auth.lock().await;
    stream_inner(
        &state.http_inner,
        &mut auth_guard,
        input,
        model,
        &openai_tools,
    )
    .await
}

/// Test seam: the real streaming pipeline with injected `http_inner` +
/// `auth`. The public `stream()` calls into this after resolving the session
/// cache; tests call it with a wiremock-backed `reqwest::Client` and a
/// `CopilotAuth` built via `CopilotAuth::from_token` (no real network).
///
/// Invariants: this function must NOT touch `SESSION` / `BANNER_PRINTED` /
/// `is_interactive_tty` — those are the public `stream()`'s job. It exists
/// solely to make the Mapper + wire wiring end-to-end testable.
///
/// # Wire layer ownership (invariants 10-16)
///
/// Streaming goes through `crate::wire::stream_chat`, not
/// `CopilotHttpClient::chat_stream_with_auth`. The wire layer owns:
/// - Invariant 10 (401 retry, mirrored line-for-line from upstream).
/// - Invariant 11 (`x-initiator: agent`).
/// - Invariant 12 (`x-interaction-id: <session uuid>`).
/// - Invariant 13 (`openai-intent: conversation-agent`).
/// - Invariant 14 (`Retry-After` honoring on 402/429 — surfaces typed error).
/// - Invariant 15 (extended `finish_reason` fidelity — `content_filter` /
///   `function_call` / error surfaced via `WireEvent::FinishReasonExtended`).
/// - Invariant 16 (`token_usage` via `stream_options.include_usage=true` →
///   `WireEvent::Usage`).
pub async fn stream_inner(
    http_inner: &reqwest::Client,
    auth: &mut CopilotAuth,
    input: &[ResponseItem],
    model: &str,
    tools: &[serde_json::Value],
) -> Result<AdapterStream> {
    let messages = items_to_chat_messages(input);
    let max_tokens = resolve_max_tokens();
    let response_id = format!("copilot-{}", synth_response_id());

    // Progressive streaming path. `wire::stream_chat_events` performs the
    // initial POST (with 401-retry-once) and hands back an already-live
    // stream of `WireEvent`s. We pump those through the `Mapper` and forward
    // each emitted claude-codex event through an mpsc channel in real time,
    // so the downstream consumer (core's `stream_copilot_api`) sees deltas
    // as soon as they land on the wire.
    //
    // The Mapper owns terminal-state bookkeeping: it buffers `Done`,
    // `FinishReasonExtended`, and `Usage` to produce a single `Completed`
    // event from `finalize()`, called after the wire stream ends.
    let wire_stream = wire::stream_chat_events(
        http_inner,
        auth,
        WireCallOptions {
            model,
            messages: &messages,
            max_tokens,
            request_id: Uuid::new_v4().to_string(),
            interaction_id: wire::session_interaction_id(),
            tools,
        },
    )
    .await
    .map_err(CopilotAdapterError::from)?;

    let (tx, rx) =
        tokio::sync::mpsc::channel::<std::result::Result<ResponseEvent, CopilotAdapterError>>(32);
    tokio::spawn(async move {
        use futures::StreamExt;
        let mut mapper = Mapper::new(response_id);
        let mut wire_stream = wire_stream;
        while let Some(next) = wire_stream.next().await {
            match next {
                Ok(ev) => {
                    for mapped in mapper.step_wire(ev) {
                        if tx.send(Ok(mapped)).await.is_err() {
                            return;
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(CopilotAdapterError::from(e))).await;
                    return;
                }
            }
        }
        // Graceful end-of-stream — drain terminal state through the Mapper.
        for mapped in mapper.finalize() {
            if tx.send(Ok(mapped)).await.is_err() {
                return;
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    Ok(Box::pin(stream))
}

/// Build the cached `SessionState` on first call, or return the existing one.
///
/// Uses `tokio::sync::OnceCell::get_or_try_init` so concurrent callers on
/// cold start serialize on the same init future — only one task runs
/// `CopilotAuth::init` (which may walk disk and drive the device-flow
/// loop), everyone else awaits its result. F4.
async fn ensure_session() -> Result<Arc<SessionState>> {
    let state = SESSION
        .get_or_try_init(|| async {
            // Hard read timeout so a stalled upstream connection surfaces an
            // error instead of hanging forever. The adapter's
            // `wire::stream_chat` reads the whole SSE body before returning
            // (legacy materialize-then-map design), so without a read
            // timeout a silent TCP stall on the Copilot enterprise proxy
            // would block the turn indefinitely. 90s is longer than any
            // legitimate inter-chunk gap we'd expect for a non-streaming
            // model response; true streaming over the native wires uses the
            // SSE-layer `stream_idle_timeout` path instead.
            let http_inner = reqwest::Client::builder()
                .use_rustls_tls()
                .read_timeout(std::time::Duration::from_secs(90))
                .build()
                .map_err(CopilotAdapterError::from)?;

            // `CopilotAuth::init` handles the disk → device-flow lookup
            // and will itself error if headless without
            // `force_allow_headless`. We've already done our own TTY check
            // above; we pass the same reqwest client so cookie/TCP pooling
            // is shared with the chat-completions path.
            let auth = CopilotAuth::init(http_inner.clone())
                .await
                .map_err(CopilotAdapterError::from)?;

            Ok::<Arc<SessionState>, CopilotAdapterError>(Arc::new(SessionState {
                auth: Mutex::new(auth),
                http_inner,
            }))
        })
        .await?;
    Ok(Arc::clone(state))
}

/// First-turn guards that must fire on EVERY Copilot route — native
/// `/v1/messages` and `/responses` as well as the chat-completions
/// fallback. Idempotent: TTY check on every call; TOS banner fires at most
/// once per process via `BANNER_PRINTED`.
///
/// HARD CONSTRAINT (subagent invariant): preserves the
/// headless-with-token-on-disk escape via `enforce_tty_policy()`'s
/// `discover_github_token().is_some()` check. Spawned subagents launched
/// in headless contexts depend on this — if a `ghu_` token is already on
/// disk (or in `$GITHUB_TOKEN`), the parent has already consented to
/// Copilot use and the child must be allowed to proceed without a TTY.
pub fn enforce_copilot_first_turn_guards() -> Result<()> {
    enforce_tty_policy()?;
    if !BANNER_PRINTED.swap(true, Ordering::AcqRel) {
        print_tos_banner();
    }
    Ok(())
}

/// Test-only seam (visible to integration tests in this crate too): reset
/// the once-per-process banner flag so the banner-once invariant test can
/// drive multiple `enforce_*` calls from a known initial state.
/// Production code never calls this — it is `#[doc(hidden)]` to keep it
/// out of the public surface.
#[doc(hidden)]
pub fn __reset_banner_for_tests() {
    BANNER_PRINTED.store(false, Ordering::Release);
}

/// Test-only accessor (see `__reset_banner_for_tests`): returns whether
/// the TOS banner has already fired this process. Used by the F4
/// regression suite to prove the once-per-process invariant without
/// scraping stderr.
#[doc(hidden)]
pub fn __banner_was_printed_for_tests() -> bool {
    BANNER_PRINTED.load(Ordering::Acquire)
}

/// TTY guard. See module docs.
fn enforce_tty_policy() -> Result<()> {
    if env::var_os(HEADLESS_OVERRIDE_ENV).is_some() {
        return Ok(());
    }
    if is_interactive_tty() {
        return Ok(());
    }
    // One last escape: if a GitHub token is *already* on disk, there is no
    // interactive step needed (no device flow). We allow headless execution
    // in that case because the user has already consented via a prior
    // login. This matches `CopilotAuth::init_with_config`'s behavior.
    if discover_github_token().is_some() {
        return Ok(());
    }
    Err(CopilotAdapterError::NotATty)
}

/// Resolve the max-tokens cap from env or fall back to the default.
fn resolve_max_tokens() -> Option<u32> {
    match env::var(MAX_TOKENS_ENV) {
        Ok(raw) => match raw.trim().parse::<u32>() {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(_) => {
                tracing::warn!(
                    env = MAX_TOKENS_ENV,
                    raw = raw.as_str(),
                    "copilot adapter: malformed env override; falling back to default max_tokens"
                );
                Some(DEFAULT_MAX_TOKENS)
            }
        },
        Err(_) => Some(DEFAULT_MAX_TOKENS),
    }
}

/// Synthesize a short, unique-enough response id. We use a monotonic counter
/// to avoid a `rand` dep for something that only feeds our internal event
/// stream (not a wire field).
fn synth_response_id() -> String {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{n:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_max_tokens_default() {
        // Sanity: default path returns a capped value.
        // SAFETY: we don't touch any global that the other tests care about.
        // We cannot clear env in parallel tests safely, so we only assert on
        // the absence-of-env branch by ensuring the default is sensible.
        let v = resolve_max_tokens();
        assert!(v.is_some());
        assert!(v.unwrap() > 0);
    }

    #[test]
    fn synth_response_id_is_unique_and_short() {
        let a = synth_response_id();
        let b = synth_response_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
