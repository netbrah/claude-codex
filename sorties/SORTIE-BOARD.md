# XLI Sortie Board — 2026-04-04 (Rehydrated)

**Priority:** Wire enhancements > Cross-pollination ports > Feature builds > Ecosystem research
**Strategic thesis:** XLI is a codex clone. Upstream pulls must stay painless. The /messages wire is our novel layer — implement it in Rust, keep it isolated, test it thoroughly.
**Last rehydrated:** 2026-04-04 — verified against git log on `dev` and `feat/xli-embed-assets`

---

## Upstream Compatibility Doctrine

XLI is a fork of `openai/codex`. Every code change MUST respect:

1. **New files preferred over modified files.** Our /messages wire lives in `messages_wire.rs`, `sse/messages.rs`, `endpoint/messages.rs` — all NEW files. Upstream never touches them. Zero conflict.
2. **Modified upstream files are conflict zones.** `client.rs` (20 upstream commits), `spec.rs` (94 commits), `codex.rs` (137 commits) — changes here WILL conflict. Minimize, isolate behind `WireApi::Messages` guards.
3. **Additive enum variants are viral.** `WireApi::Messages` forces every `match` to handle it. This is accepted debt — the `WireApi` enum is our integration point.
4. **Test isolation.** All /messages tests in separate files (`messages_wire_regression_tests.rs`, `proxy_e2e_messages.rs`). Never interleave with upstream test files.
5. **Feature flags over conditionals.** New capabilities behind `experimental_supported_tools`, config flags, or `WireApi` dispatch. Never modify upstream behavior paths.

---

## Current State (2026-04-04 — Rehydrated from git)

### What's on dev (verified 2026-04-04)


| Layer                              | Content                                                           | Status                 | Upstream Conflict Risk    |
| ---------------------------------- | ----------------------------------------------------------------- | ---------------------- | ------------------------- |
| /messages wire (CA-2..CA-8)        | `messages_wire.rs`, `sse/messages.rs`, `endpoint/messages.rs`     | ✅ Landed               | ZERO (new files)          |
| S-004 StreamingToolCallParser      | Truncated tool_use detection in Rust SSE parser                   | ✅ Landed (`64b0fb64d`) | ZERO (new file)           |
| S-005 Orphaned tool call cleanup   | `clean_orphaned_tool_calls()` in messages_wire.rs                 | ✅ Landed (`bc8cb5ba5`) | ZERO (new file)           |
| S-008 Modality gating              | Image→text placeholder for non-image models                       | ✅ Landed (`1347649ff`) | ZERO (new file)           |
| S-014 trailing assistant guard     | Vertex 400 fix                                                    | ✅ Landed (`e07760b9a`) | LOW                       |
| S-020 Unit tests (Sub-A/B)         | Comprehensive translator + client tests                           | ✅ Landed (`eba35c17c`) | ZERO (test files)         |
| S-030 C++ intelligence (CA-9)      | `analyze_symbol_source`, `clang_graph`, manifest, workspace_index | ✅ Landed (`b73fdf671`) | ZERO (new files)          |
| S-040 XLI proprietary deploy layer | `~/.xli` home isolation + branding                                | ✅ Landed (`31ef63f54`) | ZERO (proprietary branch) |
| S-041 grep_files cleanup           | Removed dead `top_subdirs` and `grep_files_tests`                 | ✅ Landed (`903c35049`) | ZERO                      |
| S-042 Tool registry refactor       | `analyze_symbol_source` extracted to codex-tools crate            | ✅ Landed (`1d4f503a8`) | LOW                       |
| Rebrand: core                      | Binary, home dir, CLI, build, TUI → xli                           | ✅ Landed (`3793ee797`) | Proprietary               |
| Rebrand: config/protocol           | Config, protocol, state, util renames                             | ✅ Landed (`42572f045`) | Proprietary               |
| Rebrand: sandbox/platform          | Sandbox and platform layers                                       | ✅ Landed (`de6b63936`) | Proprietary               |
| Rebrand: test suite                | Test suite codex → xli + e2e validation                           | ✅ Landed (`c8ead247e`) | Proprietary               |
| Upstream openai/codex (Apr 2)      | Second absorption — additional upstream commits                   | ✅ Landed (`99f79fa71`) | ABSORBED                  |
| Dependabot bumps                   | Rust toolchain 1.94.1, sentry, uuid, ts-rs, etc.                  | ✅ 12 PRs merged        | Housekeeping              |


