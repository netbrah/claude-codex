# XLI Upstream Compatibility Guide

**Purpose:** Every sortie agent must read this before making changes.
**Core principle:** XLI is a clone of openai/codex. Upstream pulls must stay painless.

---

## The Architecture That Makes This Work

```
codex-rs/
├── codex-api/src/
│   ├── sse/
│   │   ├── responses.rs       ← UPSTREAM (don't touch)
│   │   └── messages.rs        ← OURS (new file, zero conflict)
│   └── endpoint/
│       ├── responses.rs       ← UPSTREAM (don't touch)
│       └── messages.rs        ← OURS (new file, zero conflict)
├── core/src/
│   ├── client.rs              ← UPSTREAM (HIGH CHURN — minimize changes)
│   ├── codex.rs               ← UPSTREAM (HIGHEST CHURN — minimal touch)
│   ├── messages_wire.rs       ← OURS (new file, zero conflict)
│   ├── messages_wire_regression_tests.rs  ← OURS (new file)
│   ├── compact.rs             ← UPSTREAM (moderate churn)
│   ├── model_provider_info.rs ← UPSTREAM (moderate churn)
│   └── tools/
│       ├── spec.rs            ← UPSTREAM (94 commits — danger zone)
│       ├── handlers/
│       │   ├── mod.rs         ← UPSTREAM (24 commits)
│       │   ├── (analyze_symbol_source.rs removed)
│       │   ├── workspace_index.rs        ← OURS (new file)
│       │   ├── manifest_builder.rs       ← OURS (new file)
│       │   ├── search_rg.rs              ← OURS (new file)
│       │   └── dir_stats.rs              ← OURS (new file)
│       └── parallel.rs        ← UPSTREAM (exists)
├── tools/                     ← UPSTREAM codex-tools crate
└── exec/tests/
    ├── proxy_e2e_messages.rs  ← OURS (new file)
    └── ...                    ← UPSTREAM
```

## Rules

### DO:
- ✅ Create NEW files for XLI-specific code
- ✅ Gate changes behind `WireApi::Messages` in shared files
- ✅ Put tests in separate test files (`_regression_tests.rs`, `proxy_e2e_messages.rs`)
- ✅ Use `#[cfg(feature = "...")]` for optional capabilities
- ✅ Add `Option` fields to shared structs (backward compatible)
- ✅ Check upstream HEAD before modifying shared files — are they about to change?

### DON'T:
- ❌ Modify `codex.rs` without absolute necessity (137 upstream commits)
- ❌ Add mandatory fields to shared structs (breaks deserialization)
- ❌ Put XLI-specific tests in upstream test files
- ❌ Re-add files upstream deleted (`grep_files.rs` was deleted — don't recreate it)
- ❌ Change function signatures of upstream functions
- ❌ Add XLI codenames ("XLI", "NetApp", "ONTAP") in public source code

### CAUTION ZONES (modify with care):
- ⚠️ `client.rs` — add Messages-specific code inside `WireApi::Messages` match arms
- ⚠️ `spec.rs` — use experimental_supported_tools gating for new tools
- ⚠️ `compact.rs` — our compaction changes must not affect Responses path
- ⚠️ `protocol.rs` — additive enum variants are viral (accepted debt for `WireApi`)

## Merge Strategy

When upstream pulls arrive:
1. `git fetch upstream && git merge upstream/main` (not rebase — preserve our commit history)
2. Conflicts will be in: `client.rs`, `spec.rs`, `codex.rs`, `Cargo.lock`
3. Our new files NEVER conflict
4. Resolution: take upstream changes, re-apply our additions

## The /messages Wire Is Our Novel Layer

The entire /messages implementation is in NEW files that upstream doesn't have:
- `messages_wire.rs` (1,624 lines) — the translator
- `sse/messages.rs` (~400 lines) — SSE parser
- `endpoint/messages.rs` — HTTP client
- `messages_wire_regression_tests.rs` — test guards

This is fundamentally novel work — implementing the Anthropic /messages wire protocol in Rust, which no one else has done in the codex harness. It's designed to coexist cleanly with upstream's /responses wire.
