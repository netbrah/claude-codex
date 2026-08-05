---
name: wire-messages-anthropic
description: Use when editing the Anthropic `/messages` wire — its request construction in `codex-provider-anthropic`, the `MessagesBackend` seam in `codex-model-provider`, or the transport-orchestration side (`stream_messages_api` + `run_messages_turn`) in `codex-core`. Owns the `WireApi::Messages` adapter, `ResponseItem[]` → Anthropic `messages[]` translation, tool-use/tool-result round-trip, system-block cache-control placement, adaptive thinking, and Copilot-aware routing. Also owns the wire fidelity audit — the field-by-field cross-reference against the Anthropic SDK that ensures no capability is silently dropped. Load before touching any of those surfaces, running the `live_messages` test, or auditing wire fidelity.
allowed-tools: Read, Edit, Grep, Bash(cargo test -p codex-core -p codex-api -p codex-provider-anthropic -p codex-model-provider), Bash(cargo test -p codex-core --test all -- live_messages --ignored --test-threads=1), Bash(CODEX_PROXY_E2E=1 cargo test -p codex-exec --test proxy_e2e_messages -- --test-threads=1)
---

# Wire: /messages (Anthropic)

## When to load

Any diff touching:

- `codex-rs/provider-anthropic/src/request.rs` — pure request build.
- `codex-rs/provider-anthropic/src/wire.rs` — `ResponseItem[]` ↔ Anthropic
  JSON translator (conversation, developer blocks, tools).
- `codex-rs/provider-anthropic/src/provider.rs` —
  `AnthropicMessagesProvider::stream`, `effective_wire_api`.
- `codex-rs/model-provider/src/stream.rs` —
  `ProviderStreamRequest`, `ProviderStreamRequestBuilder`,
  `MessagesBackend` trait.
- `codex-rs/core/src/client.rs::stream_messages_api` (thin dispatcher),
  `run_messages_turn` (401 loop + Copilot splice + transport),
  `MessagesBackendAdapter`.
- Any `live_messages` integration test or `proxy_e2e_messages` run.

Also load before planning Commit 5c (Copilot wire lift) — the
Copilot-gated behavior inside `run_messages_turn` is scheduled to move
into `CopilotModelProvider::stream`.

Also load when running a **fidelity audit** — comparing our wire
implementation field-by-field against the Anthropic SDK types.

## Post-Sortie-1 split (memorize this boundary)

The Anthropic `/messages` path is split across **two sides** connected
by a one-method trait:

```
╭─ codex-provider-anthropic ──────────────────────────────╮
│ What a Messages request looks like:                     │
│   * ResponseItem[] → Anthropic messages[] translation   │
│   * Tool schema translation                             │
│   * System-block assembly + cache_control placement     │
│   * Reasoning-effort → thinking-param mapping           │
│   * Tool-choice mapping                                 │
│   * Metadata propagation                                │
│   * x-codex-turn-metadata header construction           │
│ Entry points:                                           │
│   * request::build_messages_request()  (pure)           │
│   * request::build_messages_extra_headers()  (pure)     │
│   * AnthropicMessagesProvider::stream() — overrides     │
│     ModelProvider::stream. Builds req, calls backend.   │
╰─────────────────────────────────────────────────────────╯
                        ▲
                        │ MessagesBackend trait
                        │ (one method: execute_messages_turn)
                        ▼
╭─ codex-core ────────────────────────────────────────────╮
│ How an HTTP turn runs:                                  │
│   * 401 retry loop                                      │
│   * current_client_setup (auth, provider, api_auth)     │
│   * Copilot /v1 splice + anthropic-beta header stamp    │
│     (scheduled to move to provider-copilot in 5c)       │
│   * Telemetry composition                               │
│   * ApiMessagesClient construction & dispatch           │
│   * map_response_stream → ResponseStream                │
│   * Auth recovery (handle_unauthorized_for_copilot)     │
│ Entry points:                                           │
│   * ModelClientSession::stream_messages_api — thin:     │
│     builds ProviderStreamRequest, calls provider.stream │
│   * ModelClientSession::run_messages_turn — the loop    │
│   * MessagesBackendAdapter<'a> — impls MessagesBackend  │
│     by forwarding to run_messages_turn                  │
╰─────────────────────────────────────────────────────────╯
```

