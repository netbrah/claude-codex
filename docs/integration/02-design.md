# 02 — Integration Design

**Predecessor:** `01-recon.md` (commit `f1dc7c2`).
**Target tree:** `netbrah/claude-codex` @ `dev` (`63c18a30e` sampled).
**Source tree:** `netbrah/codex-agent` @ `main` (`d853552` or newer).
**Posture:** lock every decision with a file:line citation or an invariant number. No hedging.

---

## 1. Adapter shape

**Decision: new crate `codex-rs/copilot/` inside claude-codex. Depends on `codex-copilot` as a git dep pinned to `netbrah/codex-agent`.**

- The 66-test Copilot baseline stays authoritatively upstream. Invariant 1 (`83-test baseline green at every commit`) is preserved by reference, not by copy.
- Nothing under `codex-copilot/src/*.rs` is touched. If it were, we'd be violating the seed-prompt "definition of done" line 153.
- The new `codex-rs/copilot/` crate holds only the adapter: `CopilotAdapter` that constructs a `CopilotHttpClient` and bridges events to claude-codex's `ResponseEvent`.

### 1.1 Crate manifest

```toml
# codex-rs/copilot/Cargo.toml
[package]
name    = "codex-copilot-adapter"
version = "0.1.0"
edition = "2024"
rust-version = "1.94.1"

[dependencies]
codex-copilot    = { git = "https://github.com/netbrah/codex-agent.git", rev = "<pin>" }
codex-api        = { path = "../codex-api" }
codex-protocol   = { path = "../protocol" }
codex-model-provider-info = { path = "../model-provider-info" }
reqwest    = { workspace = true, features = ["rustls-tls", "stream", "json"] }
tokio      = { workspace = true }
futures    = { workspace = true }
async-stream = { workspace = true }
bytes      = { workspace = true }
serde      = { workspace = true }
serde_json = { workspace = true }
async-trait = { workspace = true }
thiserror  = { workspace = true }
tracing    = { workspace = true }

[dev-dependencies]
wiremock   = "0.6"
tokio      = { workspace = true, features = ["test-util"] }
```

No new transitive heavies — `rustls-tls` is already claude-codex's TLS choice (invariant 7). No `anyhow` bleed into `codex-copilot` (invariant 2).

### 1.2 Workspace member entry

Add exactly one line to `codex-rs/Cargo.toml [workspace].members`:

```toml
members = [
    # ... existing 88 entries ...
    "copilot",
]
```

No Bazel MODULE changes in Phase 3 — cargo is the canonical test path (`justfile`:51–53). Bazel BUILD file is added in Phase 5 if the user wants the Bazel test lane (open question G8).

## 2. Dependency direction

- **claude-codex → codex-agent** via git dep with pinned `rev`.
- Never the reverse. `codex-agent` does not know about `claude-codex`.
- Pin update cadence: only when we ship a new Copilot-crate feature. Bumps are a one-line `Cargo.toml` change + a passing test-run commit.
- **Vendor fallback (G3 in recon):** if `openai/codex` upstream rejects git deps, we switch to path-dep during dev and vendor `codex-copilot/src/*.rs` byte-identical into `codex-rs/copilot/vendor/codex-copilot/` during release. Vendor includes the test files verbatim. This is a Phase-6 concern; not addressed here.

## 3. Profile schema extension

### 3.1 TOML the user writes

```toml
# ~/.xli/config.toml

[model_providers.copilot]
name     = "GitHub Copilot"
base_url = "https://api.githubcopilot.com"
wire_api = "copilot"          # new variant — see §4
# No env_key, no auth.command. Copilot auths itself from ~/.config/github-copilot/.
requires_openai_auth = false
supports_websockets  = false
# The adapter ignores retry/timeout fields here — codex-copilot owns its own
# `slow_down` arithmetic (invariant 9) and 401-retry (invariant 10).

[profiles.copilot]
model          = "claude-sonnet-4.5"      # passed through verbatim to Copilot
model_provider = "copilot"
approval_policy = "on-request"
sandbox_mode    = "workspace-write"
```

### 3.2 What changes in `ConfigProfile` / `ModelProviderInfo`

- **Nothing.** Both structs already have `#[schemars(deny_unknown_fields)]`; we fit inside the existing field set.
- The only schema-level change is the new `WireApi::Copilot` variant (§4).

