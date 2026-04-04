# PORT-LOOP — Loop Detection

**Priority:** 🟠 P1 — Cross-Pollination
**Complexity:** Medium (4-6 hours)
**Source:** Apex `loopDetectionService.ts`
**Target:** `codex-rs/core/src/codex.rs` (or new module `codex-rs/core/src/loop_detection.rs`)
**Upstream Risk:** LOW — new module, small integration point in codex.rs

## Problem

XLI has no loop detection. If Claude gets stuck in an infinite tool loop (read → edit → read → edit → ...), XLI runs until context exhaustion. This can consume the entire 1M context window with repetitive tool calls.

From `codex.rs:5857`: *"as long as compaction works well...we shouldn't worry about being in an infinite loop."* — This is incorrect. Compaction doesn't break loops; it just makes space for more of the same loop.

## Source Analysis

Apex `loopDetectionService.ts` monitors:
- **Tool call repetitions**: threshold=5 (same tool name + similar args 5 times in a row)
- **Content sentence repetitions**: threshold=10 (same sentences repeated 10+ times)
- On detection: auto-breaks the loop, injects a system message "You appear to be in a loop. Please try a different approach."

## Implementation

Create `codex-rs/core/src/loop_detection.rs`:

```rust
pub struct LoopDetector {
    tool_call_history: VecDeque<(String, u64)>,  // (tool_name, args_hash)
    tool_loop_threshold: usize,                    // default: 5
    content_hashes: VecDeque<u64>,                 // hash of assistant content
    content_loop_threshold: usize,                 // default: 10
}

impl LoopDetector {
    pub fn record_tool_call(&mut self, name: &str, args: &str) -> bool {
        let hash = hash_args(args);
        self.tool_call_history.push_back((name.to_string(), hash));
        // Check: are the last N entries identical?
        self.detect_tool_loop()
    }

    pub fn record_content(&mut self, content: &str) -> bool {
        let hash = hash_content(content);
        self.content_hashes.push_back(hash);
        self.detect_content_loop()
    }

    fn detect_tool_loop(&self) -> bool {
        if self.tool_call_history.len() < self.tool_loop_threshold { return false; }
        let last = self.tool_call_history.back().unwrap();
        self.tool_call_history.iter().rev().take(self.tool_loop_threshold)
            .all(|entry| entry == last)
    }
}
```

### Integration Point

In `codex.rs`, after each tool call execution, call `loop_detector.record_tool_call()`. If it returns `true`, inject a loop-breaking message and signal the turn loop to pause.

## Upstream Compatibility

- New file: `loop_detection.rs` — ZERO conflict
- Integration in `codex.rs`: Small — add a field to the session struct and a check after tool execution. This touches an upstream high-churn file but the change is minimal (2-3 lines).
- Alternative: integrate via the hook/lifecycle system if S-040 hooks infrastructure lands first

## Tests

| # | Test | Expected |
|---|------|----------|
| 1 | Same tool call 5 times → loop detected | Returns true |
| 2 | Same tool call 4 times → no detection | Returns false |
| 3 | Different tool calls → no detection | Returns false |
| 4 | Same tool name, different args → no detection | Hash-based, not name-only |
| 5 | Content repetition 10 times → loop detected | Returns true |
| 6 | Reset after loop break → counter resets | Clean state |

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-core
cargo test -p codex-core -- loop_detect
```