**Cardinal rule:** no wire-specific logic in `codex-core`; no
transport/auth/telemetry logic in `codex-provider-anthropic`.
Violations re-wedge core/client.rs which is exactly what Sortie 1
extracted.

## Where the code lives

| Concern | File | Function |
|---------|------|----------|
| Request build (pure) | `provider-anthropic/src/request.rs` | `build_messages_request` |
| Extra headers (pure) | `provider-anthropic/src/request.rs` | `build_messages_extra_headers` |
| Tool-choice mapping (pure) | `provider-anthropic/src/request.rs` | `messages_api_tool_choice` (private) |
| System-block assembly (pure) | `provider-anthropic/src/request.rs` | `build_system_blocks` (private) |
| `ResponseItem[]` → `messages[]` | `provider-anthropic/src/wire.rs` | `conversation_to_anthropic_messages` |
| Developer-block extraction | `provider-anthropic/src/wire.rs` | `extract_developer_blocks` |
| Tool schema translation | `provider-anthropic/src/wire.rs` | `tools_to_anthropic_format` |
| Wire dispatch + auto-upgrade | `provider-anthropic/src/provider.rs` | `AnthropicMessagesProvider::{stream, effective_wire_api}` |
| Model-slug helpers | `provider-anthropic/src/model.rs` | `is_anthropic_model`, `anthropic_thinking_param`, `anthropic_max_output_tokens` |
| SSE response parser | `codex-api/src/sse/messages.rs` | `spawn_messages_stream`, `process_messages_sse` |
| Request struct | `codex-api/src/endpoint/messages.rs` | `MessagesApiRequest`, `MessagesApiMetadata` |
| Seam trait | `model-provider/src/stream.rs` | `MessagesBackend`, `ProviderStreamRequest`, `ProviderStreamRequestBuilder` |
| Core dispatcher (thin) | `core/src/client.rs` | `ModelClientSession::stream_messages_api` |
| Core orchestration | `core/src/client.rs` | `ModelClientSession::run_messages_turn` |
| Core seam adapter | `core/src/client.rs` | `MessagesBackendAdapter<'a>` |

## Key invariants (preserve on any change)

### Request shape

1. **Developer-role injection into `system[]`.** Developer-role blocks
   (AGENTS.md, permission directives, personality config) must land
   in Anthropic's `system` parameter, NOT `messages[]`. BREAK-1 / W-7
   regression if broken.
2. **`cache_control` placement.** `{"type": "ephemeral"}` sits on the
   **last** system block. Anthropic caches from the start of system
   through the cache_control breakpoint; placing it earlier forfeits
   cache hits on stable developer instructions.
3. **Adaptive thinking.** `anthropic_thinking_param` returns
   `Some(thinking)` only when `effort` is `Low | Medium | High`. When
   `thinking` is set, `max_tokens` stays at the model output cap so a
   long deliberation has room to land the final answer.
4. **Tool-choice clamping.** If `tools[]` is empty, `tool_choice` is
   omitted entirely. `ToolChoice::None` falls through to `auto`
   because Anthropic has no `"none"` variant — the caller expresses
   "never" by omitting tools.
5. **Metadata propagation.** `messages_metadata_user_id`, when
   configured on `ModelClient` state, serializes to
   `metadata.user_id` on the Anthropic request. Used for
   proxy-side routing and rate-limit bucketing.

### Transport

6. **Copilot `/v1` splice is idempotent.** `ensure_v1_prefix()` in
   `run_messages_turn` handles CAPI envelopes that already terminate
   with `/v1`. Never concatenate `/v1/messages` raw without going
   through the helper.
7. **`anthropic-beta: prompt-caching-2024-07-31`** stamps on every
   Copilot-routed Messages request so the SSE emits
   `cache_read_input_tokens` / `cache_creation_input_tokens`.
   `MessagesClient::stream_request` preserves caller-supplied
   `anthropic-beta` UNLESS `thinking` is set — in which case upstream
   overwrites with `interleaved-thinking-2025-05-14`. Known issue;
   document on touch.
8. **401 retry loop calls back into Copilot auth.** When a Copilot CAPI
   bearer expires mid-turn, `handle_unauthorized_for_copilot` refreshes
   the snapshot and the loop retries. Do not short-circuit this with
   a direct error return — downstream session recovery depends on
   the retry accounting in `PendingUnauthorizedRetry`.

