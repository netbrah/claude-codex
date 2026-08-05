> **STATUS: ✅ COMPLETE** — This sortie has been implemented and merged. Kept for reference.

# S-042 — Wire analyze_symbol_source into Upstream Tool Registry

**Priority:** 🟢 P3
**Complexity:** Small (1-2 hours)
**Target:** `codex-rs/tools/` crate + `codex-rs/core/src/tools/spec.rs`
**Upstream Risk:** MEDIUM — touches upstream tool crate

## Problem

`analyze_symbol_source` tool spec is currently defined inline in `codex-rs/core/src/tools/spec.rs` in the experimental block. Upstream moved tool specs to the `codex-tools` crate. Our tool should follow the same pattern for consistency and discoverability.

## Current State

- Function `create_analyze_symbol_source_tool()` with function-scoped `use codex_tools::JsonSchema`
- Registration gated by `experimental_supported_tools.contains("analyze_symbol_source")`
- Handler struct `AnalyzeSymbolSourceHandler` in `codex-rs/core/src/tools/handlers/analyze_symbol_source.rs`

## Target Architecture

Match upstream's named tool pattern:

### Files to Create

`codex-rs/tools/src/analyze_symbol_source_tool.rs` — Move tool spec definition here:
```rust
use crate::json_schema::JsonSchema;

pub fn create_analyze_symbol_source_tool() -> ToolDefinition {
    ToolDefinition {
        name: "analyze_symbol_source".to_string(),
        description: "Analyze the source definition and call graph of a C/C++ symbol".to_string(),
        parameters: JsonSchema::Object {
            // ... existing schema ...
        },
    }
}
```

### Files to Modify

| File | Change |
|------|--------|
| `codex-rs/tools/src/lib.rs` | Add `mod analyze_symbol_source_tool;` and `pub use` |
| `codex-rs/core/src/tools/spec.rs` | Replace inline function with `use codex_tools::create_analyze_symbol_source_tool;` |

### Files NOT to Modify

| File | Reason |
|------|--------|
| `codex-rs/core/src/tools/handlers/analyze_symbol_source.rs` | Handler stays in codex-core (uses core-internal types) |
| `codex-rs/core/src/tools/handlers/mod.rs` | Handler registration stays |

## Upstream Compatibility

MEDIUM risk. We're adding a file to the upstream `codex-tools` crate. On the next upstream pull:
- Our new file won't conflict (it's new)
- `lib.rs` modification may conflict if upstream adds tools at the same location
- Mitigation: add our module at the end of the module list

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-tools               # new tool spec compiles
cargo check -p codex-core                 # registration still works
cargo test -p codex-tools                  # upstream tool tests pass
cargo test -p codex-core -- analyze_symbol # our tests pass
```
