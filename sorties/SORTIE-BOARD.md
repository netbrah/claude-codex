# XLI Sortie Board — 2026-04-04

**Priority:** Ship-blocking fixes > Cross-pollination ports > Feature builds > Ecosystem research
**Strategic thesis:** XLI is a codex clone. Upstream pulls must stay painless. The /messages wire is our novel layer — implement it in Rust, keep it isolated, test it thoroughly.

---

## Upstream Compatibility Doctrine

XLI is a fork of `openai/codex`. Every code change MUST respect:

1. **New files preferred over modified files.** Our /messages wire lives in `messages_wire.rs`, `sse/messages.rs`, `endpoint/messages.rs` — all NEW files. Upstream never touches them. Zero conflict.
2. **Modified upstream files are conflict zones.** `client.rs` (20 upstream commits), `spec.rs` (94 commits), `codex.rs` (137 commits) — changes here WILL conflict. Minimize, isolate behind `WireApi::Messages` guards.
3. **Additive enum variants are viral.** `WireApi::Messages` forces every `match` to handle it. This is accepted debt — the `WireApi` enum is our integration point.
4. **Test isolation.** All /messages tests in separate files (`messages_wire_regression_tests.rs`, `proxy_e2e_messages.rs`). Never interleave with upstream test files.
5. **Feature flags over conditionals.** New capabilities behind `experimental_supported_tools`, config flags, or `WireApi` dispatch. Never modify upstream behavior paths.

---

## Current State (2026-04-04)

### What's on dev

| Layer | Content | Lines | Upstream Conflict Risk |
|-------|---------|-------|----------------------|
| /messages wire (CA-2..CA-8) | `messages_wire.rs`, `sse/messages.rs`, `endpoint/messages.rs` | ~3,290 | ZERO (new files) |
| S-014 trailing assistant guard | Vertex 400 fix | 22 | LOW |
| S-018 max_tokens uncap | Opus 128K output | 1 | LOW |
| S-019 cache_control optimization | Last system block | 5 | LOW |
| S-030 C++ intelligence (CA-9) | `analyze_symbol_source`, `clang_graph`, `manifest`, `workspace_index` | 5,277 | MEDIUM (grep_files deleted upstream) |
| Upstream openai/codex | codex-tools, core-skills, analytics, plugins, spawn v2, etc. | 57K+ | ABSORBED (sweep 5) |

### Null Space Status

| Wire | Gaps | Critical | Actionable |
|------|------|----------|-----------|
| /messages (request) | 3 wire + 6 SDK-new | None | `stop_sequences` config wiring |
| /messages (response) | 2 | None | `server_tool_use` parsing |
| /messages (thinking) | 2 | None | By-design (`adaptive` supersedes `enabled`) |
| /messages (tools) | 4 SDK-new | None | Deferred until server support |
| /messages (usage) | 5 SDK-new | None | Telemetry-only |

### Test Coverage

- **147 tests** across 12 files
- Strong: wire translation, SSE parsing, thinking blocks, stop reasons
- Gap: `is_anthropic_model()`, `anthropic_thinking_param()`, `effective_wire_api()`, `InputImage` — ZERO unit tests (S-020)

---

## Sortie Queue

### Tier 0 — Ship-Blocking Fixes (from Code Review)

| ID | Sortie | Category | Complexity | File(s) | Impact |
|----|--------|----------|------------|---------|--------|
| S-003 | SSE text_delta gating fix | Correctness | Small | `sse/messages.rs:276-289` | Emits deltas for untracked blocks — data corruption risk |
| S-004-fix | Rate limit 429 → retryable | Correctness | Small | `api_bridge.rs:122-125` | ALL 429s mapped to non-retryable `RetryLimit` — sessions crash on transient rate limits |
| S-005-fix | `output_to_text()` image handling | Correctness | Small | `messages_wire.rs:341-358` | Silently drops image content from tool results |
| S-020 | /messages wire unit tests (Sub-A/B) | Testing | Medium | `messages_wire.rs`, `client_tests.rs` | Core routing functions have zero tests |

### Tier 1 — Cross-Pollination Ports (Apex → XLI)

