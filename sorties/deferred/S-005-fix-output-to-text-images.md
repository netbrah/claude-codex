# S-005-fix — output_to_text() Image Content Handling

**Priority:** 🔴 Ship-Blocking
**Complexity:** Small (1-2 hours)
**Files:** `codex-rs/core/src/messages_wire.rs:341-358`
**Upstream Risk:** ZERO — this is a new file

## Problem

`output_to_text()` silently drops `InputImage` items from `FunctionCallOutputBody::ContentItems`. When a tool result contains images (e.g., screenshots, diagrams), the image content is lost and only text portions are included in the Anthropic API request.

## Root Cause

Code review finding #8 (MEDIUM). The `output_to_text()` function only handles `InputText` variants:

```rust
fn output_to_text(body: &FunctionCallOutputBody) -> String {
    match body {
        FunctionCallOutputBody::Text(t) => t.clone(),
        FunctionCallOutputBody::ContentItems(items) => {
            items.iter().filter_map(|item| {
                match item {
                    ContentItem::InputText { text } => Some(text.clone()),
                    ContentItem::OutputText { text } => Some(text.clone()),
                    _ => None,  // ← InputImage silently dropped
                }
            }).collect::<Vec<_>>().join("\n")
        }
    }
}
```

## Fix

Handle `InputImage` by converting to an Anthropic `image` content block OR generating a text placeholder:

```rust
ContentItem::InputImage { image_url, .. } => {
    // Option A: Convert to text placeholder for text-only contexts
    Some(format!("[Image: {}]", image_url.as_deref().unwrap_or("embedded")))
}
```

For full image support, the function should return `Vec<serde_json::Value>` instead of `String`, allowing mixed text + image content blocks in tool results. But the text placeholder is the minimum fix.

## Upstream Compatibility

✅ This file (`messages_wire.rs`) is entirely new — no upstream conflict possible.

## Tests

1. Test: tool result with text only → text preserved
2. Test: tool result with image only → image placeholder generated
3. Test: tool result with mixed text + image → both represented
4. Test: tool result with empty content items → empty string

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-core
cargo test -p codex-core -- messages_wire
cargo test -p codex-core -- output_to_text
```
