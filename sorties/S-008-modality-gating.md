> **STATUS: ✅ COMPLETE** — This sortie has been implemented and merged. Kept for reference.

# S-008 — Add Modality Gating to /messages Wire

**Priority:** 🟠 P1 — Cross-Pollination
**Complexity:** Medium (2-3 hours)
**Source:** `cli-ops/spokes/qwen-code/packages/core/src/core/openaiContentGenerator/converter.ts` (modality gating)
**Target:** `codex-rs/core/src/messages_wire.rs`
**Upstream Risk:** ZERO — target is a new file

## Problem

When translating ResponseItem[] → Anthropic messages, XLI sends all media types (images, etc.) directly to the API without checking if the target model supports them. Text-only models reject image content blocks, causing API errors.

Apex has `modalityDefaults.ts` which checks model capabilities and substitutes text placeholders for unsupported media.

## Implementation

In `conversation_to_anthropic_messages()`, when processing `ContentItem::InputImage`:

```rust
// Check if the model supports image input
fn supports_image_input(model_slug: &str) -> bool {
    // Claude models support images (haiku, sonnet, opus)
    // Text-only proxy models may not
    model_slug.starts_with("claude")
        || model_slug.contains("vision")
        || model_slug.contains("multimodal")
}

// In the content translation:
ContentItem::InputImage { image_url, .. } => {
    if supports_image_input(model_slug) {
        // Pass through as Anthropic image content block
        json!({
            "type": "image",
            "source": { "type": "url", "url": image_url }
        })
    } else {
        // Replace with text placeholder
        json!({
            "type": "text",
            "text": format!("[Image: {}]", image_url.as_deref().unwrap_or("embedded image"))
        })
    }
}
```

### Integration Point

The model slug is available in `stream_messages_api()` at `client.rs` — thread it through to the translation function. Alternatively, use the `ModelProviderInfo.input_modalities` field if available (check `model_provider_info.rs`).

## Reference

- `codex-rs/core/src/model_provider_info.rs` — `ModelProviderInfo` struct, check for `input_modalities` field
- `codex-rs/core/src/tools/spec.rs` — `fn has_image_input()` at ~line 355 shows the modality check pattern
- `codex-rs/core/models.json` — Model definitions with capability fields

## Upstream Compatibility

✅ `messages_wire.rs` is a new file. Adding a parameter to the translation function only affects the Messages wire path.

## Tests

| # | Test | Expected |
|---|------|----------|
| 1 | InputImage with "claude-sonnet-4.6" → image block | Image passed through |
| 2 | InputImage with "gpt-4" → text placeholder | Placeholder generated |
| 3 | Mixed text + image with text-only model → image replaced, text preserved | Selective replacement |
| 4 | No images in content → no changes | Pass-through |

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-core
cargo test -p codex-core messages_wire
```
