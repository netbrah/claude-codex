# 03 — Validation

**Phase:** 4
**Branch under test:** `feat/codex-copilot-integration` on `netbrah/claude-codex`
**Tip commit at time of writing:** `608ca19dd`
**Baselines referenced:**

- `netbrah/codex-agent` @ `26ff8f1d4967360960d0886aee8462bc56b558ff` — frozen Copilot crate, 66-test upstream baseline (83 workspace-root tests, 66 within `codex-copilot`).
- `realsweetpaul/claude-codex` fork at `netbrah/claude-codex@dev` — workspace root, edition 2024, toolchain `1.94.1`.

> **ROE note for the reviewer.** The sortie authoring environment had no `cargo` / `rustc` / `git-lfs` / `nextest` on `$PATH`. Machine execution of `cargo fmt`, `cargo clippy`, `cargo check`, `cargo nextest run` is therefore **deferred to the reviewer's local checkout**. Every item in §1 is a logical (human-readable) verification I performed by reading the source; every item in §2 is a mechanical command the reviewer must run locally before merge. Read this whole document as "what I verified by inspection, and what you must verify by invocation."

---

## 1. Logical (inspection) verifications

### 1.1 Type-level match surface

Verified the Mapper's pattern-match against every upstream variant:

| `codex_copilot::sse::ResponseEvent` | Mapper arm in `copilot/src/mapping.rs` | Emitted claude-codex event |
| --- | --- | --- |
| `ContentDelta(String)` | line 48 | `OutputTextDelta(String)` |
| `ToolCall { id, name, arguments }` | line 53 | `OutputItemAdded(FunctionCall) + OutputItemDone(FunctionCall)` |
| `Done(String)` | line 71 | `Completed { stop_reason, response_id, token_usage: None }` |

Upstream enum file inspected at `codex-agent/codex-copilot/src/sse.rs` lines 44-52 (as of `26ff8f1d`). Enum is non-`#[non_exhaustive]` and has exactly three variants. If upstream ever adds a fourth, Rust's exhaustiveness check will fire at the `match` in `Mapper::step`; this is the intended early-warning.

### 1.2 Field-level construction of `ResponseItem::FunctionCall`

`codex-rs/protocol/src/models.rs` lines 237-250 (confirmed in this branch):

```rust
FunctionCall {
    id: Option<String>,
    name: String,
    namespace: Option<String>,
    arguments: String,
    call_id: String,
},
```

The Mapper constructs it as:

```rust
ResponseItem::FunctionCall {
    id: Some(id.clone()),
    name,
    namespace: None,
    arguments,
    call_id: id,
}
```

All fields present; `namespace` is intentionally `None` because Copilot has no namespace concept and upstream stores the tool name flat.

### 1.3 Invariant coverage matrix

Invariants 1–10 inherited from `codex-agent/AGENTS.md`; 11–14 are new and
enforced by the `crate::wire` layer (commit `658f50f7c`). Full design
rationale in `02-design.md` §8.

