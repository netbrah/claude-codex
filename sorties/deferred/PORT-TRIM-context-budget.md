# PORT-TRIM — Pre-Send Context Budget Trim

**Priority:** 🟠 P1 — Cross-Pollination
**Complexity:** Medium (4-6 hours)
**Source:** Apex `contextBudgetTrim.ts`
**Target:** `codex-rs/core/src/messages_wire.rs` or `codex-rs/core/src/client.rs`
**Upstream Risk:** LOW — either in a new file or isolated in the Messages wire path

## Problem

XLI relies on compaction-on-failure: when history exceeds context limit, Anthropic returns 400, and XLI reactively compacts. This blocks the session while compacting, which on Opus 1M can mean summarizing ~900K tokens — minutes of wait.

Apex trims proactively before each request. History that would exceed context budget is trimmed (old tool results head+tail truncated, oldest pairs dropped). The session never hits a 400.

**Critical for Sonnet sub-agents (200K context):** They hit limits regularly. Without pre-send trim, Sonnet workers crash. With it, they degrade gracefully.

## Implementation

Add `trim_for_context_budget()` to `messages_wire.rs`:

```rust
/// Trim conversation messages to fit within the model's context budget.
/// Called BEFORE building the MessagesApiRequest.
///
/// Strategy (matching Apex contextBudgetTrim.ts):
/// 1. Estimate token count of messages array
/// 2. If under budget (80% of max_input_tokens), return unchanged
/// 3. If over budget:
///    a. Trim large tool results (keep first 500 + last 500 chars)
///    b. Drop oldest user/assistant pairs (preserve last N turns)
///    c. Never touch current turn or last 3 turns
///
/// For Opus 1M: rarely triggers (1M is huge)
/// For Sonnet 200K: critical — worker agents hit limits regularly
pub fn trim_for_context_budget(
    messages: &mut Vec<serde_json::Value>,
    max_input_tokens: usize,
    estimated_system_tokens: usize,
) {
    let budget = (max_input_tokens * 80) / 100;  // 80% budget ratio
    let available = budget.saturating_sub(estimated_system_tokens);

    // Estimate current token count (chars / 4 as rough heuristic)
    let current_estimate = estimate_tokens(messages);
    if current_estimate <= available { return; }

    // Phase 1: Trim large tool results (keep head+tail)
    for msg in messages.iter_mut() {
        if is_tool_result(msg) {
            trim_tool_result(msg, 500, 500);  // keep first 500 + last 500 chars
        }
    }

    // Phase 2: Drop oldest pairs if still over budget
    // Never drop last 3 turns
    while estimate_tokens(messages) > available && messages.len() > 6 {
        // Remove oldest user + assistant pair
        messages.remove(0);
        if !messages.is_empty() { messages.remove(0); }
    }
}
```

### Sacred Tool Results (from MASTER.md doctrine)

```
DO NOT truncate:
  - Any tool result from the current turn
  - Any tool result from the last N turns (configurable, default 3)
  - Any tool result that the model is actively referencing

ONLY trim:
  - Tool results from turns > N ago
  - And only when estimated context > budget_ratio * max_input_tokens
  - Head+tail preservation (keep first 500 + last 500 chars)
```

### Integration Point

Call `trim_for_context_budget()` in `client.rs:stream_messages_api()` AFTER `conversation_to_anthropic_messages()` but BEFORE building the HTTP request.

## Upstream Compatibility

- If added to `messages_wire.rs`: ZERO conflict (new file)
- If integrated in `client.rs`: LOW conflict — small call site addition, gated behind `WireApi::Messages`

## Tests

| # | Test | Expected |
|---|------|----------|
| 1 | Under budget → no changes | Messages unchanged |
| 2 | Over budget, large tool results → results trimmed | Head+tail preserved |
| 3 | Over budget after trim → oldest pairs dropped | Last 3 turns preserved |
| 4 | Way over budget → aggressive trimming, last 3 turns intact | Safety floor |
| 5 | Empty messages → no crash | Edge case |
| 6 | Sacred tool results (last 3 turns) → never trimmed | Doctrine enforcement |

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-core
cargo test -p codex-core messages_wire
cargo test -p codex-core -- trim
```
