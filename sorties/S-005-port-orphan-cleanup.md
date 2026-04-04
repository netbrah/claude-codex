# S-005 — Port Orphaned Tool Call Cleanup (TS → Rust)

**Priority:** 🟠 P1 — Cross-Pollination
**Complexity:** Medium (2-3 hours)
**Source:** `cli-ops/spokes/qwen-code/packages/core/src/core/openaiContentGenerator/converter.ts` → `cleanOrphanedToolCalls()`
**Target:** `codex-rs/core/src/messages_wire.rs`
**Upstream Risk:** ZERO — target is a new file

## Context

When a session is interrupted (crash, timeout, Ctrl+C), the conversation history can contain:
1. `FunctionCall` / `LocalShellCall` items WITHOUT matching `FunctionCallOutput`
2. `FunctionCallOutput` items WITHOUT preceding `FunctionCall` / `LocalShellCall`

The Anthropic API requires perfectly paired `tool_use` / `tool_result` blocks. Orphans cause 400 errors.

## Why This Is Critical

XLI has no cleanup. Interrupted sessions → orphaned tool calls → Anthropic API 400 → session dies on resume. This is a production correctness issue.

Apex has `cleanOrphanedToolCalls()` in the OpenAI converter but NOT in the Anthropic converter — both harnesses need this fix.

## Implementation

Add a `clean_orphaned_tool_calls()` function to `messages_wire.rs`. Call it at the TOP of `conversation_to_anthropic_messages()`, before translation begins.

```rust
/// Remove unpaired tool_use/tool_result items from conversation history.
/// Matches by call_id: every LocalShellCall/FunctionCall must have a
/// FunctionCallOutput with the same call_id, and vice versa.
fn clean_orphaned_tool_calls(items: &[ResponseItem]) -> Vec<ResponseItem> {
    // Pass 1: collect all call_ids that have both call and output
    let call_ids: HashSet<String> = items.iter().filter_map(|item| {
        match item {
            ResponseItem::FunctionCall { call_id, .. }
            | ResponseItem::LocalShellCall { call_id, .. } => Some(call_id.clone()),
            _ => None,
        }
    }).collect();

    let output_ids: HashSet<String> = items.iter().filter_map(|item| {
        match item {
            ResponseItem::FunctionCallOutput { call_id, .. } => Some(call_id.clone()),
            _ => None,
        }
    }).collect();

    let paired_ids: HashSet<String> = call_ids.intersection(&output_ids).cloned().collect();

    // Pass 2: filter — keep items that are either non-tool or have paired call_ids
    items.iter().filter(|item| {
        match item {
            ResponseItem::FunctionCall { call_id, .. }
            | ResponseItem::LocalShellCall { call_id, .. }
            | ResponseItem::FunctionCallOutput { call_id, .. } => paired_ids.contains(call_id),
            _ => true,  // non-tool items always kept
        }
    }).cloned().collect()
}
```

## Upstream Compatibility

✅ `messages_wire.rs` is a new file. The function is called only from the Messages wire path — upstream /responses path is unaffected.

## Tests

Follow the pattern in `messages_wire_regression_tests.rs`:

| # | Test | Expected |
|---|------|----------|
| 1 | Paired tool call + output → both preserved | No filtering |
| 2 | Orphaned LocalShellCall (no output) → removed | Call removed |
| 3 | Orphaned FunctionCallOutput (no call) → removed | Output removed |
| 4 | Multiple tool calls, one orphaned → only orphan removed | Selective filtering |
| 5 | Empty input → empty output | Edge case |
| 6 | Multiple calls with same name but different call_ids → correct pairing | ID-based, not name-based |

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-core
cargo test -p codex-core messages_wire
cargo test -p codex-core -- orphan
```