| # | Invariant | Where preserved | How |
| --- | --- | --- | --- |
| 1 | 66-test upstream baseline green | `codex-agent` is not edited; adapter depends via `git+rev=26ff8f1d...` | Crate pinned; no source edits |
| 2 | No `anyhow` in `codex-copilot` or adapter | `copilot/src/lib.rs` uses `thiserror` only; `wire::WireError` is `thiserror`-derived | Grep below |
| 3 | TTY guard + TOS banner on adapter's first stream | `adapter::stream()` calls `enforce_tty_policy()` and swaps `BANNER_PRINTED` | Inspection |
| 4 | 11 upstream-pinned headers flow unchanged | `wire::build_headers` uses `CopilotHeaders::new(..).with_initiator_agent(true).with_request_id(..).into_header_map()` — same public builder upstream uses | `wire.rs` lines 217-240; test `build_headers_overrides_intent_and_adds_interaction_id` asserts upstream defaults (authorization, editor-version, editor-plugin-version, copilot-integration-id) remain |
| 5 | Redacted `Debug` on `CopilotTokenCache` | Upstream already implements it; adapter never peeks at internals | By reference |
| 6 | No heavy deps (chrono / secrecy / native-tls) | `copilot/Cargo.toml` deps: workspace crates + `smallvec` + `uuid` (v4 feature only) + `wiremock` (dev) | See `Cargo.toml` |
| 7 | `rustls-tls` on reqwest | `copilot/Cargo.toml` sets `reqwest = { features = ["rustls-tls", ...] }`; `ensure_session` calls `.use_rustls_tls()` on the client builder | Explicit |
| 8 | `process_body_chunked` + flush stays | `wire::do_once` calls `SseReassembler::new()`, `.process_body_chunked(text)`, and `.flush()` — upstream's public SSE parser | `wire.rs` lines 200-211 |
| 9 | Session cache across turns | `static SESSION: OnceLock<Arc<SessionState>>` caches `CopilotAuth` + `reqwest::Client`; same client reused for JWT exchange and `/chat/completions` POSTs (TCP/H2 pool reuse). Note: v2 dropped `CopilotHttpClient` from the cache because we no longer call its methods — `slow_down` backoff arithmetic is only consulted inside `chat_stream_with_auth`, which we bypass. | `adapter.rs` `SessionState` |
| 10 | 401-retry-once semantics | `wire::stream_chat` mirrors `codex_copilot::client.rs::chat_stream_with_auth` line-for-line: cached-JWT attempt → on 401 `auth.cache_mut().force_refresh().await?` → retry once → second 401 surfaces `CopilotAuthError::TokenRejected(401)` | `wire.rs` lines 134-161; `tests/stream_401_retry.rs` |
| 11 | `x-initiator: agent` | `CopilotHeaders::with_initiator_agent(true)` — upstream's public builder | `wire.rs` line 223; wiremock `header("x-initiator", "agent")` in `tests/stream_happy_path.rs`; unit test in `wire.rs::tests` |
| 12 | `x-interaction-id: <session uuid>` | Inserted on `HeaderMap` after `into_header_map()`. Session-scoped via `OnceLock<String>` so all turns in a process share one ID (matches `vscode-copilot-chat`'s one-chat-view = one-interaction model) | `wire.rs` lines 98-102, 234-237; wiremock `header_exists("x-interaction-id")`; unit test `session_interaction_id_is_stable_across_calls` |
| 13 | `openai-intent: conversation-agent` | `HeaderMap::insert` replaces upstream's `conversation-panel` default. Matches `locationToIntent(ChatLocation.Agent)` in `vscode-copilot-chat` | `wire.rs` lines 228-232; wiremock `header("openai-intent", "conversation-agent")` |
| 14 | `Retry-After` honored on 402/429 | `wire::parse_retry_after` reads the header before consuming response body; returns typed `WireError::RateLimited { status, retry_after }` with integer-seconds form. HTTP-date form degrades to `None` (caller still knows it was rate-limited — acceptable) | `wire.rs` lines 186-192, 274-285; unit tests `parse_retry_after_accepts_integer_seconds` + `parse_retry_after_returns_none_on_garbage/missing` |

### 1.4 Crate-graph sanity

`codex-rs/Cargo.toml` members include `"copilot"`; workspace dep
`codex-copilot-adapter = { path = "copilot" }` is wired. `core/Cargo.toml`
now depends on `codex-copilot-adapter = { workspace = true }`. The adapter
itself pulls in:

- `codex-api`, `codex-protocol`, `codex-tools`, `codex-model-provider-info` (sibling path deps)
- `codex-copilot` (git+rev pinned)
- `reqwest`, `tokio`, `futures`, `async-stream`, `bytes`, `serde`, `serde_json`, `async-trait`, `thiserror`, `tracing`, `smallvec` (workspace / thin external)

No `anyhow`, `chrono`, `secrecy`, or `native-tls` deps anywhere in the
adapter's transitive closure (invariants 2, 6, 7).

### 1.5 Event-flow tracing

For a user prompt "say hi" against model `gpt-4o`:

1. `core/src/client.rs::stream_copilot_api` calls `prompt.get_formatted_input()` (pub(crate), reachable from `core`) to materialize the input as `Vec<ResponseItem>`.
2. Hands off to `codex_copilot_adapter::stream(&input, &model, &tools)`.
3. Adapter's `stream()` runs `enforce_tty_policy` → `BANNER_PRINTED.swap` → `ensure_session` → `stream_inner`.
4. `stream_inner` calls `items_to_chat_messages(input)` (`request.rs`) producing `[ChatMessage { role:"user", content:"say hi" }]`.
5. `http.chat_stream_with_auth(&mut auth, "gpt-4o", &messages, Some(8192))` hits `/chat/completions`; upstream's `SseReassembler` yields `Vec<ResponseEvent>`.
6. Each upstream event flows through `Mapper::step`, producing claude-codex events. `Mapper` owns `had_tool_call` state so the final `Done` event picks the correct `stop_reason` without inspecting upstream's `finish_reason` string.
7. `stream_copilot_api` funnels events into an `mpsc::channel` and returns a `ResponseStream`.

No state crosses turn boundaries except the cached `SessionState`.

---

## 2. Required local verification (reviewer's CHARLIE MIKE)

Run these in the reviewer's checkout of `netbrah/claude-codex@feat/codex-copilot-integration`. All commands are from `codex-rs/` unless noted.

### 2.1 Format + clippy

```bash
cargo fmt --check -p codex-copilot-adapter
cargo clippy -p codex-copilot-adapter --all-targets -- -D warnings
```

The adapter crate is annotated with `#![warn(clippy::pedantic)]`. If pedantic
fires in upstream tests via macro expansion, demote with a targeted
`#[allow]` — do **not** remove the crate-level warn.

### 2.2 Type-check the whole tree

```bash
cargo check -p codex-copilot-adapter
cargo check -p codex-core
cargo check --workspace
```

The workspace `--all-targets` check is the real integration gate — it
confirms the new `WireApi::Copilot` arm, the adapter bridge, and the
model-provider-info plumbing compile together. If you see any `unused
import` warning on `Arc` in `adapter.rs`, drop it; I held onto the import
defensively after removing `state_for_task`.

### 2.3 Targeted tests (nextest)

```bash
# Adapter-only unit tests (Mapper + request flattener).
cargo nextest run -p codex-copilot-adapter --lib --no-fail-fast

# Adapter integration tests (wiremock).
cargo nextest run -p codex-copilot-adapter --test stream_happy_path --no-fail-fast
cargo nextest run -p codex-copilot-adapter --test stream_401_retry --no-fail-fast

# The WireApi variant + dispatch arm tests in the provider crate.
cargo nextest run -p codex-model-provider-info --no-fail-fast
```

### 2.4 Regression-sweep around edits

`core/src/client.rs` is the only hot file touched. Run the client crate's
test harness to catch unintended collateral:

```bash
cargo nextest run -p codex-core --no-fail-fast
```

> **Do not** run `cargo test` at the workspace root. `codex-agent`'s house
> rule against workspace-wide runs applies here too (job-server contention
> plus flaky fixture deps in sibling crates).

### 2.5 Full baseline — opt-in, not required

The upstream `codex-copilot` 66-test baseline is **not** migrated into
this tree. If you want defense in depth, verify it separately in a
checkout of `netbrah/codex-agent@26ff8f1d`:

```bash
# In a sibling clone of netbrah/codex-agent at 26ff8f1d.
cargo nextest run -p codex-copilot --no-fail-fast
```

This is the authoritative 66-green signal. The adapter cannot regress
those tests because the adapter crate does not edit `codex-copilot`.

---

## 3. Known gaps / deferred items

1. **No streaming (incremental) mode.** `chat_stream_with_auth` returns `Vec<ResponseEvent>`, so the adapter materializes the full response before emitting any claude-codex event. For short Copilot turns this is fine; for long-form generations the first-byte latency will be noticeable. Addressing it requires either upstream exposing an incremental iterator variant or the adapter adopting `chat_stream_raw` and wrapping its own 401-retry — both deferred to v2.
2. **Tool schemas are not forwarded.** `chat_stream_with_auth` takes `&[ChatMessage]` with no `tools` parameter. The adapter logs a single `tracing::warn` on turns that declare `web_search`, `image_generation`, or `tool_search` tools and proceeds without them. Function-call tool semantics still ride inline in text (see `request.rs`).
3. **`session_telemetry`, `sampling`, and `turn_metadata_header` are dropped** in `stream_copilot_api`. Copilot's chat-completions endpoint has no equivalents. If claude-codex later requires per-turn telemetry from every provider, the adapter will need a hook.
4. **`response_id` is process-local.** Synthesized via an atomic counter (`adapter::synth_response_id`). Copilot SSE does not surface a server-side response id; callers that correlate by id must not rely on it matching any backend log.
5. **No vision input.** `ContentItem::InputImage` is dropped during flattening (`request.rs::flatten_content`). Copilot's multimodal payload shape is not modeled here.
6. **No `cargo` in authoring sandbox — all compile/test signals are deferred to the reviewer.** See header.

### 3.7 `X-GitHub-Api-Version` override (vscode parity)

Every Copilot LLM-plane request — `/chat/completions`, `/v1/messages`, and
`/responses` — ships `X-GitHub-Api-Version: 2025-05-01` to match what
`microsoft/vscode-copilot-chat@9e668cb12144c701cf0f2c6b3458c00fe3da20f1`
sends on every CAPI POST
(`src/platform/networking/common/networking.ts:283`). The same header is
listed in the BYOK reserved-headers set
(`src/extension/byok/node/openAIEndpoint.ts:96`), so even custom endpoints
in vscode cannot drop it — strong evidence GitHub treats this header as
required infrastructure on the LLM plane, not just an `api.github.com`
REST concern.

Upstream `netbrah/codex-agent@26ff8f1d4967360960d0886aee8462bc56b558ff`
(`codex-copilot/src/auth.rs:25`) defines the public constant
`GITHUB_API_VERSION = "2025-04-01"` and unconditionally stamps it via
`CopilotHeaders::into_header_map` (`codex-copilot/src/client.rs:139`).
That value is one month stale relative to vscode. Because the upstream
crate is frozen by SHA, the adapter overrides at every wire seam:

- `/chat/completions`: `codex-copilot-adapter::wire::build_headers()`
  inserts the override after `into_header_map()?` returns. Header casing
  matches upstream (`http::HeaderName::from_static` lowercase), so the
  insert is a true overwrite, not a duplicate.
- `/v1/messages` and `/responses`:
  `codex-core::client::stamp_copilot_shared_headers()` (called from
  `current_client_setup` for the Copilot wire) seeds the shared
  `api_provider.headers` map. Per-wire dispatch arms (`anthropic-beta` on
  Messages, etc.) layer on top.

The shared constant lives in `codex-copilot-adapter::GITHUB_API_VERSION`
and is the single source of truth. A drift canary
(`codex-rs/copilot/tests/upstream_drift.rs`) asserts upstream still pins
`2025-04-01`; if upstream bumps, the test fails and prompts a
re-evaluation of whether the override is still needed. Unit and
integration regression guards live in
`codex-rs/copilot/src/wire.rs::tests` and
`codex-rs/copilot/tests/stream_happy_path.rs`.

The header value is opaque to us — we do not know whether GitHub treats
it as a routing key, a feature flag, or a server-side schema selector.
The conservative posture is to ship what real Copilot Chat ships and let
the drift canary surface upstream movement.

Closes `S-COPILOT-API-VERSION-DROP`.

### 3.8 `Copilot-Integration-Id: vscode-chat` is hardcoded (NB2b)

Upstream `CopilotHeaders::new` hardcodes `Copilot-Integration-Id: vscode-chat`
and exposes no builder override. The adapter cannot change this without
editing the frozen crate, which is out of scope for this sortie. This is
acceptable for Phase-1 parity — every claim about agentic-context behavior
we make is keyed off `x-initiator: agent` + `openai-intent: conversation-agent`,
not off the integration-id string. Revisit if GitHub ever gates features on
the integration-id.

---

## 4. Sign-off criteria

Merge is cleared when, on a local checkout:

- [ ] §2.1 `cargo fmt --check` and `cargo clippy -- -D warnings` pass for `codex-copilot-adapter`.
- [ ] §2.2 `cargo check --workspace` passes.
- [ ] §2.3 Adapter unit tests (7) pass: 4 in `mapping.rs`, 2 in `request.rs` (plus `prompt_to_copilot_body_drops_image_only_message`), 2 in `adapter.rs`.
- [ ] §2.3 Adapter integ tests (3) pass: `stream_inner_content_then_completed_end_turn`, `stream_inner_tool_call_emits_added_done_and_tool_use_stop`, `stream_inner_retries_once_on_401_and_succeeds`.
- [ ] §2.3 `model-provider-info` suite stays green (the 5 new `WireApi::Copilot` tests included).
- [ ] §2.4 `codex-core` suite stays green.
- [ ] §2.5 (optional) upstream 66-test `codex-copilot` baseline stays green on `26ff8f1d`.

Any red on §2.1-§2.4 is a blocker. §2.5 is a sanity check only.
