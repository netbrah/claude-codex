# Anthropic `/messages` Wire-Fidelity Audit

**Date:** 2026-04-29
**Auditor:** Apex (Opus 4.7)
**Scope:** Full turn lifecycle on the Anthropic `/messages` wire after the Sortie 1 provider extraction. Goal: prove no context — thinking, tool calls, system instructions, signatures, cache breakpoints — is being silently dropped between `ResponseItem[]` and the model.
**Sources cross-referenced:**
- `~/Projects/xli/codex-rs/{provider-anthropic,codex-api,core}` (live impl)
- `~/Projects/anthropic-sdk-python/src/anthropic/types/` (canonical Stainless-generated spec)
- `~/Projects/anthropic-sdk-typescript/src/resources/messages/`
- `~/Projects/cli-ops/docs/null-space-ops/` and `docs/architecture/` (sortie-board priors)

---

## Verdict

**The wire is healthy on the critical path.** Thinking traces, signatures, redacted_thinking, tool_use_id continuity, system extraction, prompt-cache breakpoints, and orphan cleanup all round-trip correctly. The Sortie 1 hybrid split (provider owns request build + SSE translation; core owns transport + retry + Copilot splice) is correctly enforced and the `MessagesBackend` seam is honored.

**There are no critical-path silent drops handicapping the model.**

There are, however, **non-critical-path silent drops** (telemetry math, optional response metadata, deferred SDK features) and **SDK-feature deferments** (1h cache TTL, server tools, structured outputs, MCP) that warrant ranked follow-ups. None require an architecture change.

---

## 1. SDK upgrade question — answered

**No SDK to upgrade.** `provider-anthropic/Cargo.toml` does not depend on any vendored Anthropic crate. We hand-roll JSON via `serde_json::Value`. The reference is the live API spec mirrored in `~/Projects/anthropic-sdk-python/src/anthropic/types/`.

**Implication:** every new Anthropic feature is a deliberate carve, not a free dependency bump. We've fallen behind the spec by ~12 months on optional surface, but the load-bearing core is intact.

---

## 2. Wire abstraction — answered

**The hybrid split is correct.** Sortie 1 cleanly separated:

```
provider-anthropic                 |  codex-core
-----------------------------------+----------------------------------
build_messages_request (pure)      |  run_messages_turn (401 loop)
build_messages_extra_headers       |  current_client_setup
conversation_to_anthropic_messages |  Copilot /v1 splice + beta header
extract_developer_blocks           |  ApiMessagesClient construction
tools_to_anthropic_format          |  map_response_stream
build_system_blocks                |  handle_unauthorized_for_copilot
AnthropicMessagesProvider::stream  |  MessagesBackendAdapter
                                   |
            connected by MessagesBackend (one method)
```

Re-reading `request.rs`, `wire.rs::conversation_to_anthropic_messages`, `provider.rs::stream`, `sse/messages.rs`, and `core::run_messages_turn`, the boundary is honored. No core internals leak through the seam; no transport logic in the provider. Skill invariants 1–10 all upheld.

The shared body type `MessagesApiRequest` (in `codex-api`, not `provider-anthropic`) is consumed by both sides — that is correct, not a leak.

**No abstraction change needed.**

---

## 3. Critical-path fidelity (✅ confirmed intact)

