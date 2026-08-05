# Claude-First Architecture Audit (`dev` branch)

**Date:** 2026-08-05  
**Scope:** `netbrah/claude-codex` call flow, file ownership, coupling, and the
remaining extraction steps needed to make `core/src/client.rs` effectively
Responses-native while keeping Anthropic `/messages` behavior intact.

---

## Verdict

The Anthropic-specific request-building and history-translation logic is already
in the right place: `provider-anthropic`, `codex-api`, and the shared
provider/model abstractions. The main remaining Claude-first coupling is the
Messages/Copilot/GenerateContent dispatch code that still lives inside
`codex-rs/core/src/messages_dispatch.rs` and is still invoked from
`codex-rs/core/src/client.rs`.

The highest-leverage cleanup path is:

1. move provider-aware 401 recovery to `codex-model-provider`
2. extract `messages_dispatch.rs` into its own workspace crate
3. move the wire-routing match out of `client.rs` and into the turn-entry layer

That sequence removes the most volatile XLI-owned logic from upstream-heavy
files without destabilizing the provider-anthropic leaf crates.

---

## 1. ASCII call-flow diagram

```text
User / TUI input
     |
     v
thread_manager.rs :: ThreadManager
     |  builds Prompt + ResponseItem[] history
     |  calls ModelClientSession::stream()
     v
core/src/client.rs :: ModelClientSession::stream()
     |
     | provider.effective_wire_api(model_slug)
     |        |
     |   +----+--------------------------------------------+
     |   | WireApi::Responses                              |
     |   |                                                 |
     |   |  stream_responses_websocket()                   |
     |   |    or stream_responses_api()                    |
     |   |    -> codex-api::ResponsesClient                |
     |   |    -> SSE: codex-api/sse/responses.rs           |
     |   +-------------------------------------------------+
     |
     |   +-------------------------------------------------+
     |   | WireApi::Messages | Copilot | GenerateContent   |
     |   |                                                 |
     |   |  stream_via_provider()   <- messages_dispatch.rs|
     |   |    | ensures_session_ctx()                      |
     |   |    | builds ProviderStreamRequest               |
     |   |    | provider.stream(req)                       |
     |   |    |        |                                   |
     |   |    |   +----+--------------------------------+  |
     |   |    |   | provider-anthropic/                 |  |
     |   |    |   |   AnthropicMessagesProvider         |  |
     |   |    |   |                                     |  |
     |   |    |   |  build_messages_request()           |  |
     |   |    |   |   -> conversation_to_anthropic_     |  |
     |   |    |   |      messages()  (wire.rs)          |  |
     |   |    |   |   -> extract_developer_blocks()     |  |
     |   |    |   |   -> tools_to_anthropic_format()    |  |
     |   |    |   |   -> anthropic_thinking_param()     |  |
     |   |    |   |   -> anthropic_max_output_tokens()  |  |
     |   |    |   |  build_messages_extra_headers()     |  |
     |   |    |   |                                     |  |
     |   |    |   |  backend.execute_messages_turn()    |  |
     |   |    |   +--------------+----------------------+  |
     |   |    |                  |                         |
     |   |    |  MessagesBackendAdapter                    |
     |   |    |      -> run_messages_turn()               |
     |   |    |                  |                         |
     |   |    |  run_messages_turn() <- messages_dispatch.rs
     |   |    |   401 retry loop                          |
     |   |    |   provider.transform_messages_base_url() |
     |   |    |   ApiMessagesClient::stream_request()    |
     |   |    |        |                                  |
     |   |    |   codex-api/endpoint/messages.rs         |
     |   |    |        | POST /v1/messages               |
     |   |    |        v                                  |
     |   |    |   codex-api/sse/messages.rs              |
     |   |    |        | SSE parser                      |
     |   |    |        | messages_wire_types.rs          |
     |   |    |        | -> ResponseEvent                |
     |   |    |        v                                  |
     |   |    |   map_response_stream()                  |
     |   |    |   -> ResponseStream                      |
     |   |    +------------------------------------------+
     |   +-------------------------------------------------+
     |
     v
ResponseStream -> thread_manager turn loop
  -> ResponseItem[] accumulation
  -> tool dispatch, compaction, history replay
```