## Adding a field to `ProviderStreamRequest`

Common scenario after an upstream sync. Recipe:

1. Add field to the struct in `model-provider/src/stream.rs`
   (internal, non-breaking because of `#[non_exhaustive]`).
2. Add the matching field to `ProviderStreamRequestBuilder` and a
   `.with_*(field)` chainable setter.
3. Forward the field in `ProviderStreamRequestBuilder::build()`.
4. Update `stream_messages_api` in `core/src/client.rs` to pass the
   new field through the builder chain.
5. Decide if `AnthropicMessagesProvider::stream` consumes it; if yes,
   plumb it into `build_messages_request`'s signature. If no (e.g.
   the field is Responses-only), it quietly rides on the struct
   without reaching Anthropic's translator.

---

## Wire Fidelity Audit

> **Purpose.** Field-by-field cross-reference between the Anthropic SDK
> types and our XLI implementation. Ensures no capability is silently
> dropped across refactors. This section is the canonical reference
> for concordance runs.
>
> **Last audit:** 2026-04-29 (refreshed) against SDK Python v0.97.0.
> **Gaps found:** 11 total (8 existing + 3 new: GAP-9, GAP-10, GAP-11).
> **Code issues found:** 4 (NEW-1 through NEW-6, see Known code issues).

### SDK reference paths (keep updated after `git pull`)

| SDK | Local path | Latest version |
|-----|-----------|----------------|
| Python | `~/Projects/anthropic-sdk-python/` | v0.97.0 |
| TypeScript | `~/Projects/anthropic-sdk-typescript/` | v0.91.1 |

Key SDK type files for cross-reference:

| Concern | SDK file (Python) |
|---------|-------------------|
| Request params | `src/anthropic/types/message_create_params.py` |
| Response message | `src/anthropic/types/message.py` |
| Content blocks | `src/anthropic/types/content_block.py` |
| Stop reason enum | `src/anthropic/types/stop_reason.py` |
| Stop details | `src/anthropic/types/refusal_stop_details.py` |
| SSE delta event | `src/anthropic/types/raw_message_delta_event.py` |
| Thinking config | `src/anthropic/types/thinking_config_param.py` |
| Thinking adaptive | `src/anthropic/types/thinking_config_adaptive_param.py` |
| Output config | `src/anthropic/types/output_config_param.py` |
| Tool union | `src/anthropic/types/tool_union_param.py` |
| Usage | `src/anthropic/types/usage.py` |
| Metadata | `src/anthropic/types/metadata_param.py` |

### Request field fidelity matrix

Cross-references `MessageCreateParamsBase` fields against
`MessagesApiRequest` (`codex-api/src/endpoint/messages.rs`) and
`build_messages_request` (`provider-anthropic/src/request.rs`).

| SDK field | XLI field | Status | Notes |
|-----------|-----------|--------|-------|
| `model` | `model` | OK | |
| `messages` | `messages` | OK | Translated via `conversation_to_anthropic_messages` |
| `max_tokens` | `max_tokens` | OK | Per-model caps in `anthropic_max_output_tokens` |
| `stream` | `stream` | OK | Always `true` |
| `system` | `system` | OK | `build_system_blocks` with cache_control placement |
| `tools` | `tools` | OK | `tools_to_anthropic_format` (client tools only) |
| `tool_choice` | `tool_choice` | OK | `messages_api_tool_choice` mapper |
| `thinking` | `thinking` | PARTIAL | `{"type": "adaptive"}` only; `display` not wired (GAP-3); `budget_tokens` not wired for `enabled` type |
| `temperature` | `temperature` | OK | |
| `top_p` | `top_p` | OK | |
| `top_k` | `top_k` | OK | |
| `stop_sequences` | `stop_sequences` | OK | Field present, always `None` (no config path yet) |
| `metadata` | `metadata` | OK | `user_id` only |
| `output_config` | `output_config` | PARTIAL | Copilot CAPI path only; effort values `xhigh`/`max` need mapping (GAP-4); `format` (JSON schema structured output) not wired (GAP-9) |
| `cache_control` (top-level) | — | MISSING | GAP-6: we place manually on system blocks (works, but top-level shortcut unavailable) |
| `container` | — | MISSING | GAP-7: code execution containers |
| `inference_geo` | — | MISSING | GAP-7: geographic routing |
| `service_tier` | — | MISSING | GAP-7: priority/standard/batch |

