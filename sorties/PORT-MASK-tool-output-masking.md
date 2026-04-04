# PORT-MASK — Tool Output Masking Service

**Priority:** 🟡 P2 — Cross-Pollination
**Complexity:** Medium (3-4 hours)
**Source:** Apex `toolOutputMaskingService.ts`
**Target:** New module `codex-rs/core/src/tool_output_masking.rs`
**Upstream Risk:** ZERO — new file + small integration point

## Problem

XLI sends full tool outputs in every subsequent API request. With Opus 1M context, this is tolerable. But for Sonnet sub-agents at 200K, large tool outputs (>50K tokens — e.g., full file reads, large grep results) quickly exhaust the context window.

Apex masks large tool outputs by writing them to disk and replacing with a reference: `<tool_output_masked ref="hash">First 200 chars...{N} chars masked...Last 200 chars</tool_output_masked>`.

## Implementation

```rust
pub struct ToolOutputMasker {
    mask_dir: PathBuf,       // e.g., ~/.xli/masked_outputs/
    threshold_chars: usize,  // default: 50_000 (roughly 12.5K tokens)
    exempt_tools: HashSet<String>,  // tools that should never be masked
}

impl ToolOutputMasker {
    pub fn maybe_mask(&self, tool_name: &str, output: &str) -> MaskResult {
        if self.exempt_tools.contains(tool_name) {
            return MaskResult::Unmasked(output.to_string());
        }
        if output.len() < self.threshold_chars {
            return MaskResult::Unmasked(output.to_string());
        }
        
        let hash = sha256_hex(output);
        let mask_path = self.mask_dir.join(&hash);
        std::fs::write(&mask_path, output)?;
        
        let head = &output[..200.min(output.len())];
        let tail = &output[output.len().saturating_sub(200)..];
        let masked = format!(
            "<tool_output_masked ref=\"{}\">{}\n...{} chars masked...\n{}</tool_output_masked>",
            hash, head, output.len() - 400, tail
        );
        MaskResult::Masked { replacement: masked, original_path: mask_path }
    }
}
```

### Exempt Tools

Never mask output from:
- `ask_user_question` (user interaction)
- `memory` (server-side tool)
- Skill tools (ONTAP domain tools)

### Protection Rules

- Never mask tool results from the current turn
- Never mask tool results from the last 3 turns
- Only mask when estimated context exceeds 60% of max_input_tokens

## Upstream Compatibility

✅ New file. Integration point is in the Messages wire path only.

## Tests

| # | Test | Expected |
|---|------|----------|
| 1 | Short output (< threshold) → unmasked | Pass-through |
| 2 | Long output (> threshold) → masked with head+tail | Replacement generated |
| 3 | Exempt tool → never masked | Pass-through regardless of size |
| 4 | Mask file written to disk | File exists and contains full output |

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-core
cargo test -p codex-core -- mask
```
