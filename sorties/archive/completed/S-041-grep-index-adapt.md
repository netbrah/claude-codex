> **STATUS: ✅ COMPLETE** — This sortie has been implemented and merged. Kept for reference.

# S-041 — Adapt grep_files Index to codex-tools Architecture

**Priority:** 🟡 P2
**Complexity:** Medium (2-3 hours)
**Target:** Investigation → code changes
**Upstream Risk:** MEDIUM — this addresses an upstream deletion

## Problem

Upstream deleted `codex-rs/core/src/tools/handlers/grep_files.rs` (commit `178c3b15b`) as part of the `codex-tools` crate extraction. Our manifest-based workspace index (`search_rg.rs`, `manifest_builder.rs`, `workspace_index.rs`, `dir_stats.rs`) lost its integration point — these modules still compile but `grep_files.rs` (which called them) is gone.

## Investigation Phase

Read these upstream files FIRST to understand the new architecture:

| File | Purpose |
|------|---------|
| `codex-rs/tools/src/lib.rs` | Upstream's tool crate entry point |
| `codex-rs/tools/src/local_tool.rs` | May contain the new grep implementation |
| `codex-rs/tools/src/utility_tool.rs` | Upstream utility tools |
| `codex-rs/tools/src/tool_spec.rs` | How tool specs are defined now |
| `codex-rs/core/src/tools/spec.rs` | Registration site (how tools get wired) |

## Our Index Modules (still compile, need new host)

| Module | Purpose |
|--------|---------|
| `workspace_index.rs` | Background indexer — builds file manifest |
| `manifest_builder.rs` | Builds file manifest from git/walk |
| `search_rg.rs` | Manifest-filtered ripgrep search |
| `dir_stats.rs` | File count estimation for large repos |
| `analyze_symbol_source.rs` | Already uses the index (working) |

## Options (pick based on investigation)

### Option 1: Hook into upstream's grep handler
If `codex-tools` has a handler with a search function, inject our index as an optimization layer. Our manifest pre-filters the file list before rg runs, scoping searches to relevant files.

### Option 2: Re-add grep_files.rs
Create a new handler that wraps upstream's grep with our index pre-filter. Register it alongside or instead of upstream's grep.

### Option 3: Make analyze_symbol_source the sole index consumer
If grep is handled entirely by upstream now and our index only serves `analyze_symbol_source` (which it already does), declare victory. The index modules remain but only `analyze_symbol_source` calls them.

## Upstream Compatibility

**This is the critical upstream concern.** Our approach must not fight the `codex-tools` extraction. Options ranked by upstream compatibility:
- Option 3: BEST — no upstream conflict at all
- Option 1: GOOD — additive optimization, upstream grep still works
- Option 2: RISKY — re-adding a deleted file will conflict on next upstream pull

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-core
cargo test -p codex-core -- analyze_symbol  # verify index still works
cargo test -p codex-tools                    # upstream tool tests pass
```