### Anthropic history-translation chain

```text
ResponseItem[] (Codex internal)
     |
     v  provider-anthropic/src/wire.rs :: conversation_to_anthropic_messages()
        |- clean_orphaned_tool_calls()
        |- strip_thinking_from_non_latest_assistant_messages()
        |- per-item translation into Anthropic blocks
        |    - thinking / redacted_thinking with signature passthrough
        |    - tool_use / tool_result pairing
        |    - cache_control injection
        '- developer-role blocks -> system[] param
     v
Vec<serde_json::Value> (Anthropic messages[])
```

---

## 2. File-level inventory

### Core orchestration (upstream-heavy, XLI-modified)

| File                                     |          LoC | Role                                                       | Coupling note                                                                   |
| ---------------------------------------- | -----------: | ---------------------------------------------------------- | ------------------------------------------------------------------------------- |
| `codex-rs/core/src/client.rs`            |        2,361 | Session/turn orchestration and top-level wire dispatch     | Imports `handle_unauthorized_for_provider`; routes `WireApi` at lines 1655-1705 |
| `codex-rs/core/src/messages_dispatch.rs` |          347 | Provider dispatch, transport retry, and backend bridge     | XLI-owned dispatch file inside `codex-core`                                     |
| `codex-rs/core/src/client_common.rs`     |           16 | Re-exports `Prompt`, `ResponseStream`, and `ResponseEvent` | Shared thin shim                                                                |
| `codex-rs/core/src/thread_manager.rs`    |        1,561 | Turn loop, history, tool dispatch, compaction              | Calls `ModelClientSession::stream()`                                            |
| `codex-rs/core/src/lib.rs`               | module decls | Core module wiring                                         | Still declares `mod messages_dispatch;`                                         |

### Anthropic wire layer (already split into dedicated crates)

| File                                                    |   LoC | Role                                                     |
| ------------------------------------------------------- | ----: | -------------------------------------------------------- |
| `codex-rs/provider-anthropic/src/wire.rs`               | 4,875 | `ResponseItem[]` ↔ Anthropic JSON translator            |
| `codex-rs/provider-anthropic/src/request.rs`            |   498 | `build_messages_request()` and extra header construction |
| `codex-rs/provider-anthropic/src/provider.rs`           |   285 | `AnthropicMessagesProvider` implementation               |
| `codex-rs/provider-anthropic/src/model.rs`              |   320 | Anthropic model helpers and thinking/output-token rules  |
| `codex-rs/provider-anthropic/src/stream_accumulator.rs` |   298 | Turn-level stream accumulation                           |
| `codex-rs/provider-anthropic/src/stream_invariants.rs`  |   117 | Stream invariant checks                                  |
| `codex-rs/provider-anthropic/src/regression_tests.rs`   |   328 | Field-level regression guards                            |
| `codex-rs/provider-anthropic/src/lib.rs`                |    62 | Public API surface                                       |

### HTTP endpoint and SSE parser

| File                                                |   LoC | Role                                                        |
| --------------------------------------------------- | ----: | ----------------------------------------------------------- |
| `codex-rs/codex-api/src/endpoint/messages.rs`       |   181 | `MessagesApiRequest` and `MessagesClient::stream_request()` |
| `codex-rs/codex-api/src/sse/messages.rs`            | 2,620 | Anthropic SSE parser and `ResponseEvent` emission           |
| `codex-rs/codex-api/src/sse/messages_wire_types.rs` | 1,053 | Typed Messages-wire vocabulary                              |

### Provider infrastructure

| File                                      | Role                                                                     |
| ----------------------------------------- | ------------------------------------------------------------------------ |
| `codex-rs/model-provider/src/stream.rs`   | `MessagesBackend`, `ProviderStreamRequest`, cache retention mirror types |
| `codex-rs/model-provider/src/provider.rs` | `ModelProvider` trait, provider capabilities, defaults                   |
| `codex-rs/provider-registry/src/lib.rs`   | Factory dispatch from `WireApi` to provider instances                    |
| `codex-rs/model-provider-info/src/`       | `WireApi`, `ModelProviderInfo`, provider capability data                 |

---

