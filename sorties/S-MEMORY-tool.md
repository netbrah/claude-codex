# S-MEMORY — Wire memory_20250818 Tool

**Priority:** 🟡 P2
**Complexity:** Medium (2-3 hours)
**Dependency:** S-SERVER-TOOLS (server_tool_use parsing)
**Files:** `codex-rs/core/src/client.rs`
**Upstream Risk:** LOW — additive config + tool injection

## Problem

Anthropic's `memory_20250818` server-side tool gives Claude a persistent cross-session notebook. It's LIVE TESTED and working on Vertex. But XLI can't use it because:
1. We don't inject the tool definition in the request
2. We don't parse server_tool_use responses (fixed by S-SERVER-TOOLS)

## What Memory Tool Does

Claude writes to a persistent notebook via `create`/`str_replace`/`insert` operations and reads via `view`. The notebook survives across API requests. For ONTAP development, this means Claude remembers subsystem knowledge, coding patterns, and key decisions between sessions.

## Implementation

### Tool Definition Injection

In `client.rs build_tools_list()`, when memory is enabled:

```rust
if config.memory_tool_enabled {
    tools.push(json!({
        "type": "memory_20250818",
        "name": "memory",
    }));
}
```

No `input_schema` required — server-side tools use the `type` field as the capability indicator.

### Config

```toml
# ~/.codex/config.toml or ~/.xli/config.toml
[model]
memory_tool = true  # opt-in, default false
```

### Beta Header

Check if `memory_20250818` requires an `anthropic-beta` header. If so, add to the dynamic header list in `endpoint/messages.rs`.

## Upstream Compatibility

LOW risk. Additive config field + additive tool injection. No upstream code paths affected.

## Tests

| # | Test | Expected |
|---|------|----------|
| 1 | memory_tool_enabled = true → tool in request | Tool definition included |
| 2 | memory_tool_enabled = false → tool not in request | Default behavior |
| 3 | server_tool_use response for memory → parsed | Block handled (dep: S-SERVER-TOOLS) |

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-core
cargo test -p codex-core -- memory
```
