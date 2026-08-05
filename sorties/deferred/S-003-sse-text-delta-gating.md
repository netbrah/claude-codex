# S-003 — SSE text_delta Gating Fix

**Priority:** 🔴 Ship-Blocking
**Complexity:** Small (1-2 hours)
**Files:** `codex-rs/codex-api/src/sse/messages.rs:276-289`
**Upstream Risk:** ZERO — this is a new file

## Problem

`text_delta` events are emitted unconditionally for ALL block indices, even when the block index is untracked in `tracker.blocks`. The `thinking_delta` and `input_json_delta` handlers are correctly gated behind `if let Some(state) = tracker.blocks.get_mut(&index)` — but `text_delta` is not.

This means if the SSE stream references a block index that was never initialized via `content_block_start`, XLI emits a `OutputTextDelta` event with orphaned text. This is a data corruption risk.

## Root Cause

Code review finding #7 (MEDIUM). The three delta handlers have asymmetric gating:

```rust
// CORRECT — thinking_delta is gated:
"thinking_delta" => {
    if let Some(BlockState::Thinking { .. }) = tracker.blocks.get_mut(&index) {
        // only emits if block is tracked
    }
}

// BUG — text_delta is NOT gated:
"text_delta" => {
    // emits OutputTextDelta regardless of whether block index is tracked
    let _ = tx_event.send(Ok(ResponseEvent::OutputTextDelta(text))).await;
}
```

## Fix

Move the `OutputTextDelta` send inside the `BlockState::Text` guard, matching the pattern used by `thinking_delta` and `input_json_delta`.

```rust
"text_delta" => {
    if let Some(BlockState::Text { .. }) = tracker.blocks.get_mut(&index) {
        let _ = tx_event.send(Ok(ResponseEvent::OutputTextDelta(text))).await;
    } else {
        trace!("text_delta for untracked block index {index}, ignoring");
    }
}
```

## Upstream Compatibility

✅ This file (`sse/messages.rs`) is entirely new — no upstream conflict possible.

## Tests

Add to existing SSE parser tests:
1. Test: `text_delta` with valid tracked block → emitted
2. Test: `text_delta` with untracked block index → NOT emitted
3. Test: verify `thinking_delta` and `input_json_delta` gating still works (regression guard)

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-api
cargo test -p codex-api -- sse
```