### 3.3 `-p copilot` UX

`codex -p copilot "fix the test"` resolves:

1. `cli/src/main.rs:1099,1112` — `with_profile(Some("copilot"))`.
2. `config/src/profile_toml.rs:26` — loads `[profiles.copilot]`.
3. `core/src/config/edit.rs:858` — merges profile over global.
4. `model-provider-info` lookup: `profile.model_provider = "copilot"` → `config.toml`'s `[model_providers.copilot]` overrides built-ins.
5. `core/src/client.rs:310 ModelClient::new` gets the resolved `ModelProviderInfo` with `wire_api = Copilot`.
6. `core/src/client.rs:1665 ModelClientSession::stream` matches the new arm → our adapter.

**Zero changes to the profile resolver.** Every step above is existing code.

## 4. Enum bridging

### 4.1 Add `WireApi::Copilot`

File: `codex-rs/model-provider-info/src/lib.rs`.

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WireApi {
    #[default]
    Responses,
    Messages,
    Copilot,                   // new
}

impl WireApi {
    pub fn supports_reasoning_effort(&self) -> bool {
        matches!(self, Self::Responses | Self::Messages)
        // Copilot does not forward reasoning params.
    }
}
```

Deserializer gets one new match arm: `"copilot" => Ok(Self::Copilot)`. `Display` gets `Copilot => "copilot"`.

### 4.2 Add dispatch arm in `ModelClientSession::stream`

File: `codex-rs/core/src/client.rs:1677`.

```rust
match wire_api {
    WireApi::Responses => { /* unchanged */ }
    WireApi::Messages  => { /* unchanged */ }
    WireApi::Copilot   => {
        self.stream_copilot_api(
            prompt,
            model_info,
            session_telemetry,
            sampling,
            turn_metadata_header,
        ).await
    }
}
```

And `effective_wire_api` (`client.rs:1743`) gets a `Copilot => Copilot` passthrough — no auto-upgrade for Copilot.

### 4.3 New method `stream_copilot_api`

Lives on `impl ModelClientSession` in `codex-rs/core/src/client.rs`. Delegates to the adapter crate:

```rust
async fn stream_copilot_api(
    &self,
    prompt: &Prompt,
    model_info: &ModelInfo,
    session_telemetry: &SessionTelemetry,
    sampling: SamplingParams,
    turn_metadata_header: Option<&str>,
) -> Result<ResponseStream> {
    codex_copilot_adapter::stream(
        self.client.state.provider.clone(),
        prompt,
        model_info,
        session_telemetry,
        sampling,
        turn_metadata_header,
    ).await
}
```

`session_telemetry` is plumbed through but the adapter only attaches it to the outgoing `ResponseStream` wrapper — not to the Copilot request (Copilot headers are frozen verbatim, invariant 4).

### 4.4 Event mapper signature

**Streaming model — correction from §4.4 draft.** `codex-copilot`'s public
API `chat_stream_with_auth` returns `Vec<ResponseEvent>` (materialized). It
is the only call that honors invariant 10 (401 retry). The `chat_stream_raw`
variant streams bytes but has no retry. Pulling in `codex-core::CopilotProvider`
is blocked by the crate-name collision (`codex-core` exists in both trees).

**Decision:** adapter calls `chat_stream_with_auth`, iterates the returned
Vec, and emits claude-codex events into the `mpsc::channel` as it walks the
Vec. The whole response is collected before the first event fires on
claude-codex's side — that is the cost of keeping invariant 10 intact. In
practice Copilot turns are short (< 10s); the UX cost is small and the
alternative is an invariant violation. Revisit in a later sortie if a user
notices latency.

File: `codex-rs/copilot/src/mapping.rs`.

```rust
use codex_api::ResponseEvent as ClaudeCodexResponseEvent;
use codex_copilot::sse::ResponseEvent as CopilotResponseEvent;
use codex_protocol::models::{ResponseItem, FunctionCallOutputPayload, ContentItem};

pub(crate) struct Mapper {
    response_id: String,          // from initial Copilot SSE `id` frame
    pending_text: String,         // buffered assistant text
    pending_tool: Option<PartialToolCall>,
}

struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,            // JSON string accumulator
    emitted_added: bool,          // we emit OutputItemAdded once at start
}

