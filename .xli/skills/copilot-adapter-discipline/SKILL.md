---
name: copilot-adapter-discipline
description: Use when editing the codex-rs/copilot/ adapter crate. Mirrors the invariants from copilot/AGENTS.md — thiserror only (no anyhow), v2 wire layer owns its own POST/headers/401-retry, SseReassembler for parsing, Mapper for event translation. The adapter is a thin bridge, not a reimplementation.
allowed-tools: Read, Edit, Grep, Bash(cargo nextest run -p codex-copilot-adapter --no-fail-fast)
---

# Copilot adapter discipline

## When to load
Any diff under `codex-rs/copilot/` — the adapter crate that bridges
`WireApi::Copilot` into the `ResponseEvent` lingua franca.

## Architecture (v2)

The v2 adapter does **NOT** use `CopilotHttpClient` directly. The wire
layer POSTs through a raw `reqwest::Client` so it can inject parity
headers without bouncing through `chat_stream_with_auth`'s hardcoded
header builder.

### Module map
| File | Owns |
|------|------|
| `src/lib.rs` | Crate root, `CopilotAdapterError` enum, constants |
| `src/adapter.rs` | Entry point: `stream()` / `stream_inner()`, `SessionState` via `OnceCell` |
| `src/wire.rs` | POST + headers + 401-retry + SSE parsing + side-channel, `WireError`, `WireEvent`, `WireCallOptions` |
| `src/mapping.rs` | `Mapper` — stateful `WireEvent` → `codex_api::ResponseEvent` translation |
| `src/request.rs` | `items_to_chat_messages()`, `tools_to_openai_chat()` — prompt serialization |
| `src/ctx.rs` | `CopilotCtx`, `CopilotAuthSnapshot` (redacted-Debug bearer) |
| `src/endpoints.rs` | `CopilotWire` enum (`Messages` / `Responses` / `ChatCompletions`), `route_for_model()` |
| `src/auth_client.rs` | `build_copilot_auth_http_client()` — hardened reqwest builder |

### Entry point — `adapter.rs`
```
pub async fn stream(
    input: &[ResponseItem],
    model: &str,
    tools: &[ToolSpec],
) -> Result<AdapterStream>
```
- `AdapterStream` = `Pin<Box<dyn Stream<Item = Result<ResponseEvent, CopilotAdapterError>>>>`
- Session state cached in `static SESSION: tokio::sync::OnceCell<Arc<SessionState>>`
- TOS banner fires once via `static BANNER_PRINTED: AtomicBool`

### SSE event mapping flow
```
reqwest::Response.bytes_stream()
  → SseReassembler::process_body_chunked(text)   // upstream frozen parser
    → WireEvent::Copilot(CopilotResponseEvent)
  → SideChannel::ingest(text)                    // parallel raw-JSON scan
    → WireEvent::FinishReasonExtended(reason)
    → WireEvent::Usage(WireUsage)
  → Mapper::step_wire(WireEvent)                 // mapping.rs
    → codex_api::ResponseEvent                   // final output
```

### Error types (thiserror only — NO anyhow)
**`CopilotAdapterError`** (`lib.rs`):
- `CopilotAuth(CopilotAuthError)`, `Transport(reqwest::Error)`, `NotATty`,
  `TokenDiscovery(String)`, `Sse(String)`, `Wire(WireError)`

**`WireError`** (`wire.rs`):
- `Auth(CopilotAuthError)`, `Transport(reqwest::Error)`,
  `UpstreamStatus { status: u16 }`, `RateLimited { status, retry_after }`,
  `Sse(String)`, `HeaderBuild(String)`

### Wire routing — `endpoints.rs`
`CopilotWire` enum: `Messages` | `Responses` | `ChatCompletions`
`route_for_model()` returns a static route table — the core dispatcher
in `client.rs` calls `effective_wire_api()` which consults this table to
decide whether a Copilot request goes via `/messages`, `/responses`, or
`/chat/completions`.

## Non-negotiable invariants
1. No `anyhow` — `thiserror` + typed error enums only.
2. 401-retry is owned by `wire.rs::stream_chat_events()` — not by upstream
   `chat_stream_with_auth`. The v2 adapter re-implements this.
3. `SseReassembler::process_body_chunked` is the upstream frozen SSE parser —
   do not rewrite it.
4. No heavy deps: no `chrono`, no `secrecy`, no `native-tls`.
5. `rustls-tls` feature on `reqwest`.
6. Session state (`SessionState`) is cached in a `OnceCell` across turns —
   do not recreate per-call.
7. `Mapper` is stateful — it tracks pending finish reasons and usage across
   SSE events within a single stream.

## Workflow
1. Read `copilot/AGENTS.md` before making any change.
2. Identify which module owns the change (wire? mapping? request building?).
3. If the change involves auth or TLS, check whether it belongs in `auth_client.rs`
   or upstream in `codex-copilot`.
4. Implement the change.
5. `cargo nextest run -p codex-copilot-adapter --no-fail-fast` green.

## Anti-patterns
- Using `anyhow` anywhere in this crate.
- Adding auth/header construction logic that duplicates `wire.rs`.
- Recreating `SessionState` per-call instead of using the cached `OnceCell`.
- Modifying `SseReassembler` parsing logic (it's upstream-frozen).
