# S-016 — Wire stop_sequences from Config

**Priority:** 🟡 P2
**Complexity:** Small (30 min - 1 hour)
**Files:** `codex-rs/core/src/config/types.rs`, `codex-rs/core/src/client.rs:1270`
**Upstream Risk:** LOW — additive field to config struct

## Problem

`stop_sequences` field exists in `MessagesApiRequest` (at `messages.rs:52`) but is hardcoded to `None` at `client.rs:1270`. The `SamplingParams` struct has no `stop_sequences` field, so there's no way for users to configure it.

Two-step fix needed:
1. Add `stop_sequences: Option<Vec<String>>` to `SamplingParams`
2. Wire at callsite from `sampling.stop_sequences`

## Implementation

### Step 1: Add to config
```rust
// In config/types.rs, SamplingParams struct:
pub struct SamplingParams {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u32>,
    pub stop_sequences: Option<Vec<String>>,  // NEW
}
```

### Step 2: Wire at callsite
```rust
// In client.rs, stream_messages_api():
stop_sequences: sampling.stop_sequences.clone(),  // was: None
```

### Config usage
```toml
# In ~/.codex/config.toml:
[sampling]
stop_sequences = ["</answer>", "## Next Step", "STOP"]
```

## Evidence Ledger

Live tested: `stop_sequences: ["STOP"]` → `stop_reason: stop_sequence, stop_sequence: "STOP"` ✅ (2026-03-25)

## Upstream Compatibility

LOW risk. Adding an `Option` field to `SamplingParams` is backward-compatible. Serialization uses `skip_serializing_if = "Option::is_none"`.

## Tests

1. Test: stop_sequences configured → sent in request
2. Test: stop_sequences not configured → `None` (current behavior)
3. Test: stop_sequence response captured when stop_sequences set

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-core
cargo test -p codex-core -- stop_sequence
```