| ID | Sortie | Category | Complexity | Source | Impact |
|----|--------|----------|------------|--------|--------|
| S-004 | Port StreamingToolCallParser (TS → Rust) | Correctness | Medium | `streamingToolCallParser.ts` (442 lines) | Prevents silent tool call corruption on context overflow |
| S-005 | Port orphaned tool call cleanup (TS → Rust) | Correctness | Medium | `converter.ts:cleanOrphanedToolCalls()` | Prevents Anthropic API 400 errors from unpaired tool_use/tool_result |
| S-008 | Modality gating to /messages wire | Correctness | Medium | `modalityDefaults.ts` | Prevents API errors when images sent to text-only models |
| PORT-LOOP | Loop detection | Safety | Medium | `loopDetectionService.ts` | Prevents infinite tool loops (XLI runs until context exhaustion) |
| PORT-OMIT | Omission placeholder detector | Correctness | Small | Apex FQ-14 | Prevents `// ... existing code ...` silent truncation |
| PORT-TRIM | Pre-send context budget trim | Resilience | Medium | `contextBudgetTrim.ts` | Prevents 400 errors on Sonnet 200K; XLI relies on compaction-on-failure |
| PORT-MASK | Tool output masking | Efficiency | Medium | `toolOutputMaskingService.ts` | Masks large tool outputs (>50K tokens) — critical for Sonnet sub-agents |
| PORT-PARALLEL | Read-only tool parallelization | Performance | Medium | `coreToolScheduler.ts` | 3x wall-clock speedup on codebase exploration |

### Tier 2 — Upstream Adaptation + Architecture

| ID | Sortie | Category | Complexity | Impact |
|----|--------|----------|------------|--------|
| S-040 | XLI rebrand / home isolation (`~/.xli`) | Branding | Small | Proprietary branch only — no engine changes |
| S-041 | Adapt grep_files index to codex-tools | Architecture | Medium | Upstream deleted `grep_files.rs` — our index needs new host |
| S-042 | Wire analyze_symbol_source into tool registry | Architecture | Small | Move from `spec.rs` experimental block to `codex-tools` crate |
| S-030-MERGE | Merge S-030 feat branch into dev | Merge | Medium | Unblocks S-031 (LSP server) |
| S-031 | codex-lsp-server crate skeleton | Feature | Large | C++ intelligence via LSP — blocked on S-030 |

### Tier 3 — Wire Enhancements

| ID | Sortie | Category | Complexity | Impact |
|----|--------|----------|------------|--------|
| S-016 | Wire `stop_sequences` from config | Wire | Small | Currently hardcoded `None` at `client.rs:1270` |
| S-SERVER-TOOLS | Parse `server_tool_use` blocks | Wire | Medium | Unblocks memory tool + tool_search |
| S-MEMORY | Wire `memory_20250818` tool | Wire | Medium | Cross-session persistent notebook (live on Vertex) |
| S-TOOL-SEARCH | Wire `tool_search_bm25` | Wire | Small | On-demand tool discovery — reduces token overhead |
| S-CACHE-TTL | `cache_control.ttl: "1h"` | Wire | Small | Extended cache lifetimes for long Opus sessions |
| S-OUTPUT-CONFIG | `output_config.format` (structured output) | Wire | Small | JSON schema-constrained model output |

### Tier 4 — Feature Builds (from Enhancement Spec)

| ID | Sortie | Category | Complexity | Enhancement Spec |
|----|--------|----------|------------|-----------------|
| E-01 | Markdown rendering panic-safe fallback | TUI Stability | Low | §2.3 |
| E-02 | TUI history retention / memory bounding | TUI Stability | Low | §2.2 |
| E-03 | Block cache with LRU eviction | TUI Performance | Low | §2.1 |
| E-04 | Usage display / /stats command | TUI Observability | Low | §2.9 / §5.7 |
| E-05 | /export conversation to Markdown/JSON | Feature | Low | §5.1 |
| E-06 | Session vectorization (core search engine) | Feature | Very High | §3.2 |
| E-13 | Plugin / agent registry system | Feature | Medium | §1.1 |