### Response field fidelity matrix

Cross-references `Message`, `RawMessageDeltaEvent`, `ContentBlock`,
`Usage`, and `StopReason` against `process_messages_sse`
(`codex-api/src/sse/messages.rs`), `AnthropicUsage`, and
`ResponseEvent` (`codex-api/src/common.rs`).

| SDK field / block type | XLI handling | Status | Notes |
|------------------------|-------------|--------|-------|
| `id` (message) | `response_id` | OK | |
| `model` (message) | `ResponseEvent::ServerModel` | OK | |
| `role` (always "assistant") | Hardcoded on `ResponseItem::Message` | OK | |
| `content[].text` | `BlockState::Text` → `OutputTextDelta` + `OutputItemDone` | OK | |
| `content[].thinking` | `BlockState::Thinking` → `ReasoningContentDelta` + `Reasoning` item | OK | signature preserved |
| `content[].redacted_thinking` | `BlockState::RedactedThinking` → `Reasoning` item with `raw_wire_block` | OK | byte-identical replay |
| `content[].tool_use` | `BlockState::ToolUse` → `FunctionCall` item | OK | call_id, name, arguments all preserved |
| `content[].server_tool_use` | — | MISSING | GAP-5 |
| `content[].web_search_tool_result` | — | MISSING | GAP-5 |
| `content[].web_fetch_tool_result` | — | MISSING | GAP-5 |
| `content[].code_execution_tool_result` | — | MISSING | GAP-5 |
| `content[].bash_code_execution_tool_result` | — | MISSING | GAP-5 |
| `content[].text_editor_code_execution_tool_result` | — | MISSING | GAP-5 |
| `content[].tool_search_tool_result` | — | MISSING | GAP-5 |
| `content[].container_upload` | — | MISSING | GAP-5 |
| `stop_reason`: end_turn | Captured as string | OK | |
| `stop_reason`: tool_use | Captured + truncation override | OK | |
| `stop_reason`: max_tokens | Captured | OK | |
| `stop_reason`: stop_sequence | Captured | OK | |
| `stop_reason`: pause_turn | Passes through as string but NO auto-continue | GAP-1 | |
| `stop_reason`: refusal | Passes through as string but no special handling | GAP-2 | |
| `stop_details` | — | MISSING | GAP-2: refusal category/explanation not parsed |
| `stop_sequence` (matched string) | Not captured | LOW | Informational only |
| `usage.input_tokens` | `AnthropicUsage.input_tokens` → `TokenUsage.input_tokens` | OK | |
| `usage.output_tokens` | `AnthropicUsage.output_tokens` → `TokenUsage.output_tokens` | OK | |
| `usage.cache_read_input_tokens` | `AnthropicUsage.cache_read_input_tokens` → `TokenUsage.cached_input_tokens` | OK | |
| `usage.cache_creation_input_tokens` | `AnthropicUsage.cache_creation_input_tokens` → `TokenUsage.cache_creation_input_tokens` | OK | |
| `usage.server_tool_use` | — | MISSING | GAP-8: server tool request counts |
| `usage.inference_geo` | — | MISSING | GAP-8: informational |
| `usage.service_tier` | — | MISSING | GAP-8: informational |
| `usage.cache_creation` (breakdown by TTL) | — | MISSING | GAP-8: fine-grained cache cost |
| `container` (response) | — | MISSING | GAP-10: Container metadata (id, expires_at) not parsed from message or delta |

### Conversation round-trip fidelity

