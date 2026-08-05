# S-SERVER-TOOLS — Parse server_tool_use Blocks

**Priority:** 🟡 P2
**Complexity:** Medium (2-4 hours)
**Files:** `codex-rs/codex-api/src/sse/messages.rs`, `codex-rs/core/src/messages_wire.rs`
**Upstream Risk:** LOW — changes in new files

## Problem

When Claude returns `server_tool_use` blocks (memory tool, tool_search), XLI SSE parser logs `trace!("ignoring unknown content_block type")` at `sse/messages.rs:264` and drops the block. This means:
- Memory tool results are lost
- Tool_search results are lost
- Server-side tools are completely non-functional

## Evidence

Memory tool is LIVE TESTED and working on Vertex (evidence ledger 2026-03-25).

## Implementation

### Phase 1: SSE Parser
Add `BlockState::ServerToolUse` variant:

```rust
enum BlockState {
    // ... existing variants ...
    ServerToolUse {
        tool_type: String,    // e.g., "memory_20250818"
        id: String,
        name: String,
        input: String,        // accumulated from input_json_delta
    },
}
```

Handle in content_block_start:
```rust
"server_tool_use" => {
    let id = block_obj.get("id").and_then(|v| v.as_str()).unwrap_or_default();
    let name = block_obj.get("name").and_then(|v| v.as_str()).unwrap_or_default();
    tracker.blocks.insert(index, BlockState::ServerToolUse {
        tool_type: "server_tool_use".into(),
        id: id.into(),
        name: name.into(),
        input: String::new(),
    });
}
```

### Phase 2: ResponseItem Round-Trip
Add handling in `messages_wire.rs` for server_tool_use blocks in conversation history.

## Dependencies

This unblocks:
- S-MEMORY (memory_20250818 tool)
- S-TOOL-SEARCH (tool_search_bm25)
- Sprint 3 from MASTER.md (S-1, S-2, S-3)

## Upstream Compatibility

✅ All changes in new files. ZERO upstream conflict.

## Tests

| # | Test | Expected |
|---|------|----------|
| 1 | SSE stream with server_tool_use block → parsed | BlockState::ServerToolUse created |
| 2 | server_tool_use with input_json_delta → input accumulated | Input built correctly |
| 3 | server_tool_use in history → round-trips through messages_wire | Block preserved |
| 4 | Unknown server tool type → graceful handling | Logged and skipped |

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-api
cargo test -p codex-api -- sse
cargo check -p codex-core
cargo test -p codex-core messages_wire
```