### Tier 5 — Ecosystem Research

| ID | Sortie | Category | Complexity | Source |
|----|--------|----------|------------|--------|
| XLI-S10 | Clone + analyze anthropic-sdk-rust SSE parser | Research | Medium | `refs/anthropic-sdk-rust` |
| XLI-S11 | Clone + analyze rig agent loop architecture | Research | Medium | `refs/rig` |
| XLI-S12 | Compare rig tool dispatch vs XLI tool dispatch | Analysis | Medium | Dep: XLI-S11 |
| XLI-S13 | Extract adk-rust multi-model routing pattern | Research | Medium | `refs/adk-rust` (via swarms-rs) |

---

## Parallel-Safe Groups

- **Group A** (no file overlap): S-040 + S-004-fix + S-042 + PORT-LOOP + E-01
- **Group B** (messages_wire.rs contention): S-005, S-008, PORT-TRIM, PORT-MASK — run sequentially
- **Group C** (sse/messages.rs contention): S-003, S-004, S-SERVER-TOOLS — run sequentially
- **Group D** (independent investigation): S-041, XLI-S10, XLI-S11

---

## Completed Sorties

| ID | Sortie | Date | Result |
|----|--------|------|--------|
| W-1..W-7 | Sprint 1 wire closure | 2026-03-25..30 | ✅ All merged to dev |
| S-001..S-003 | Developer role, token calc, model matching | 2026-03-30 | ✅ On dev |
| S-006, S-009 | Beta headers, model matching | 2026-03-30 | ✅ On dev |
| S-013 | live_messages.rs e2e | 2026-03-30 | ✅ On dev |
| S-014 | Trailing assistant guard | 2026-03-30 | ✅ On dev |
| S-015 | Proxy model mismatch suppression | 2026-03-30 | ✅ Merged |
| S-018, S-019 | max_tokens uncap, cache_control | 2026-03-30 | ✅ On dev |
| S-020 Sub-C | 7 SSE parser + integration tests | 2026-03-30 | ✅ On dev |
| S-030 | C++ intelligence extraction (17 files, 5277L) | 2026-03-30 | ✅ On branch (unmerged) |
| XLI-S1 | Security scrub (.mcp.json, symlink, corp refs) | 2026-03-31 | ✅ 3 commits |
| UPSTREAM | Absorb openai/codex 160 commits | 2026-03-30 | ✅ Sweep 5 complete |

## Failed / Retry Needed

| ID | Sortie | Issue | Action |
|----|--------|-------|--------|
| XLI-S2 | /messages wire unit tests | Auth failure | Retry with correct proxy config |

---

## Anthropic /messages API — Systematic Investigation Summary

The /messages wire is XLI's novel contribution. Every field has been audited against the Anthropic SDK TypeScript source (`refs/anthropic-sdk-typescript/src/resources/messages/messages.ts`).

### What We Wire (Complete)

| Field | Status | Sortie |
|-------|--------|--------|
| `model`, `messages`, `max_tokens`, `stream` | ✅ Wired | Foundation |
| `system` (base + developer role + cache_control) | ✅ Wired | W-7, S-019 |
| `tools` (function-only filter + cache_control on last) | ✅ Wired | Foundation |
| `tool_choice` (auto/any/tool{name}/none) | ✅ Wired | W-2 |
| `thinking` (`{type: "adaptive"}`) | ✅ Wired | S-017 |
| `temperature`, `top_p`, `top_k` | ✅ Wired | W-3 |
| `metadata.user_id` | ✅ Wired | W-5 |
| `anthropic-beta: interleaved-thinking` | ✅ Dynamic | S-009 |
| All SSE events (8 types, 4 content blocks, 4 deltas) | ✅ Parsed | Foundation |
| `stop_reason` (all 4 + generic passthrough) | ✅ Propagated | W-1 |
| `cache_creation_input_tokens` | ✅ Surfaced | W-6 |
| Developer role → system[] injection | ✅ Wired | W-7 |
| Thinking blocks (`raw_wire_block` byte-identical replay) | ✅ Preserved | Foundation |
| Redacted thinking (opaque data round-trip) | ✅ Preserved | Foundation |
| Thinking strip (non-latest turns) | ✅ Token savings | Foundation |