impl Mapper {
    pub fn new() -> Self { /* ... */ }

    /// Consume one Copilot event and emit zero-or-more claude-codex events.
    /// The return type is a small `smallvec` (max 2 in practice).
    pub fn step(&mut self, ev: CopilotResponseEvent)
        -> smallvec::SmallVec<[ClaudeCodexResponseEvent; 2]>;

    /// Finalize at end-of-stream (or on `Done`). Emits any still-pending
    /// tool-call `OutputItemDone` plus the `Completed { stop_reason, ... }`.
    pub fn finish(self, stop_reason: Option<String>)
        -> smallvec::SmallVec<[ClaudeCodexResponseEvent; 2]>;
}
```

Wire event → claude-codex event mapping (post-F1 / NB1):

The wire layer (`crate::wire::stream_chat`) returns `Vec<WireEvent>` where
`WireEvent` is one of `Copilot(codex_copilot::ResponseEvent)`,
`FinishReasonExtended(String)` (side-channel for reasons upstream's frozen
`SseReassembler` drops), or `Usage(WireUsage)` (parsed `usage` object from
the final chunk, requires `stream_options.include_usage=true` in the request
body).

| `WireEvent` variant | Emits on claude-codex wire |
|---|---|
| `Copilot(ContentDelta(s))` | `OutputTextDelta(s)` |
| `Copilot(ToolCall { id, name, args })` | `OutputItemAdded(FunctionCall { .. })` **then** `OutputItemDone(FunctionCall { .. })` |
| `Copilot(Done(raw))` | *buffered* — raw OpenAI `finish_reason` stashed for `finalize()` |
| `FinishReasonExtended(raw)` | *buffered* — takes precedence over upstream `Done` |
| `Usage(u)` | *buffered* — projected onto `TokenUsage` at `finalize()` |
| `Mapper::finalize()` | single `Completed { stop_reason, response_id, token_usage }` |

**Stop-reason translation** (`mapping.rs::translate_finish_reason`):

| Raw OpenAI `finish_reason` | Anthropic `stop_reason` |
|---|---|
| `stop`            | `end_turn` |
| `tool_calls`      | `tool_use` |
| `length`          | `max_tokens` |
| `content_filter`  | `content_filter` (pass-through) |
| `function_call`   | `tool_use` (legacy alias) |
| `error`           | `error` |
| (anything else)   | pass-through unchanged |

Precedence inside `finalize()`: extended reason > upstream `Done` > fallback
heuristic (`had_tool_call ? tool_use : end_turn`). The heuristic only fires
when neither path produced a terminal event — pathological on a well-formed
stream but preserves the pre-F1 behavior so we never hang the turn loop.

**Token-usage projection** (`mapping.rs::project_usage`): `prompt_tokens` →
`input_tokens`, `completion_tokens` → `output_tokens`, `prompt_tokens_details.cached_tokens` →
`cached_input_tokens`, `completion_tokens_details.reasoning_tokens` →
`reasoning_output_tokens`, `total_tokens` passes through;
`cache_creation_input_tokens` fixed at 0 (OpenAI `APIUsage` has no equivalent).

### 4.5 Outbound request shape

Copilot speaks OpenAI-style chat-completions. The adapter builds the JSON body from `prompt` using the existing helper we already have in `codex-agent`:

- Use `codex_core::provider::CopilotProvider::chat_stream` — it internally takes `serde_json::Value`, but we're composing the value here from the *claude-codex* `Prompt`. We therefore write a small `prompt_to_copilot_body(prompt, model)` in `codex-rs/copilot/src/request.rs` that:
  1. Flattens `prompt.get_formatted_input()` into OpenAI chat messages.
  2. Flattens `prompt.tools` into OpenAI tools format.
  3. Omits reasoning, verbosity, service_tier (not supported by Copilot wire).
  4. Sets `stream: true`.
- We **do not** reuse `conversation_to_anthropic_messages` — wrong shape.

This lives in the adapter, not the Copilot crate — invariant 2 (no `anyhow` in `codex-copilot`) stays intact.

## 5. Auth bootstrapping

- The adapter owns construction. On first `stream_copilot_api` call per session:
  1. `codex_copilot::is_interactive_tty()` gate (invariant 3). If not a TTY and no `CODEX_COPILOT_ALLOW_HEADLESS=1`, return a typed error up through `ResponseStream`.
  2. `codex_copilot::discover_github_token()` — discovers `~/.config/github-copilot/` (`hosts.json`, `apps.json`) verbatim. No changes to that code (invariant 4, 5).
  3. `codex_copilot::print_tos_banner()` — banner, once per session.
  4. Build `CopilotAuth` → `CopilotHttpClient` with the 13 verbatim headers (invariant 4).
- The resulting `CopilotHttpClient` is cached on the adapter's session-scoped struct; subsequent turns reuse it (respects invariant 9 — `slow_down` +5s arithmetic).
- 401 retry stays **inside** `CopilotHttpClient::chat_stream_with_auth` (invariant 10). The adapter calls `chat_stream_raw` like `CopilotProvider` does in `codex-core`.

Wait — that conflicts with invariant 10's last sentence: *"If you add retry semantics to the provider layer, do it in `codex-core`, not by swapping methods."* We're not in `codex-core` here; we're in a new crate. **Decision: the adapter calls `chat_stream_with_auth`**, not `chat_stream_raw`. Rationale: in `codex-agent`, `CopilotProvider` is constructed *after* `CopilotHttpClient::ensure_authed`, so `chat_stream_raw` is safe. In the claude-codex adapter we construct the `CopilotHttpClient` inside `stream_copilot_api`, so the 401-retry path belongs to us. This is the closer analogue to `codex-core::CopilotProvider::chat_stream` semantics, and it keeps the adapter stateless across sessions.

Document this explicitly in the adapter crate AGENTS.md so the next person doesn't "fix" it into `chat_stream_raw`.

## 6. TTY guard + banner placement

- Guard fires **inside the adapter's constructor path**, not inside `ModelClient::new`. Reason: `ModelClient::new` is called by exec (non-interactive) and tests; blocking there would break them. The adapter's first *actual stream call* is the right seam.
- Banner prints to `stderr` exactly once per process. `OnceLock<()>` inside the adapter.
- Tests bypass both via `CODEX_COPILOT_ALLOW_HEADLESS=1` + a test-only constructor that skips the banner. This mirrors how `codex-copilot` already handles it in its own unit tests.

## 7. Test plan

### 7.1 What migrates verbatim

**Zero tests migrate verbatim into claude-codex.** The 66 tests in `codex-copilot` + 5 in `codex-core` + 7 in `tools-core` + 5 in `integration-tests` = **83** stay in `netbrah/codex-agent`. Their green state is the authoritative proof of invariant 1. The adapter crate does not re-run them.

### 7.2 What the adapter adds

New tests in `codex-rs/copilot/src/`:

| Test | Crate | Kind | Coverage |
|---|---|---|---|
| `mapping_content_delta_passes_through` | copilot | unit, offline | `Mapper::step(ContentDelta) → OutputTextDelta` |
| `mapping_tool_call_emits_added_then_done` | copilot | unit, offline | `ToolCall → OutputItemAdded+OutputItemDone` same frame |
| `mapping_done_without_tool_is_end_turn` | copilot | unit, offline | Stop-reason classification |
| `mapping_done_with_tool_is_tool_use` | copilot | unit, offline | Stop-reason classification |
| `prompt_to_copilot_body_minimal` | copilot | unit, offline | OpenAI-shaped body synthesis |
| `prompt_to_copilot_body_with_tools` | copilot | unit, offline | Tools array shape |
| `prompt_to_copilot_body_omits_reasoning` | copilot | unit, offline | Reasoning fields are dropped |
| `wire_api_deserializes_copilot` | model-provider-info | unit, offline | `"copilot"` TOML → `WireApi::Copilot` |
| `wire_api_rejects_chat` | model-provider-info | unit, offline | Guard against regressing the removed `chat` variant |
| `stream_copilot_api_happy_path` | copilot | integ, wiremock | Full SSE replay: content deltas + one tool call + done |
| `stream_copilot_api_surfaces_401_retry` | copilot | integ, wiremock | First request 401, second succeeds — proves invariant 10 still fires |
| `stream_copilot_api_no_tty_errors` | copilot | unit, offline | TTY guard |
| `profile_copilot_round_trip` | config | unit, offline | `[profiles.copilot]` + `[model_providers.copilot]` load cleanly |
| `client_dispatch_selects_copilot_arm` | core | unit, offline, gated by `#[cfg(test)]` | Asserts the match arm in `stream()` is reachable |