| Concern | Status | Notes |
|---------|--------|-------|
| `tool_use` → `tool_result` pairing (call_id) | OK | `clean_orphaned_tool_calls` + `clean_orphaned_tool_blocks_by_adjacency` |
| `thinking` blocks stripped from non-latest assistant | OK | `strip_thinking_from_non_latest_assistant_messages` |
| `thinking` with `signature` round-tripped byte-identical | OK | `raw_wire_block` stored at parse, replayed at send |
| `redacted_thinking` round-tripped byte-identical | OK | `raw_wire_block` stored at parse, replayed at send |
| Developer-role → `system[]` (never `messages[]`) | OK | `extract_developer_blocks` + `build_system_blocks` |
| System-role messages skipped from `messages[]` | OK | `continue` in `conversation_to_anthropic_messages` |
| Parallel tool calls split and re-paired | OK | S-021 normalization |
| Image content gated by model capability | OK | `supports_image` flag with text placeholder fallback |
| Truncated tool_use JSON → max_tokens override | OK | S-004 detection at both content_block_stop and message_stop |
| `FunctionCallOutput.success` → `is_error` | MISSING | GAP-11: failed tool results not flagged with `is_error: true` |
| Variant exhaustiveness guard | OK | Compile-time match exhaustiveness test on `ResponseItem` and `ToolSpec` |
| Trailing assistant guard (S-014) | OK | Synthetic user message appended for Vertex AI compatibility |
| History cache control sliding window | OK | 4-slot: last system + last tool + 2nd-to-last user + last user |

### Gap registry

Canonical list of known fidelity gaps. Each gap has a severity, a
description, and a recommended fix. Gaps are closed by implementing
the fix and updating this table (status → CLOSED, add closing sortie).

| ID | Severity | Gap | Impact | Fix | Status |
|----|----------|-----|--------|-----|--------|
| GAP-1 | HIGH | `pause_turn` stop_reason passes through but harness does not auto-continue | Model output silently truncated on long turns | Detect `pause_turn` in turn loop, feed response back as next input to continue generation | OPEN |
| GAP-2 | LOW-MED | `stop_details` (refusal) not parsed; `refusal` stop_reason not surfaced to user | User gets no explanation when refusal occurs | Parse `stop_details` from `message_delta`, surface `category` + `explanation` in UI | OPEN |
| GAP-3 | MEDIUM | `thinking.display` parameter not wired (`summarized` vs `omitted`) | Cannot opt into token-saving omitted mode; always defaults to `summarized` | Add `thinking_display` to config; wire into `anthropic_thinking_param` | OPEN |
| GAP-4 | LOW-MED | `output_config.effort` missing `max` value; only wired for Copilot CAPI path | Cannot request maximum reasoning depth on Anthropic-direct path | Add `Max` to `ReasoningEffort` enum; wire `output_config.effort` for Anthropic-direct | OPEN |
| GAP-5 | MEDIUM | Server tool content blocks not parsed in SSE (server_tool_use, web_search, web_fetch, code_execution, container_upload, tool_search_result) | Results from server-side tools silently dropped | Add BlockState variants for each server tool type; emit appropriate ResponseItems | OPEN |
| GAP-6 | LOW | Top-level `cache_control` request field not wired | No functional impact — manual placement on system blocks works correctly | Add field to `MessagesApiRequest`; let Anthropic handle placement automatically | OPEN |
| GAP-7 | LOW | `container`, `inference_geo`, `service_tier` request fields not wired | Cannot use code execution containers, geographic routing, or tier selection | Add fields to `MessagesApiRequest` and config surface | OPEN |
| GAP-8 | LOW | `server_tool_use`, `inference_geo`, `service_tier`, `cache_creation` (TTL breakdown) usage fields not parsed | Loss of cost visibility for server tools and fine-grained cache breakdown | Add fields to `AnthropicUsage`; propagate to `TokenUsage` | OPEN |
| GAP-9 | LOW-MED | `output_config.format` (JSON schema structured output) not wired | Cannot request structured JSON output from Claude via `JSONOutputFormatParam` | Add `format` to `output_config` construction; expose via config | OPEN |
| GAP-10 | LOW | `container` response field (id + expires_at) not parsed from message or delta | Cannot track container lifecycle for persistent code execution | Parse `container` from `message_start` and `message_delta`; propagate through `ResponseEvent` | OPEN |
| GAP-11 | LOW-MED | `FunctionCallOutput.success` not mapped to Anthropic's `is_error` field on `tool_result` | Failed tool results sent without `is_error: true` — model cannot distinguish tool failure from success | Map `success: Some(false)` → `"is_error": true` on `tool_result` blocks in `conversation_to_anthropic_messages` | OPEN |

### Known code issues (from 2026-04-29 audit)

