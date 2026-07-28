# PORT-PARALLEL — Read-Only Tool Parallelization

**Priority:** 🟡 P2 — Cross-Pollination
**Complexity:** Medium (2-3 hours)
**Source:** Apex `coreToolScheduler.ts:1348-1374`
**Target:** `codex-rs/core/src/tools/parallel.rs`
**Upstream Risk:** LOW — existing file, additive change

## Problem

XLI serializes all tool calls, even read-only ones. Apex fires `read_file` + `list_dir` + `grep` concurrently. On codebase exploration (10 files, 5 greps, 3 listings), Apex completes in 1-2 API round-trips while XLI needs 18 sequential calls.

Behavioral analysis confirmed: Apex is 3x+ faster on exploration tasks due to tool parallelization.

## Context

XLI ALREADY has parallel infrastructure in `tools/parallel.rs`:

```rust
let supports_parallel = self.router.tool_supports_parallel(&call.tool_name);
let _guard = if supports_parallel {
    Either::Left(lock.read().await)   // read lock = concurrent
} else {
    Either::Right(lock.write().await) // write lock = exclusive
};
```

The mechanism exists. The gap is: most tools default to non-parallel. We need to mark read-only tools as parallel-safe.

## Implementation

1. In the tool router, mark these tools as `supports_parallel = true`:
   - `read_file` — pure read, no side effects
   - `list_directory` — pure read
   - `search_text` / `grep` — pure read
   - ~~`analyze_symbol_source` — REMOVED~~

2. In `parallel.rs`, ensure the batching logic correctly fires parallel tools concurrently when Claude returns multiple tool calls in one response.

3. On the /messages wire: Claude typically returns one tool call at a time. But with `tool_choice: {type: "any"}`, it can batch. The parallel infrastructure should handle both cases.

## Upstream Compatibility

LOW risk. `parallel.rs` exists in upstream. Changes are additive — marking more tools as parallel-safe.

## Tests

| # | Test | Expected |
|---|------|----------|
| 1 | Two read_file calls → concurrent execution | Both start simultaneously |
| 2 | read_file + write_file → sequential | Write waits for read |
| 3 | Three greps → concurrent | All start simultaneously |
| 4 | Mixed read + write batch → reads parallel, write sequential | Correct ordering |

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-core
cargo test -p codex-core -- parallel
```