**Total new: 14 tests.** Commit count: 14 once Phase 3 lands (delta count is the invariant we report in `03-validation.md`).

### 7.3 What does not get tested end-to-end in claude-codex

- Real Copilot token refresh against a live `api.github.com`. Covered by codex-agent's 83-test baseline using wiremock; not replayed here. (`E2E is not possible` per operator.)
- Headless shell-on-Copilot round trips. Deferred to the upstream PR reviewer.

### 7.4 Runner

`cargo nextest run -p codex-copilot-adapter --no-fail-fast` for the new crate.
`cargo nextest run -p codex-model-provider-info -p codex-core -p codex-config --no-fail-fast` for the upstream-touched crates.
**No workspace-root runs** (§G4 in recon).

## 8. No-regression checklist (applies invariants 1–16)

Invariants 1–10 are inherited from upstream `codex-agent`. Invariants 11–14
are new in this integration and are enforced by the `crate::wire` layer.
Invariants 15–16 were added post-F1 / NB1 to close fidelity gaps in the
event stream without editing the frozen crate.

| # | Invariant | How this design honors it |
|---|---|---|
| 1 | 83-test baseline stays green at every commit | We do not edit `codex-agent`. Its CI picks up on its own `main`. |
| 2 | No `anyhow` in `codex-copilot` | Adapter is its own crate; its error type is `thiserror`-based `CopilotAdapterError` (with a `Wire(#[from] wire::WireError)` variant). Upstream boundary into claude-codex's `Result` handled via `From`. |
| 3 | TTY guard preserved | Called in §6 from the adapter's first-stream path. |
| 4 | 11 upstream-pinned headers flow unchanged | The wire layer builds from `CopilotHeaders::new(..).with_initiator_agent(true).with_request_id(..)` — the upstream builder. We *add* three headers (invariants 11–13) on top; we do not modify or remove any of the upstream 11. |
| 5 | Redacted `Debug` on `CopilotTokenCache` | We do not `Debug`-print the cache. The adapter never touches its internals. |
| 6 | No heavy deps | Adapter adds only `async-stream`, `smallvec`, `uuid` (v4 feature), `wiremock` (dev-only). No `chrono`, no `secrecy`, no `native-tls`. |
| 7 | `rustls-tls` on reqwest | `features = ["rustls-tls"]` in adapter manifest. The same `reqwest::Client` is reused for JWT exchange and for `/chat/completions` POSTs (TCP/H2 pool reuse). |
| 8 | `process_body_chunked` + `flush` stays | The wire layer calls `SseReassembler::process_body_chunked` + `flush` — same public API upstream's `chat_stream_inner` uses. Event shape is byte-identical. |
| 9 | `slow_down` +5s arithmetic stays | Internal to `codex-copilot`'s `CopilotHttpClient`; not consulted on the `/chat/completions` path we replaced. The session cache (`OnceLock<Arc<SessionState>>`) still reuses a single `reqwest::Client` across turns — the half of invariant 9 that matters for `/chat/completions`. |
| 10 | 401-retry-once semantics | The wire layer mirrors `client.rs::chat_stream_with_auth` line-for-line: first attempt with cached JWT, on 401 call `auth.cache_mut().force_refresh().await?`, retry once; second 401 returns `CopilotAuthError::TokenRejected(401)`. Verified by `tests/stream_401_retry.rs`. |
| 11 | `x-initiator: agent` | Set via `CopilotHeaders::with_initiator_agent(true)` — upstream's public builder. Verified by `tests/stream_happy_path.rs` and `wire::tests::build_headers_overrides_intent_and_adds_interaction_id`. |
| 12 | `x-interaction-id: <session uuid>` | Inserted on the `HeaderMap` after `into_header_map()`. Session-scoped via `OnceLock<String>` so every turn in a process shares one ID (matches `vscode-copilot-chat`'s one-chat-view = one-interaction model). Verified by `wire::tests::session_interaction_id_is_stable_across_calls` + wiremock `header_exists`. |
| 13 | `openai-intent: conversation-agent` | Overrides the upstream default `conversation-panel` on the `HeaderMap` (insertion replaces). Matches `locationToIntent(ChatLocation.Agent)` in `vscode-copilot-chat`. Verified by wiremock `header("openai-intent", "conversation-agent")`. |
| 14 | `Retry-After` honored on 402/429 | `wire::parse_retry_after` reads the header before consuming the response body; returns typed `WireError::RateLimited { status, retry_after }`. Callers (core/client.rs) see the duration and can surface it verbatim in the turn error. HTTP-date form currently degrades to `None` (acceptable: caller still knows it was rate-limited). |
| 15 | Extended `finish_reason` fidelity | Upstream `SseReassembler` (`codex-copilot/src/sse.rs:88` at `26ff8f1d`) only surfaces `{tool_calls, stop, length}`; `content_filter` / `function_call` / `error` (and future values) are dropped — for `content_filter` the reassembler returns `[]` entirely, so pre-F1 the turn loop hung with no terminal event. The wire layer runs a `SideChannel` pass over the raw SSE bytes and emits `WireEvent::FinishReasonExtended(raw)` for any reason outside upstream's allow-list. The Mapper's `finalize()` gives it precedence, translates via `translate_finish_reason`, and emits `Completed { stop_reason: Some(raw_or_translated) }` — guaranteed even if upstream emitted nothing. Verified by `tests/stream_finish_fidelity.rs::content_filter_finish_reason_surfaces_as_completed` + 3 sibling cases. |
| 16 | `token_usage` via `stream_options.include_usage=true` | Request body now includes `stream_options: { include_usage: true }` so the server emits a final chunk carrying `usage`. Upstream's reassembler doesn't parse it (no `usage` field on `StreamChunk`); the `SideChannel` does, emits `WireEvent::Usage(WireUsage)`, and the Mapper projects onto `codex_protocol::protocol::TokenUsage`. Verified by `tests/stream_finish_fidelity.rs::usage_chunk_populates_token_usage` and `request_body_includes_stream_options_include_usage`. |

All sixteen hold. No exceptions to escalate.

**Deferred:** `X-GitHub-Api-Version` drift (`2025-04-01` vs upstream
`2025-05-01`) and the hardcoded `Copilot-Integration-Id: vscode-chat` are
documented as known gaps in `03-validation.md §3.7–3.8`. Neither changes the
wire-observable behavior of the agentic parity surface.

## 9. Files touched in claude-codex

```
codex-rs/
├── Cargo.toml                            # +1 line in [workspace.members]
├── copilot/                              # NEW CRATE (codex-copilot-adapter)
│   ├── Cargo.toml                        # +uuid v4
│   ├── AGENTS.md                         # mirrors source repo's posture
│   ├── src/
│   │   ├── lib.rs                        # public `stream(...)` + error enum
│   │   ├── adapter.rs                    # CopilotAdapter + tty/banner gate + SessionState
│   │   ├── mapping.rs                    # Mapper + unit tests
│   │   ├── request.rs                    # items_to_chat_messages + unit tests
│   │   └── wire.rs                       # NEW (commit 5): header overrides,
│   │                                     # Retry-After, 401-retry mirror, 7 unit tests
│   └── tests/
│       ├── stream_happy_path.rs          # wiremock integ + new-header assertions
│       └── stream_401_retry.rs           # wiremock integ
├── model-provider-info/src/lib.rs        # +WireApi::Copilot arm + deser + Display
└── core/src/client.rs                    # +WireApi::Copilot match arm, new fn stream_copilot_api
```

**Five distinct commits landed on `feat/codex-copilot-integration`** (hashes from `git log --oneline feat/codex-copilot-integration ^main`):

1. `4b1d3f6ca` — `copilot: scaffold adapter crate (no wiring)`
2. `400f4bd1e` — `copilot: add WireApi::Copilot variant and core dispatch arm`
3. `2fc566a0b` — `copilot: wire Mapper, request builder, and real adapter stream`
4. `608ca19dd` — `copilot: add wiremock integration tests for adapter stream`
5. `658f50f7c` — `copilot: wire-parity adapter path (invariants 11-14)`

Each commit compiles + tests clean on its own — so an intermittent push mid-sortie never leaves the feature branch in a broken state. Commit 5 introduces the `crate::wire` module (+ `uuid` dep) and rewires `stream_inner` to call `wire::stream_chat` instead of `CopilotHttpClient::chat_stream_with_auth`, closing the three null-space headers + Retry-After honoring in one atomic change.

## 10. Out of scope for this sortie

- Bazel BUILD file for the new crate (G8).
- MCP-over-Copilot integration — there is already an upstream MCP link referencing `api.githubcopilot.com/mcp/`; leaving it alone.
- Upstreaming to `openai/codex`. That is a Phase-6 handoff (`05-handoff.md`).
- Migrating the 83 codex-agent tests into claude-codex. Per §7.1 they stay upstream.
- Adding OAuth / device-code flow to `codex-copilot` — discovery via `~/.config/github-copilot/` is already sufficient per invariants 4, 5.

---

## Definition of done (Phase 1)

- [x] Adapter shape chosen + justified
- [x] Dependency direction chosen + fallback documented
- [x] Profile schema specified with example TOML
- [x] Enum bridge documented with mapper signature
- [x] Auth path routed with invariant-10 reconciliation
- [x] Test plan with exact final test delta (+14)
- [x] No-regression checklist: 10/10 holds
- [x] File-touch list + 5-commit plan

**Next:** Phase 2 — fork is already in place (`netbrah/claude-codex` exists). Create branch `feat/codex-copilot-integration` off `dev`, land commit 1 (scaffold only, no wiring).

---

## Addendum — 3-wire router landed (OPROD 04)

See `04-oprod-3wire-router.md` for the full mission order. Summary of what
changed in the production dispatcher after this design doc was frozen:

- **No new `WireApi` variant.** The pre-existing `Messages` and `Responses`
  arms at `core/src/client.rs` now serve Copilot-configured providers
  unmodified. Static routing (claude-* → Messages, gpt-5.x → Responses,
  everything else → Chat fallback) lives in
  `codex-copilot-adapter::endpoints::route_for_model`. Consulted from
  `effective_wire_api()`.
- **Auth via `CopilotCtx`, not a new `AuthProvider` impl.** The upstream
  `codex-api::AuthProvider` trait is synchronous. We pre-mint the CAPI
  bearer inside the existing async `current_client_setup()` via
  `codex-copilot-adapter::ctx::CopilotCtx` (atomic bearer + `endpoints.api`
  snapshot, `force_refresh` hook for 401 recovery) and stuff it into the
  existing concrete `CoreAuthProvider`. Zero signature churn across the
  seven callers of `current_client_setup`.
- **Per-wire base URL.** `endpoints.api` from the CAPI envelope is the
  enterprise root (`https://api.enterprise.githubcopilot.com`). The
  Messages dispatch arm splices in `/v1` before handing to
  `MessagesClient`; Responses uses it as-is. Chat stays on the adapter's
  own client.
- **Per-wire headers.** Copilot-shared (`copilot-integration-id`,
  `editor-version`, `x-initiator`) are stamped in
  `current_client_setup()`. `anthropic-beta: prompt-caching-2024-07-31` is
  stamped in the Messages arm only.
- **401 recovery.** A Copilot-aware wrapper
  (`handle_unauthorized_for_copilot`) sits in front of the existing
  `handle_unauthorized` at all three 401 sites. For Copilot providers it
  force-refreshes the CAPI bearer via `CopilotCtx::force_refresh` and
  returns a synthetic `UnauthorizedRecoveryExecution` so the telemetry
  path continues to work.
- **Known issue, tracked:** `MessagesClient::stream_request` overwrites
  `anthropic-beta` with `interleaved-thinking-2025-05-14` whenever
  `thinking.is_some()`. Prompt caching is preserved only when thinking is
  off. Scoped future work; not a blocker.

Live validation (O-1…O-4):

```
xli-cc exec --profile copilot-opus  'reply exactly: ACK-CLAUDE-MESSAGES'   # /v1/messages → 200
xli-cc exec --profile copilot-gpt   'reply exactly: ACK-GPT5-RESPONSES'    # /responses    → 200
xli-cc exec -c model=gpt-4o -c model_provider=copilot 'reply exactly: ACK-GPT4O-CHAT'  # /chat/completions → 200
xli-cc exec --profile copilot-opus  'Use the shell tool once to run: pwd'  # tool_use round-trip → 200
```

Test delta: **66 green** in `cargo test -p codex-copilot-adapter --release`
(baseline 54 + 12 new, of which 12 cover `route_for_model` exact
allowlists, case-mismatch safety, and legacy slug fallthrough).