## 3. Current coupling analysis

### What is genuinely shared and should stay shared

- **`ResponseItem` / `ResponseEvent`**: the internal lingua franca for turn
  history and streamed output.
- **`Prompt` / `ResponseStream`**: the request/response envelope shared by all
  wires.
- **`map_response_stream()`**: wraps raw `ResponseEvent` streams with telemetry
  and inference tracing.
- **`WireApi`**: the dispatch pivot across Responses, Messages, Copilot, and
  GenerateContent.
- **`MessagesApiRequest`**: shared between provider-side request builders and the
  transport/SSE layer.

### Anthropic-specific logic that already belongs in the provider layer

These are already in the right home and should not be pulled back into core:

- `conversation_to_anthropic_messages()`
- `extract_developer_blocks()`
- `tools_to_anthropic_format()`
- `anthropic_thinking_param()`
- `anthropic_max_output_tokens()`
- `build_messages_request()`
- `AnthropicMessagesProvider::stream()`

### What still lives in the wrong place

#### `messages_dispatch.rs` inside `codex-core`

This file still owns:

- `stream_via_provider()`
- `run_messages_turn()`
- `MessagesBackendAdapter`
- `handle_unauthorized_for_provider()`

That keeps a Claude-first transport seam inside an upstream-heavy crate.

#### Provider-aware 401 recovery is shared, not Messages-specific

`handle_unauthorized_for_provider()` is currently defined in
`messages_dispatch.rs`, but `client.rs` also calls it from the Responses retry
paths at lines 1359 and 1475. That makes it a cross-wire concern rather than a
Messages-only concern.

#### `stream_via_provider()` is still a `ModelClientSession` method

Because the dispatch functions live in an `impl ModelClientSession` block,
extracting them into a lower crate is not a simple file move. The seam must
become a free function or trait-driven adapter so the dependency graph does not
cycle back into `codex-core`.

#### Core-internal config types still cross the seam

`messages_dispatch.rs` imports core-local config types such as:

- `CacheRetention`
- `CacheRetentionByBlock`
- `ModelEffort`
- `SamplingParams`

The current design already mirrors some of these through leaf-safe types in
`codex-model-provider`, which is a strong signal that this extraction seam is
real and worth finishing.

---

## 4. Dependency graph (relevant portion)

```text
codex-protocol            ResponseItem, ResponseEvent, error types
codex-prompt              Prompt, ResponseStream
codex-model-provider-info WireApi, ModelProviderInfo
        ^
        |
codex-model-provider      ModelProvider trait, MessagesBackend trait,
                          ProviderStreamRequest
        ^                         ^
        |                         |
codex-api                codex-provider-anthropic
MessagesApiRequest       wire.rs, request.rs, provider.rs, model.rs
MessagesClient
sse/messages.rs
        ^                         ^
        +-----------+-------------+
                    |
            codex-provider-registry
                    ^
                    |
                codex-core
        client.rs, messages_dispatch.rs, thread_manager.rs
                    ^
                    |
                 codex-cli
```

### Primary cycle risk

A new dispatch crate cannot depend on `codex-core` while `codex-core` also
depends on that crate. The way out is to:

- stop implementing the dispatch seam as `ModelClientSession` methods
- pass the needed state by argument or trait
- keep the transport bridge abstracted through `MessagesBackend`

---

## 5. Staged extraction plan

### Phase 0 — tag the current state

**Purpose:** create a rollback oracle.

- Tag the current `dev` baseline.
- Keep the tag as the last known-good Claude-first reference point.

**Gate:** tag created and pushed.  
**Rollback:** always available.

### Phase 1 — move provider-aware auth recovery to `codex-model-provider`

**Problem solved:** `handle_unauthorized_for_provider()` is shared across wire
paths, but it lives in a Messages-specific file.

**Files to change:**

- `core/src/messages_dispatch.rs`
- `model-provider/src/provider.rs` or a new
  `model-provider/src/auth_recovery.rs`
- `core/src/client.rs`
- `core/src/client_tests.rs`

**Target seam:** a function that takes provider/auth state directly rather than
an entire `ModelClient`.

**Why first:** it removes the cross-wire dependency before the larger dispatch
extraction.

