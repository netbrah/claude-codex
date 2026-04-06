> **STATUS: ✅ COMPLETE** — This sortie has been implemented and merged. Kept for reference.

# S-004 — Port StreamingToolCallParser (TS → Rust)

**Priority:** 🟠 P1 — Cross-Pollination
**Complexity:** Medium (4-6 hours)
**Source:** `cli-ops/spokes/qwen-code/packages/core/src/core/openaiContentGenerator/streamingToolCallParser.ts`
**Target:** `codex-rs/codex-api/src/sse/messages.rs`
**Upstream Risk:** ZERO — target is a new file

## Context

This is implementing a fundamentally novel capability in Rust — truncated tool call detection in the /messages SSE stream. The TS implementation exists in Apex but is for the /responses wire. We're adapting the concept for the Anthropic /messages wire protocol.

### Why This Is Novel

The Anthropic /messages SSE protocol streams tool call arguments via `input_json_delta` events. If the model hits `max_tokens` mid-tool-call, the argument JSON is truncated. XLI currently uses this truncated JSON as-is (falling back to `{}` for malformed JSON at `messages_wire.rs:72-74`). This causes:
- Silent tool call corruption
- Unpredictable tool execution with partial arguments
- No retry signal to the harness

## Objective

When `message_stop` arrives, check if any `ToolUse` blocks have incomplete/invalid JSON arguments. If so, signal truncation so the harness can retry.

## Implementation

```rust
// In process_messages_sse, when handling message_stop:
for (_, state) in &tracker.blocks {
    if let BlockState::ToolUse { arguments, .. } = state {
        if serde_json::from_str::<serde_json::Value>(arguments).is_err() {
            // Signal truncation to the harness
            // Option A: emit a new ResponseEvent variant
            // Option B: set a flag on the existing Completed event
        }
    }
}
```

### Design Decision: How to Signal Truncation

**Option A** (preferred): Add `ResponseEvent::TruncatedToolCall { call_id, name }` variant. The harness receives this before `Completed` and can decide to retry.

**Option B**: Add `truncated_tool_calls: Vec<String>` to `ResponseEvent::Completed`. Simpler but requires the harness to check a field on every completion.

**Upstream impact of Option A**: Adding a `ResponseEvent` variant is additive. Upstream `match` statements will need a catch-all `_` or explicit handler, but since they don't use the Messages wire, this is safe.

## Source Reference (Read-Only)

Study in the TS source:
- `isCompleteJson()` — JSON completeness heuristic
- How truncation is detected at stream end
- How retry is signaled to the caller

Key insight: Anthropic correctly reports `stop_reason: max_tokens` on truncation, so we can also check stop_reason. But the JSON validation is a defense-in-depth — catches cases where stop_reason is misleading.

## Tests

| # | Test | Expected |
|---|------|----------|
| 1 | Complete tool_use JSON → no truncation signal | `Completed` without truncation |
| 2 | Incomplete tool_use JSON (truncated mid-object) → truncation detected | `TruncatedToolCall` emitted |
| 3 | Multiple tool_use blocks, one truncated → truncation detected | Signal for the truncated one |
| 4 | Tool_use with empty arguments `""` → handled gracefully | Not truncation (empty is valid intent) |
| 5 | Tool_use with `stop_reason: max_tokens` + valid JSON → no false positive | Only signal on actual bad JSON |

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-api
cargo test -p codex-api -- sse
cargo test -p codex-core messages_wire  # regression check
```