| ID | Severity | Issue | File | Fix |
|----|----------|-------|------|-----|
| NEW-1 | MEDIUM | `anthropic_max_output_tokens` uses `starts_with("claude")` but `is_anthropic_model` uses `contains("claude")` — proxy-prefixed slugs like `anthropic/claude-opus-4-7` get 64K cap instead of 128K | `provider-anthropic/src/model.rs` | Align slug matching: use `contains` for both, or normalize slug before cap lookup |
| NEW-4 | INFO | `thinking.display` is now GA since SDK v0.85 (2026-03-16) — elevates GAP-3 priority | `provider-anthropic/src/model.rs` | Wire `display` field on both `adaptive` and `enabled` thinking configs |
| NEW-5 | INFO | SDK `ToolUnionParam` has 16 variants vs XLI's 5 — 8 new server tool types, memory tool | N/A | Server tools are correctly filtered out in `tools_to_anthropic_format` (we don't send them); but we can't request them either |
| NEW-6 | LOW | `message_delta` now includes `container` and `stop_details` fields — silently ignored by SSE parser | `codex-api/src/sse/messages.rs` | Extract `delta.stop_details` and `delta.container` in `message_delta` handler |

### Audit procedure

When running a fidelity audit (after SDK updates or major refactors):

1. Pull latest SDKs: `cd ~/Projects/anthropic-sdk-python && git pull origin main`
   and same for TypeScript.
2. Check SDK changelogs for wire-relevant changes:
   `cd ~/Projects/anthropic-sdk-python && cat CHANGELOG.md | head -200`
3. Cross-reference `message_create_params.py` fields against
   `MessagesApiRequest` and `build_messages_request`.
4. Cross-reference `content_block.py` union variants against
   `BlockState` enum in `codex-api/src/sse/messages.rs`.
5. Cross-reference `stop_reason.py` literals against `message_delta`
   handler in SSE parser.
6. Cross-reference `usage.py` fields against `AnthropicUsage` struct.
7. Cross-reference `thinking_config_param.py` variants against
   `anthropic_thinking_param` in `provider-anthropic/src/model.rs`.
8. Update the fidelity matrices and gap registry in this file.
9. Record the audit date and SDK versions at the top of this section.

---

## Tests

| Kind | Command | What it covers |
|------|---------|----------------|
| Unit | `cargo test -p codex-provider-anthropic` | Request build, tool-choice mapping, system-block assembly, wire translator, provider trait impls. ~96 tests. |
| Unit | `cargo test -p codex-model-provider` | `MessagesBackend` trait defaults, `ProviderStreamRequest`, builder, default `stream` refuses. ~13 tests. |
| Unit | `cargo test -p codex-core --lib 'client::tests'` | Legacy tests that reach through `super::anthropic_thinking_param` etc. ~34 tests. |
| Live | `cargo test -p codex-core --test all -- live_messages --ignored --test-threads=1` | End-to-end against a real Anthropic endpoint. |
| Proxy e2e | `CODEX_PROXY_E2E=1 CODEX_PROXY_BASE_URL=... cargo test -p codex-exec --test proxy_e2e_messages -- --test-threads=1` | Full stack: LiteLLM → Vertex AI → Claude. |

## Anti-patterns (will fail review)

- Constructing `MessagesApiRequest` anywhere outside
  `provider-anthropic/src/request.rs`.
- Calling `conversation_to_anthropic_messages`, `extract_developer_blocks`,
  or `tools_to_anthropic_format` from `codex-core`. Those live in
  `codex-provider-anthropic` and the re-export shim was deleted in
  Commit 5b.
- Adding a fat method to `MessagesBackend`. The trait has one method
  by design — every proposed second method is likely a core internal
  leaking through the seam. Re-examine whether the work belongs on
  the provider side instead.
- Leaking `ContentBlock` or `MessageStreamEvent` past
  `map_response_stream` (the core→session boundary).
- Building system prompts as user messages.
- Dropping `tool_use_id` on tool-use → tool-result round-trip.
- Forgetting that this path serves Copilot-routed Messages until
  Commit 5c lands. The `matches!(provider.info().wire_api,
  WireApi::Copilot)` guard inside `run_messages_turn` is load-bearing
  until Copilot owns its own stream.
- Closing a gap without updating the fidelity matrices and gap
  registry in this file. Every wire change must be reflected here.