### Branch Status


| Branch                  | HEAD                       | State                                                          |
| ----------------------- | -------------------------- | -------------------------------------------------------------- |
| `dev`                   | `f81ca201f`                | Active — all sorties merged + dependabot + upstream + rebrand  |
| `feat/xli-embed-assets` | `c8ead247e`                | Behind dev — dev has dependabot + second upstream merge on top |
| `main`                  | `51835ce31` (PR #13 merge) | Tracks dev via PR merges                                       |


### Null Space Status (Updated)


| Wire                 | Gaps               | Critical | Actionable                                  | Δ from initial board           |
| -------------------- | ------------------ | -------- | ------------------------------------------- | ------------------------------ |
| /messages (request)  | 3 wire + 6 SDK-new | None     | `stop_sequences` config wiring              | 0 change                       |
| /messages (response) | 2                  | None     | `server_tool_use` parsing                   | 0 change                       |
| /messages (harness)  | **0** (was 3)      | None     | —                                           | **S-004, S-005, S-008 CLOSED** |
| /messages (thinking) | 2                  | None     | By-design (`adaptive` supersedes `enabled`) | 0 change                       |
| /messages (tools)    | 4 SDK-new          | None     | Deferred until server support               | 0 change                       |
| /messages (usage)    | 5 SDK-new          | None     | Telemetry-only                              | 0 change                       |


### Test Coverage (Updated)

- **147+ tests** (S-020 Sub-A/B added comprehensive translator + client tests)
- ✅ `is_anthropic_model()`, `anthropic_thinking_param()`, `effective_wire_api()` — now tested (S-020)
- ✅ `InputImage` modality gating — now tested (S-008)
- ✅ Orphaned tool call cleanup — now tested (S-005)
- ✅ Truncated tool_use detection — now tested (S-004)
- Remaining gap: SSE text_delta gating (S-003), rate limit 429 mapping (S-004-fix), `output_to_text()` image handling (S-005-fix)

---

## Sortie Queue (Rehydrated — reflects actual git state)

### Tier 0 — Remaining Ship-Blocking Fixes (from Code Review)


| ID        | Sortie                            | Category    | Complexity | File(s)                    | Impact                                                                                  |
| --------- | --------------------------------- | ----------- | ---------- | -------------------------- | --------------------------------------------------------------------------------------- |
| S-003     | SSE text_delta gating fix         | Correctness | Small      | `sse/messages.rs:276-289`  | Emits deltas for untracked blocks — data corruption risk                                |
| S-004-fix | Rate limit 429 → retryable        | Correctness | Small      | `api_bridge.rs:122-125`    | ALL 429s mapped to non-retryable `RetryLimit` — sessions crash on transient rate limits |
| S-005-fix | `output_to_text()` image handling | Correctness | Small      | `messages_wire.rs:341-358` | Silently drops image content from tool results                                          |


> **NOTE:** S-020 (unit tests) is ✅ COMPLETE — landed in `eba35c17c`. The three items above are the remaining ship-blocking issues from the code review.

### Tier 1 — Cross-Pollination Ports (Apex → XLI) — REMAINING

> **S-004 (StreamingToolCallParser), S-005 (orphan cleanup), S-008 (modality gating) are ✅ COMPLETE** — all landed on `feat/xli-embed-assets` and merged to dev.


| ID            | Sortie                         | Category    | Complexity | Source                        | Impact                                                                  |
| ------------- | ------------------------------ | ----------- | ---------- | ----------------------------- | ----------------------------------------------------------------------- |
| PORT-LOOP     | Loop detection                 | Safety      | Medium     | `loopDetectionService.ts`     | Prevents infinite tool loops (XLI runs until context exhaustion)        |
| PORT-OMIT     | Omission placeholder detector  | Correctness | Small      | Apex FQ-14                    | Prevents `// ... existing code ...` silent truncation                   |
| PORT-TRIM     | Pre-send context budget trim   | Resilience  | Medium     | `contextBudgetTrim.ts`        | Prevents 400 errors on Sonnet 200K; XLI relies on compaction-on-failure |
| PORT-MASK     | Tool output masking            | Efficiency  | Medium     | `toolOutputMaskingService.ts` | Masks large tool outputs (>50K tokens) — critical for Sonnet sub-agents |
| PORT-PARALLEL | Read-only tool parallelization | Performance | Medium     | `coreToolScheduler.ts`        | 3x wall-clock speedup on codebase exploration                           |


### Tier 2 — Wire Enhancements (Now the active frontier)


| ID              | Sortie                                     | Category | Complexity | Impact                                             |
| --------------- | ------------------------------------------ | -------- | ---------- | -------------------------------------------------- |
| S-016           | Wire `stop_sequences` from config          | Wire     | Small      | Currently hardcoded `None` at `client.rs:1270`     |
| S-SERVER-TOOLS  | Parse `server_tool_use` blocks             | Wire     | Medium     | Unblocks memory tool + tool_search                 |
| S-MEMORY        | Wire `memory_20250818` tool                | Wire     | Medium     | Cross-session persistent notebook (live on Vertex) |
| S-TOOL-SEARCH   | Wire `tool_search_bm25`                    | Wire     | Small      | On-demand tool discovery — reduces token overhead  |
| S-CACHE-TTL     | `cache_control.ttl: "1h"`                  | Wire     | Small      | Extended cache lifetimes for long Opus sessions    |
| S-OUTPUT-CONFIG | `output_config.format` (structured output) | Wire     | Small      | JSON schema-constrained model output               |


### Tier 3 — Architecture


| ID    | Sortie                          | Category | Complexity | Impact                                                     |
| ----- | ------------------------------- | -------- | ---------- | ---------------------------------------------------------- |
| S-031 | codex-lsp-server crate skeleton | Feature  | Large      | C++ intelligence via LSP — S-030 landed, this is unblocked |


### Tier 4 — Feature Builds (from Enhancement Spec)


| ID   | Sortie                                     | Category          | Complexity | Enhancement Spec |
| ---- | ------------------------------------------ | ----------------- | ---------- | ---------------- |
| E-01 | Markdown rendering panic-safe fallback     | TUI Stability     | Low        | §2.3             |
| E-02 | TUI history retention / memory bounding    | TUI Stability     | Low        | §2.2             |
| E-03 | Block cache with LRU eviction              | TUI Performance   | Low        | §2.1             |
| E-04 | Usage display / /stats command             | TUI Observability | Low        | §2.9 / §5.7      |
| E-05 | /export conversation to Markdown/JSON      | Feature           | Low        | §5.1             |
| E-06 | Session vectorization (core search engine) | Feature           | Very High  | §3.2             |
| E-13 | Plugin / agent registry system             | Feature           | Medium     | §1.1             |


### Tier 5 — Ecosystem Research


| ID      | Sortie                                         | Category | Complexity | Source                          |
| ------- | ---------------------------------------------- | -------- | ---------- | ------------------------------- |
| XLI-S10 | Clone + analyze anthropic-sdk-rust SSE parser  | Research | Medium     | `refs/anthropic-sdk-rust`       |
| XLI-S11 | Clone + analyze rig agent loop architecture    | Research | Medium     | `refs/rig`                      |
| XLI-S12 | Compare rig tool dispatch vs XLI tool dispatch | Analysis | Medium     | Dep: XLI-S11                    |
| XLI-S13 | Extract adk-rust multi-model routing pattern   | Research | Medium     | `refs/adk-rust` (via swarms-rs) |


---

## Parallel-Safe Groups (Updated)

- **Group A** (no file overlap): S-003 + PORT-LOOP + PORT-OMIT + S-016 + E-01
- **Group B** (messages_wire.rs contention): PORT-TRIM, PORT-MASK — run sequentially
- **Group C** (sse/messages.rs contention): S-003, S-SERVER-TOOLS — run sequentially
- **Group D** (client.rs contention): S-016, S-MEMORY — run sequentially
- **Group E** (independent): PORT-PARALLEL, S-031, XLI-S10, XLI-S11

---

## Completed Sorties (Full Ledger)

### Wave 1 — Wire Closure (Sprint 1)


| ID           | Sortie                                     | Date           | Commit      | Result                                 |
| ------------ | ------------------------------------------ | -------------- | ----------- | -------------------------------------- |
| W-1..W-7     | Sprint 1 wire closure                      | 2026-03-25..30 | Multiple    | ✅ All merged to dev                    |
| S-001..S-003 | Developer role, token calc, model matching | 2026-03-30     | Multiple    | ✅ On dev                               |
| S-006, S-009 | Beta headers, model matching               | 2026-03-30     | `8b4ce7d13` | ✅ On dev                               |
| S-013        | live_messages.rs e2e                       | 2026-03-30     | —           | ✅ On dev                               |
| S-014        | Trailing assistant guard                   | 2026-03-30     | `e07760b9a` | ✅ On dev (updated unconditional guard) |
| S-015        | Proxy model mismatch suppression           | 2026-03-30     | `5b5cdc72e` | ✅ Merged                               |
| S-018, S-019 | max_tokens uncap, cache_control            | 2026-03-30     | `9d8224e10` | ✅ On dev                               |
| S-020 Sub-C  | 7 SSE parser + integration tests           | 2026-03-30     | —           | ✅ On dev                               |


### Wave 2 — Cross-Pollination + Harness Hardening


| ID            | Sortie                                         | Date       | Commit      | Result                                              |
| ------------- | ---------------------------------------------- | ---------- | ----------- | --------------------------------------------------- |
| XLI-S1        | Security scrub (.mcp.json, symlink, corp refs) | 2026-03-31 | 3 commits   | ✅ CRITICAL+HIGH fixed                               |
| S-004         | Port StreamingToolCallParser (TS → Rust)       | Post 03-31 | `64b0fb64d` | ✅ Truncated tool_use detection implemented          |
| S-005         | Port orphaned tool call cleanup (TS → Rust)    | Post 03-31 | `bc8cb5ba5` | ✅ `clean_orphaned_tool_calls()` in messages_wire.rs |
| S-008         | Modality gating (image→text placeholder)       | Post 03-31 | `1347649ff` | ✅ Non-image model image replacement                 |
| S-020 Sub-A/B | Comprehensive translator + client tests        | Post 03-31 | `eba35c17c` | ✅ 18+ tests added                                   |


### Wave 3 — Architecture + Upstream Adaptation


| ID    | Sortie                                           | Date       | Commit      | Result                                |
| ----- | ------------------------------------------------ | ---------- | ----------- | ------------------------------------- |
| S-030 | C++ intelligence payload (merged to dev)         | Pre 03-31  | `b73fdf671` | ✅ Surgical extraction landed          |
| S-041 | Remove dead `top_subdirs` and `grep_files_tests` | Post 03-31 | `903c35049` | ✅ Dead code cleaned (Option 3 chosen) |
| S-042 | Extract `analyze_symbol_source` to codex-tools   | Post 03-31 | `1d4f503a8` | ✅ Moved to upstream crate pattern     |


### Wave 4 — XLI Rebrand + Home Isolation


| ID        | Sortie                                   | Date       | Commit      | Result                               |
| --------- | ---------------------------------------- | ---------- | ----------- | ------------------------------------ |
| S-040     | XLI proprietary deploy layer + branding  | Post 03-31 | `31ef63f54` | ✅ `~/.xli` home, branding, deploy    |
| S-040a    | TUI env-driven branding (CODEX_APP_NAME) | Post 03-31 | `71ac3b962` | ✅ Upstream-compatible branding hooks |
| S-040b    | XLI ASCII banner + version intercept     | Post 03-31 | `a6cee2db6` | ✅ Proprietary branding layer         |
| S-040c    | `~/.xli` home with CODEX_HOME bridge     | Post 03-31 | `e2e4a4e9a` | ✅ Home isolation working             |
| REBRAND-1 | Core: binary, home dir, CLI, build, TUI  | Post 03-31 | `3793ee797` | ✅ Full codex → xli rename            |
| REBRAND-2 | Config, protocol, state, util renames    | Post 03-31 | `42572f045` | ✅ Comprehensive rename               |
| REBRAND-3 | Sandbox and platform layers              | Post 03-31 | `de6b63936` | ✅ All platform layers renamed        |
| REBRAND-4 | Test suite + e2e validation              | Post 03-31 | `c8ead247e` | ✅ Tests pass under xli branding      |
| REBRAND-5 | Apply rebrand to new upstream files      | Post 04-02 | `34e3737ea` | ✅ Post-upstream-merge rebrand        |


### Upstream Absorptions


| ID             | Sortie                                | Date       | Commit                   | Result                                         |
| -------------- | ------------------------------------- | ---------- | ------------------------ | ---------------------------------------------- |
| UPSTREAM-1     | Absorb openai/codex 160 commits       | 2026-03-30 | `b5c468b01`              | ✅ Sweep 5 complete                             |
| UPSTREAM-2     | Second upstream absorption (Apr 2)    | 2026-04-02 | `99f79fa71`              | ✅ codex-config extraction, tool registry, etc. |
| UPSTREAM-3     | Third upstream merge (PR #13)         | 2026-04-04 | `51835ce31`              | ✅ Via copilot/merge-upstream-openai-codex      |
| FIX-POST-MERGE | Resolve post-merge compilation errors | 2026-04-04 | `55048e522`, `51791b1ce` | ✅ JsonSchema import + test fixes               |
| FIX-TS-RS      | Update ts-rs 12.x API calls           | 2026-04-04 | `f81ca201f`              | ✅ export_all_to → export_all                   |


### Housekeeping


| ID                 | Detail                                                         | Date       | Result       |
| ------------------ | -------------------------------------------------------------- | ---------- | ------------ |
| DEPENDABOT         | 12 dependency PRs (rust toolchain, sentry, uuid, ts-rs, etc.)  | 2026-04-04 | ✅ All merged |
| FIX-MERGE-BREAKAGE | Resolve sortie merge breakage + upstream API signature updates | Post 03-31 | `ee44eafa6`  |
| SCHEMA-REGEN       | Regenerate config schema after upstream merge                  | Post 03-31 | `844cfcc8d`  |


## Failed / Retry Needed


| ID     | Sortie                                       | Issue        | Status                                                                  |
| ------ | -------------------------------------------- | ------------ | ----------------------------------------------------------------------- |
| XLI-S2 | /messages wire unit tests (original attempt) | Auth failure | ✅ SUPERSEDED — S-020 Sub-A/B landed successfully via different approach |


---

## Anthropic /messages API — Systematic Investigation Summary

The /messages wire is XLI's novel contribution. Every field has been audited against the Anthropic SDK TypeScript source (`refs/anthropic-sdk-typescript/src/resources/messages/messages.ts`).

### What We Wire (Complete)


| Field                                                    | Status          | Sortie     |
| -------------------------------------------------------- | --------------- | ---------- |
| `model`, `messages`, `max_tokens`, `stream`              | ✅ Wired         | Foundation |
| `system` (base + developer role + cache_control)         | ✅ Wired         | W-7, S-019 |
| `tools` (function-only filter + cache_control on last)   | ✅ Wired         | Foundation |
| `tool_choice` (auto/any/tool{name}/none)                 | ✅ Wired         | W-2        |
| `thinking` (`{type: "adaptive"}`)                        | ✅ Wired         | S-017      |
| `temperature`, `top_p`, `top_k`                          | ✅ Wired         | W-3        |
| `metadata.user_id`                                       | ✅ Wired         | W-5        |
| `anthropic-beta: interleaved-thinking`                   | ✅ Dynamic       | S-009      |
| All SSE events (8 types, 4 content blocks, 4 deltas)     | ✅ Parsed        | Foundation |
| `stop_reason` (all 4 + generic passthrough)              | ✅ Propagated    | W-1        |
| `cache_creation_input_tokens`                            | ✅ Surfaced      | W-6        |
| Developer role → system[] injection                      | ✅ Wired         | W-7        |
| Thinking blocks (`raw_wire_block` byte-identical replay) | ✅ Preserved     | Foundation |
| Redacted thinking (opaque data round-trip)               | ✅ Preserved     | Foundation |
| Thinking strip (non-latest turns)                        | ✅ Token savings | Foundation |


### What We Don't Wire (Prioritized)


| Field                                                     | Severity | Gate            | Sortie          |
| --------------------------------------------------------- | -------- | --------------- | --------------- |
| `stop_sequences` (hardcoded `None`)                       | MEDIUM   | Config gap      | S-016           |
| `server_tool_use` blocks                                  | MEDIUM   | Vertex live     | S-SERVER-TOOLS  |
| `cache_control.ttl`                                       | MEDIUM   | SDK-new         | S-CACHE-TTL     |
| `output_config.format`                                    | LOW      | SDK-new         | S-OUTPUT-CONFIG |
| `service_tier`                                            | LOW      | SDK-new         | Deferred        |
| `container`                                               | LOW      | Vertex rejected | Deferred        |
| `inference_geo`                                           | LOW      | SDK-new         | Deferred        |
| `thinking.display: "omitted"`                             | LOW      | SDK-new         | Deferred        |
| Tool fields: `defer_loading`, `strict`, `allowed_callers` | LOW      | SDK-new         | Deferred        |
| Usage fields: `server_tool_use`, `inference_geo`, etc.    | INFO     | Telemetry       | Deferred        |


### What Apex Has That We Don't (Cross-Pollination)


| Feature                                  | Apex Impl                                 | XLI Gap                         | Port Sortie     |
| ---------------------------------------- | ----------------------------------------- | ------------------------------- | --------------- |
| Orphaned tool call cleanup               | `cleanOrphanedToolCalls()`                | Sends orphans → 400 error       | S-005           |
| Truncated tool call detection            | `StreamingToolCallParser`                 | Corrupt JSON args               | S-004           |
| Pre-send context budget trim             | `trimAnthropicMessagesForContextBudget()` | Relies on compaction-on-failure | PORT-TRIM       |
| Tool output masking (>50K tokens)        | `toolOutputMaskingService.ts`             | Sends full outputs every turn   | PORT-MASK       |
| Read-only tool parallelization           | `coreToolScheduler.ts`                    | Sequential execution            | PORT-PARALLEL   |
| Loop detection (5-repetition breaker)    | `loopDetectionService.ts`                 | Runs until context exhaustion   | PORT-LOOP       |
| Omission placeholder detector            | FQ-14                                     | Silent code truncation          | PORT-OMIT       |
| Modality gating (image→text placeholder) | `modalityDefaults.ts`                     | API error on text-only models   | S-008           |
| Schema compliance modes                  | `schemaConverter.ts`                      | Raw passthrough                 | PORT-SCHEMA     |
| Cache control on user messages           | `addCacheControlToMessages()`             | System + tools only             | PORT-CACHE-USER |


### What We Have That Apex Doesn't (XLI Advantages)


| Feature                                  | XLI Impl                   | Apex Gap                                               |
| ---------------------------------------- | -------------------------- | ------------------------------------------------------ |
| Adaptive thinking (`{type: "adaptive"}`) | `client.rs:2124`           | Uses `enabled` with fixed budget (deprecation warning) |
| `raw_wire_block` byte-identical replay   | `messages_wire.rs:155`     | Decompose→reconstruct may invalidate signatures        |
| Redacted thinking full round-trip        | `messages_wire.rs:163-168` | `data` blob lost (multi-turn broken)                   |
| Thinking strip (non-latest turns)        | `messages_wire.rs:260-297` | All thinking replayed every turn                       |
| Developer role injection                 | W-7                        | No concept in Apex                                     |
| `tool_choice` on /messages               | W-2                        | Never sent                                             |
| `metadata.user_id` on /messages          | W-5                        | Never sent                                             |
| Guardian LLM-based approval              | `guardian/mod.rs`          | Rule-based only                                        |
| OS-level sandboxing (seatbelt/landlock)  | Native                     | None                                                   |
| Exec policy engine                       | `exec_policy.rs`           | Simpler regex-based                                    |


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

