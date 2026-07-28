> **STATUS: ✅ COMPLETE** — This sortie has been implemented and merged. Kept for reference.

# S-020 — /messages Wire Unit Test Coverage (Sub-A/B)

**Priority:** 🔴 Ship-Blocking
**Complexity:** Medium (3-4 hours)
**Files:** `codex-rs/core/src/messages_wire.rs`, `codex-rs/core/src/client_tests.rs`
**Upstream Risk:** ZERO — test files are new/separate

## Problem

Core /messages routing functions have ZERO unit tests:
- `is_anthropic_model()` — routes ALL traffic; bug silently misroutes requests
- `anthropic_thinking_param()` — controls whether adaptive thinking is sent
- `anthropic_max_output_tokens()` — wrong values cause API rejections
- `effective_wire_api()` — auto-upgrades Messages to Responses for non-Anthropic models
- `messages_api_tool_choice()` — Messages wire tool choice translation
- `InputImage` content translation path — zero coverage

## Sub-A: messages_wire.rs Translator Tests

Add to existing `#[cfg(test)] mod tests` block in `messages_wire.rs`:

| # | Test | What It Verifies |
|---|------|-----------------|
| 1 | Multi-turn conversation with alternating user/assistant/tool_use/tool_result | Round-trip fidelity |
| 2 | Thinking blocks preserved in latest assistant, stripped from earlier turns | Token savings correctness |
| 3 | Developer role messages extracted to system prompt | W-7 / BREAK-1 |
| 4 | `cache_control` placed on last system block | S-019 |
| 5 | Empty conversation → empty messages array | Edge case |
| 6 | Tool_use with complex nested JSON arguments | Arg fidelity |
| 7 | Multiple consecutive user messages merged | Anthropic API requirement |
| 8 | `FunctionCallOutput` with `is_error` field | Error propagation |
| 9 | Redacted thinking blocks preserved across turns | `raw_wire_block` replay |

## Sub-B: client.rs Stream/Messages API Tests

Add to `codex-rs/core/src/client_tests.rs`:

| # | Test | What It Verifies |
|---|------|-----------------|
| 1 | `WireApi::Messages` routes to `stream_messages_api` | Routing correctness |
| 2 | Anthropic thinking param builds correctly for adaptive mode | `{type:"adaptive"}` |
| 3 | Max output tokens set per model family (opus=128K, sonnet=64K, haiku=8K) | `anthropic_max_output_tokens()` |
| 4 | Stop sequences wired through to request | Field passthrough |
| 5 | Temperature/top_p/top_k wired through to request | Sampling params |
| 6 | Tool_choice wired through to request | `messages_api_tool_choice()` |
| 7 | Metadata.user_id wired through to request | W-5 |
| 8 | Cache_creation_input_tokens parsed from usage | W-6 |
| 9 | Anthropic-beta header includes interleaved-thinking | S-009 |

## Reference Patterns

Follow existing patterns in:
- `codex-rs/core/src/messages_wire_regression_tests.rs` — regression test style
- `codex-rs/exec/tests/proxy_e2e_messages.rs` — E2E test patterns

## Upstream Compatibility

✅ All test files are new/separate. Zero upstream conflict risk.

## Build & Verify

```bash
cd codex-rs
cargo test -p codex-core messages_wire    # Sub-A
cargo test -p codex-core client_tests     # Sub-B
cargo test -p codex-core -p codex-api     # full suite — no regressions
```