| Surface | Status | Evidence |
|---|---|---|
| Developer-block injection into `system[]` (BREAK-1 / W-7) | ✅ | `wire.rs:485 extract_developer_blocks` + `request.rs:96 build_system_blocks` |
| `cache_control: ephemeral` on last system block | ✅ | `request.rs:188-192` |
| `cache_control` on last tool def | ✅ | `wire.rs:826-830` |
| Sliding-window cache_control on last 2 user msgs (slots 3+4) | ✅ | `wire.rs::apply_history_cache_control` |
| `thinking` blocks: `thinking_delta` + `signature_delta` accumulated, packaged into `raw_wire_block` for byte-identical replay | ✅ | `sse/messages.rs:299-339`, `wire.rs:240-247` |
| `redacted_thinking.data` round-trip via `\0REDACTED\0` sentinel | ✅ | `sse/messages.rs:438-461`, `wire.rs:259-267` |
| `tool_use_id` round-trip through `tool_result` | ✅ | `wire.rs:175-186` |
| `input_json_delta` accumulated, JSON-validated, truncation→`max_tokens` retry (S-004) | ✅ | `sse/messages.rs:333-345, 354-377` |
| Strip thinking from non-latest assistant msgs (Anthropic invariant) | ✅ | `wire.rs::strip_thinking_from_non_latest_assistant_messages` |
| Orphan tool_use/result cleanup (S-005 + S-021 adjacency) | ✅ | `wire.rs:19, 525` (closes BREAK-5 from cli-ops) |
| Vertex no-prefill guard (S-014) | ✅ | `wire.rs::conversation_to_anthropic_messages` tail |
| Adaptive thinking via `{type: "adaptive"}` | ✅ | `model.rs::anthropic_thinking_param` |
| `interleaved-thinking-2025-05-14` beta when thinking is set | ✅ | `endpoint/messages.rs:120-122` |
| `prompt-caching-2024-07-31` beta always-on | ✅ | `endpoint/messages.rs:117` |
| Tool-choice variants `auto`/`any`/`tool{name}` | ✅ | `request.rs::messages_api_tool_choice` |
| `metadata.user_id` plumbed | ✅ | `request.rs:107-109` |
| Image base64 + URL passthrough (with modality gating placeholder) | ✅ | `wire.rs:117-141` |
| Tool-result with mixed image/text content | ✅ | `wire.rs::output_to_content` |
| `ping` event correctly ignored | ✅ | `sse/messages.rs:582` |
| `error` event terminates stream with typed `ApiError` | ✅ | `sse/messages.rs:585-602` |

Every surface the model needs to maintain coherent context across a turn — thinking traces, signatures, tool_use_id continuity, system instructions, cache breakpoints — is round-tripped correctly.

---

## 4. Silent drops — ranked by impact

### 🔴 P0 — Token-usage math is wrong (telemetry silent corruption)

**File:** `codex-rs/codex-api/src/sse/messages.rs:536`

```rust
total_tokens: input + cached + output,  // ❌ omits cache_creation_input_tokens
```

Anthropic's `input_tokens` is **uncached** input only (verified at `~/Projects/anthropic-sdk-python/src/anthropic/types/usage.py:26-27`). The correct sum is:

```
input_tokens
  + cache_read_input_tokens
  + cache_creation_input_tokens
  + output_tokens
```

Our `total_tokens` undercounts by exactly `cache_creation_input_tokens` on every turn that creates new cache entries (every turn where the developer prefix or last user message shifts). The test fixture at `messages.rs:1006` enshrines the wrong math (asserts 192 = 100 + 50 + 0 + 42 when 25 cache_creation tokens should bring the total to 217).

**Fix:** one-line change + update fixtures + add a regression test.

### 🟠 P1 — `stop_sequences` field plumbing missing

**File:** `codex-rs/provider-anthropic/src/request.rs:133`

```rust
stop_sequences: None,  // ❌ hardcoded
```

