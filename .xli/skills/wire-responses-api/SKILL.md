---
name: wire-responses-api
description: Use when editing codex-rs/responses-api-proxy/ or the stream_responses_api() path in core/src/client.rs. Note that responses-api-proxy is a blocking HTTP forward proxy (NOT an SSE parser) — SSE passthrough is implicit. The actual /responses stream handling lives in core.
allowed-tools: Read, Edit, Grep, Bash(cargo nextest run -p codex-api *), Bash(cargo nextest run -p responses-api-proxy *)
---

# Wire: /responses API

## When to load
Any diff under `codex-rs/responses-api-proxy/` or touching
`stream_responses_api()` in `core/src/client.rs`.

## Architecture — two separate concerns

### 1. responses-api-proxy (the proxy crate)
`codex-rs/responses-api-proxy/` is a **blocking HTTP forward proxy** for
`/v1/responses`. It is NOT an SSE parser.

| File | Role |
|------|------|
| `src/lib.rs` | `run_main()` — binds `tiny_http::Server`, forwards requests |
| `src/main.rs` | Binary entry point |
| `src/dump.rs` | `ExchangeDumper` — optionally tees req/res to disk |
| `src/read_api_key.rs` | API key handling |

Key behavior:
- `forward_request()` validates `POST /v1/responses` only (403 otherwise)
- Strips/re-injects `Authorization` + `Host` headers
- Streams response body directly via `Box<dyn Read>` — SSE passthrough is implicit
- Uses `reqwest::blocking::Client` (not async)

### 2. stream_responses_api() (in core)
The actual `/responses` stream handling lives in `core/src/client.rs` at ~L1355.
This is the `WireApi::Responses` (default) path.

- `WireApi::Responses` is the default wire protocol — OpenAI's Responses API
- SSE events are parsed in core and mapped to `ResponseEvent` variants
- WebSocket transport is also supported for `/responses`

## ResponseEvent mapping
`/responses` produces `ResponseEvent` and `ResponseItem[]` natively — the
lingua franca was designed around this wire format. Other wires must adapt
into it. (See `response-item-lingua-franca` skill.)

## Workflow
1. Identify whether the change is in the proxy crate or the core stream handler.
2. If proxy: changes are about HTTP forwarding, not SSE semantics.
3. If core: the SSE parser and event mapping are the concern.
4. `cargo nextest run -p codex-api -p responses-api-proxy` green.

## Anti-patterns
- Treating `responses-api-proxy` as an SSE parser — it's a passthrough proxy.
- Leaking `/responses`-specific wire shapes past the adapter boundary.
- Adding new `ResponseEvent` variants without updating all wire adapters.