### What We Don't Wire (Prioritized)

| Field | Severity | Gate | Sortie |
|-------|----------|------|--------|
| `stop_sequences` (hardcoded `None`) | MEDIUM | Config gap | S-016 |
| `server_tool_use` blocks | MEDIUM | Vertex live | S-SERVER-TOOLS |
| `cache_control.ttl` | MEDIUM | SDK-new | S-CACHE-TTL |
| `output_config.format` | LOW | SDK-new | S-OUTPUT-CONFIG |
| `service_tier` | LOW | SDK-new | Deferred |
| `container` | LOW | Vertex rejected | Deferred |
| `inference_geo` | LOW | SDK-new | Deferred |
| `thinking.display: "omitted"` | LOW | SDK-new | Deferred |
| Tool fields: `defer_loading`, `strict`, `allowed_callers` | LOW | SDK-new | Deferred |
| Usage fields: `server_tool_use`, `inference_geo`, etc. | INFO | Telemetry | Deferred |

### What Apex Has That We Don't (Cross-Pollination)

| Feature | Apex Impl | XLI Gap | Port Sortie |
|---------|-----------|---------|-------------|
| Orphaned tool call cleanup | `cleanOrphanedToolCalls()` | Sends orphans → 400 error | S-005 |
| Truncated tool call detection | `StreamingToolCallParser` | Corrupt JSON args | S-004 |
| Pre-send context budget trim | `trimAnthropicMessagesForContextBudget()` | Relies on compaction-on-failure | PORT-TRIM |
| Tool output masking (>50K tokens) | `toolOutputMaskingService.ts` | Sends full outputs every turn | PORT-MASK |
| Read-only tool parallelization | `coreToolScheduler.ts` | Sequential execution | PORT-PARALLEL |
| Loop detection (5-repetition breaker) | `loopDetectionService.ts` | Runs until context exhaustion | PORT-LOOP |
| Omission placeholder detector | FQ-14 | Silent code truncation | PORT-OMIT |
| Modality gating (image→text placeholder) | `modalityDefaults.ts` | API error on text-only models | S-008 |
| Schema compliance modes | `schemaConverter.ts` | Raw passthrough | PORT-SCHEMA |
| Cache control on user messages | `addCacheControlToMessages()` | System + tools only | PORT-CACHE-USER |

### What We Have That Apex Doesn't (XLI Advantages)

| Feature | XLI Impl | Apex Gap |
|---------|----------|----------|
| Adaptive thinking (`{type: "adaptive"}`) | `client.rs:2124` | Uses `enabled` with fixed budget (deprecation warning) |
| `raw_wire_block` byte-identical replay | `messages_wire.rs:155` | Decompose→reconstruct may invalidate signatures |
| Redacted thinking full round-trip | `messages_wire.rs:163-168` | `data` blob lost (multi-turn broken) |
| Thinking strip (non-latest turns) | `messages_wire.rs:260-297` | All thinking replayed every turn |
| Developer role injection | W-7 | No concept in Apex |
| `tool_choice` on /messages | W-2 | Never sent |
| `metadata.user_id` on /messages | W-5 | Never sent |
| Guardian LLM-based approval | `guardian/mod.rs` | Rule-based only |
| OS-level sandboxing (seatbelt/landlock) | Native | None |
| Exec policy engine | `exec_policy.rs` | Simpler regex-based |

---

## Evidence Ledger

Every wire change has a live test result. See `cli-ops/docs/null-space-ops/MASTER.md` for full ledger.

Key validations:
- Basic /messages round-trip ✅
- Thinking blocks + signatures ✅
- tool_choice:any → forced tool use ✅
- temperature:0.0 → deterministic ✅
- stop_sequences → stop_reason:stop_sequence ✅
- metadata.user_id → accepted ✅
- memory_20250818 → live on Vertex ✅
- web_search → PROXY BLOCKED ❌
- code_execution → VERTEX REJECTED ❌
- adaptive thinking → VERTEX REJECTED ❌ (uses enabled+budget instead)