The `MessagesApiRequest.stop_sequences` field exists; nothing reaches it. `Sampling` doesn't carry it; `ProviderStreamRequest` doesn't carry it. Callers cannot set stop sequences on the Messages wire. Low blast-radius (most callers don't use stop_sequences) but it's a silent gap — config that flows on the Responses wire is dropped on Messages.

### 🟠 P1 — `pause_turn` and `refusal` stop reasons unhandled

`stop_reason` is propagated as a free-form `Option<String>` through `ResponseEvent::Completed` (`sse/messages.rs:540`), so the value isn't lost mid-flight. **But** nothing downstream knows what to do with:

- `pause_turn` — long-running server-tool turn. Resume protocol: replay the response back as-is. Until plumbed, the harness will treat it like `end_turn` and the user sees a half-finished turn.
- `refusal` — Anthropic's safety stop. Carries `Message.stop_details = {category: "cyber"|"bio"|null, explanation: str}`. We don't parse `stop_details` off `message_delta.delta`, so the user gets no explanation.

Both are recent additions (≤12 mo) that a 2024-vintage integration would not handle.

### 🟠 P1 — `server_tool_use` blocks silently ignored

**File:** `codex-rs/codex-api/src/sse/messages.rs:264`

Falls through with `trace!("ignoring unknown content_block type: ...")` for:
- `server_tool_use`
- `web_search_tool_result`
- `web_fetch_tool_result`
- `code_execution_tool_result`
- `bash_code_execution_tool_result`
- `text_editor_code_execution_tool_result`
- `tool_search_tool_result`
- `mcp_tool_use` / `mcp_tool_result`
- `container_upload`

Today this is mostly moot because (a) we don't enable server-tools in our request, (b) Vertex/Copilot proxies block them. **But** if a user routes direct-Anthropic and turns on `web_search_20250305`, the model's web-search results vanish and multi-turn breaks because the assistant message in history is missing the block the model expects to see echoed back.

If we ever ship server-tools, this becomes P0 and needs the same `raw_wire_block` treatment we gave thinking.

### 🟡 P2 — `usage.cache_creation` breakdown (5m vs 1h) dropped

**File:** `codex-rs/codex-api/src/sse/messages.rs:588-593`

```rust
struct AnthropicUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
    // ❌ missing: cache_creation { ephemeral_5m, ephemeral_1h }
    // ❌ missing: server_tool_use { web_search_requests, web_fetch_requests }
    // ❌ missing: service_tier { standard|priority|batch }
    // ❌ missing: inference_geo
}
```

All silently dropped at deserialization. Doesn't break the model — but the moment we enable 1h cache TTL we have **zero observability** on whether 1h-cached entries are hitting.

### 🟡 P2 — `citations_delta` SSE event unhandled

**File:** `codex-rs/codex-api/src/sse/messages.rs:347` (fall-through in `content_block_delta`)

If we ingest a PDF/document with `citations: {enabled: true}`, the citation deltas attached to text chunks are dropped. The text comes through (because `text_delta` works), but cited-source metadata is lost. Today this is moot (we don't send `document` blocks), but it's a surface-area gap.

### 🟡 P2 — Cache-TTL hardcoded to 5m

Every `cache_control` we emit is `{"type": "ephemeral"}` (implicit 5m). The 1h TTL (`{"type":"ephemeral","ttl":"1h"}` + `extended-cache-ttl-2025-04-11` beta) is unused. For interactive sessions ≥5m apart (lunch break), we re-pay full input cost.

Apex enforces a 4-block cap on cache_control breakpoints (commit `6b612bb7a` in their tree); we should mirror that when we wire 1h.

### 🟡 P2 — `tool_choice: {type:"none"}` falls through to `auto`

**File:** `codex-rs/provider-anthropic/src/request.rs:122-127`

Documented invariant ("caller arranges for `has_tools == false`") isn't enforced anywhere in our pipeline. If a caller passes `ToolChoice::None` with non-empty tools, we silently send `tool_choice: auto` and the model is free to call them.

Two valid resolutions: emit `{type: "none"}` for real (Anthropic supports it), or assert/strip tools when `ToolChoice::None`.

### 🟢 P3 — SDK-feature deferments (intentional, document only)

Not bugs; carves we haven't done. Listed for completeness:

| Feature | Beta header | Notes |
|---|---|---|
| `service_tier: "auto"\|"standard_only"` request | none | cost knob |
| `output_config: {effort, format: json_schema}` | none / `effort-2025-11-24` | structured outputs without round-tripping Responses |
| `container` request field + skills | `skills-2025-10-02` | code-execution attach |
| `context_management: {edits: [...]}` | `context-management-2025-06-27` | server-side history compaction |
| `mcp_servers[]` + mcp_tool_use/result blocks | `mcp-client-2025-11-20` | MCP via Anthropic |
| `speed: "fast"` | `fast-mode-2026-02-01` | new |
| `user_profile_id` | `user-profiles-2026-03-24` | new |
| Per-tool `strict`, `defer_loading`, `eager_input_streaming`, `allowed_callers`, `input_examples` | per-tool | tool quality knobs |
| Document blocks (PDF, plain_text, content, URL-PDF) + citations | `pdfs-2024-09-25` | input modality gap |
| `image source: {type:"file", file_id}` (Files API) | `files-api-2025-04-14` | input modality gap |
| `search_result` content block | none | agentic-search inputs |
| `top_k`, `top_p`, `temperature` | n/a | ✅ already plumbed via `Sampling` |

---

## 5. Anti-patterns we are NOT exhibiting

Worth recording because these are common failure modes the audit specifically looked for:

- ✅ Not constructing `MessagesApiRequest` outside `provider-anthropic/src/request.rs`.
- ✅ Not calling `conversation_to_anthropic_messages` / `extract_developer_blocks` / `tools_to_anthropic_format` from `codex-core`.
- ✅ Not building system prompts as user messages.
- ✅ Not dropping `tool_use_id` on round-trip.
- ✅ `MessagesBackend` has exactly one method — boundary discipline held.
- ✅ Adaptive thinking (`{type:"adaptive"}`) is the modern variant — not the older `{type:"enabled", budget_tokens: N}`.
- ✅ `raw_wire_block` fast-path for byte-identical thinking replay is present and correct (the multi-turn-thinking failure mode that breaks Apex `converter.ts` is closed in our tree).
- ✅ Per-provider retry policy (not the test-fixture `retry_429: false`) governs production.

---

## 6. Recommended action list

In priority order. None require an architecture change.

| # | Priority | Effort | Action |
|---|----------|--------|--------|
| 1 | P0 | 30 min | Fix `total_tokens` math in `sse/messages.rs:536` to include `cache_creation_input_tokens`. Update fixture at line 1006. Add regression test asserting `total = input + cache_read + cache_creation + output`. |
| 2 | P1 | ~1 hr | Plumb `stop_sequences`: add to `Sampling` or `ProviderStreamRequest`, thread through `build_messages_request`, set `request.stop_sequences = ...` instead of `None`. |
| 3 | P1 | ~2 hrs | Extend `AnthropicUsage` to deserialize `cache_creation` (5m/1h breakdown), `server_tool_use`, `service_tier`, `inference_geo`. Plumb at minimum `service_tier` and the 1h-cache breakdown into `TokenUsage`. |
| 4 | P1 | ~3 hrs | Handle `pause_turn` and `refusal` stop reasons. Parse `message_delta.delta.stop_details` into a typed struct. Surface `RefusalStopDetails` through `ResponseEvent::Completed`. Decide a policy for `pause_turn` (suggest: hard error with clear log + follow-up to implement resume-on-pause). |
| 5 | P2 | ~4 hrs | When we light up `web_search` or `code_execution`: extend `BlockState` with `ServerToolUse` and `…ToolResult` variants, preserve raw JSON in `raw_wire_block` so multi-turn replay works. **Do not ship server tools until this lands.** |
| 6 | P2 | ~2 hrs | Add `citations_delta` accumulator → attach citation array to the text block's metadata. Same pattern as `signature_delta`. |
| 7 | P2 | when needed | Add 1h cache TTL: thread an enum through `apply_history_cache_control` / `build_system_blocks`, emit `{"type":"ephemeral","ttl":"1h"}`, send `extended-cache-ttl-2025-04-11` beta. Cap to 4 cache_control breakpoints. |
| 8 | Polish | ~30 min | Tighten `ToolChoice::None`: either map to real `{type:"none"}` or strip tools. |

---

## 7. Stale claims from cli-ops to retire

These cli-ops null-space notes were valid at sortie-board time but are now closed by the Sortie 1 lift:

| Claim | Source | Status |
|---|---|---|
| "stop_reason discarded after parse" | MASTER.md W-1 | ❌ closed; `Completed.stop_reason` carries it |
| "response `model` not extracted from `message_start`" | CROSS-WIRE-2026-04-11 | ❌ closed; `ResponseEvent::ServerModel` emits it |
| "tool_choice hardcoded `{type:auto}`" | MASTER.md W-2 | ❌ closed; `messages_api_tool_choice` switches all variants |
| "metadata.user_id missing from struct" | MASTER.md W-5 | ❌ closed; `MessagesApiMetadata` exists and is wired |
| "BREAK-5: no orphan tool-call cleanup on /messages" | null-space/04 | ❌ closed; S-005 + S-021 both run |
| "BREAK-2: total_tokens math wrong" | null-space/04 | ⚠️ **still open** — see §4 P0 |
| "cache_creation_input_tokens telemetry-blind" | MASTER.md W-6 | ⚠️ partially open — primary field plumbed; 5m/1h breakdown not |
| "server_tool_use silently ignored" | CROSS-WIRE-2026-04-11 | ⚠️ **still open** — see §4 P1 |
| "redacted_thinking multi-turn broken" | NULL-SPACE-2026-04-11 | ❌ closed for XLI (raw_wire_block fast-path); still open for Apex per cli-ops notes — Apex-side problem, not ours |

---

## 8. Reference — Anthropic SDK feature inventory (late 2025 / early 2026)

Sourced from `~/Projects/anthropic-sdk-python/src/anthropic/types/`. Citations are `file:line`.

### Request fields not plumbed in XLI

- `service_tier` (`message_create_params.py:121`)
- `cache_control` top-level (`:116`)
- `container` (`:122`)
- `inference_geo` (`:124`)
- `output_config` (`:133`)
- Beta-only: `betas`, `mcp_servers`, `context_management`, `speed`, `user_profile_id`

### Output content blocks not handled in XLI SSE parser

- `server_tool_use` (7 server-tool name variants)
- `web_search_tool_result`, `web_fetch_tool_result`
- `code_execution_tool_result`, `bash_code_execution_tool_result`, `text_editor_code_execution_tool_result`
- `tool_search_tool_result`
- `container_upload`
- Beta-only: `mcp_tool_use`, `mcp_tool_result`

### SSE delta types not handled

- `citations_delta` (5 location variants: char_location, page_location, content_block_location, search_result_location, web_search_result_location)

### Usage fields not deserialized

- `cache_creation: { ephemeral_5m_input_tokens, ephemeral_1h_input_tokens }`
- `server_tool_use: { web_search_requests, web_fetch_requests }`
- `service_tier: standard|priority|batch`
- `inference_geo`

### Stop reasons not handled

- `pause_turn`
- `refusal` (with `Message.stop_details = RefusalStopDetails`)

### Tool param fields not plumbed

- `strict`, `defer_loading`, `eager_input_streaming`, `allowed_callers`, `input_examples`

### Beta headers not in our rotation

- `extended-cache-ttl-2025-04-11` (1h cache)
- `pdfs-2024-09-25`
- `files-api-2025-04-14`
- `code-execution-2025-05-22`
- `mcp-client-2025-11-20`
- `context-management-2025-06-27`
- `context-1m-2025-08-07`
- `skills-2025-10-02`
- `fast-mode-2026-02-01`
- `output-300k-2026-03-24`
- `user-profiles-2026-03-24`

---

## 9. Files inspected

```
codex-rs/provider-anthropic/src/lib.rs
codex-rs/provider-anthropic/src/model.rs
codex-rs/provider-anthropic/src/provider.rs
codex-rs/provider-anthropic/src/regression_tests.rs
codex-rs/provider-anthropic/src/request.rs
codex-rs/provider-anthropic/src/wire.rs
codex-rs/codex-api/src/endpoint/messages.rs
codex-rs/codex-api/src/sse/messages.rs
codex-rs/core/src/client.rs (run_messages_turn, stream_messages_api, MessagesBackendAdapter)
codex-rs/model-provider/src/stream.rs (MessagesBackend trait)
~/Projects/anthropic-sdk-python/src/anthropic/types/ (canonical reference)
~/Projects/cli-ops/docs/null-space-ops/ (sortie-board priors)
~/Projects/cli-ops/docs/architecture/01-wire-protocol-matrix.md
~/Projects/cli-ops/docs/feature-delta/{02,03}*.md
```