**Compile/test gate:**

- `cargo check --workspace`
- `cargo test -p codex-core -p codex-model-provider`

### Phase 2 — extract `messages_dispatch.rs` into a workspace crate

**Problem solved:** the remaining Claude-first dispatch logic stops living inside
`codex-core`.

**Files to change:**

- create `codex-rs/codex-messages-dispatch/`
- delete `codex-rs/core/src/messages_dispatch.rs`
- remove `mod messages_dispatch;` from `core/src/lib.rs`
- add the new crate to workspace manifests and imports
- update any absorb-policy references that still special-case the old files

**Key refactor:** convert `stream_via_provider()` and `run_messages_turn()` from
`ModelClientSession` methods into free functions or trait-backed helpers.

**Cycle risk:** high if the new crate tries to depend on `codex-core` directly.

**Compile/test gate:**

- `cargo check -p codex-messages-dispatch`
- `cargo check --workspace`
- `cargo build --release -p codex-cli`
- `cargo test -p codex-core`

### Phase 3 — move wire routing out of `client.rs`

**Problem solved:** the `WireApi` match in `client.rs` stops carrying
Messages/Copilot/GenerateContent specifics.

**Files to change:**

- `core/src/thread_manager.rs`
- `core/src/client.rs`

**Intent:** let `thread_manager.rs` decide the wire at the turn-entry layer and
call either the Responses path or the extracted provider-dispatch path.

**Compile/test gate:**

- `cargo check --workspace`
- `cargo build --release -p codex-cli`
- full relevant test sweep

---

## 6. What should not be attempted first

- Do **not** move `codex-api/sse/messages.rs` or
  `messages_wire_types.rs` into another crate. They already sit at a clean
  transport/parser seam.
- Do **not** refactor `ModelClientSession` wholesale before Phase 1 and Phase 2.
- Do **not** create a downward dependency from `provider-anthropic` into
  `codex-core`.
- Do **not** collapse the provider crate and the HTTP/SSE layer into a single
  monolithic Anthropic crate.
- Do **not** widen the change by moving `CacheRetention` or related core config
  enums during the initial extraction.

---

## 7. Acceptance criteria for “client.rs is effectively Responses-native”

Completion does not require byte-for-byte upstream parity. It does require these
conditions together:

- `core/src/client.rs` contains no `use crate::messages_dispatch` imports.
- `core/src/client.rs` contains no
  `WireApi::Messages | WireApi::Copilot | WireApi::GenerateContent` routing arm.
- `core/src/lib.rs` no longer declares `mod messages_dispatch;`.
- `core/src/client.rs` no longer imports Messages-wire-specific provider crates.
- `core/src/messages_dispatch.rs` no longer exists.
- the release build is clean for `codex-cli`.
- unit/integration tests for `codex-core`, `codex-api`, and
  `codex-provider-anthropic` pass.
- a live `/messages` e2e run passes against the proxy path.

---

## 8. Recommended offline fixture coverage

### Before Phase 1

- 401 -> retry -> success fixture
- 401 -> provider refresh -> success fixture
- 401 -> provider fatal error fixture with enriched diagnostics
- 401 -> standard auth-recovery fallthrough fixture

### Before Phase 2

All of the above, plus:

- `ProviderStreamRequest` round-trip fixture
- tool-use conversation fixture
- thinking replay fixture with signature continuity

### Before Phase 3

All of the above, plus:

- wire-routing regression for a Messages provider
- Responses-wire regression proving the old path stays untouched

---

## 9. Recommendation: new crate vs. module-only move

**Recommendation:** use a new crate for Phase 2, not a module-only move.

### Why

- A module-only move inside `codex-core` still leaves the logic inside an
  upstream-heavy crate.
- A workspace crate gives the boundary a compiler-enforced home.
- The current `MessagesBackendAdapter` already shows the right direction: the
  seam is transport/backend driven, not provider-anthropic driven.
- The real technical challenge is dependency shape, not file motion.

### Bottom line

The repository already completed the hard provider split. The remaining work is
primarily about finishing the dispatch split so that Anthropic-specific control
flow stops living in `core/src/client.rs` and `core/src/messages_dispatch.rs`.
