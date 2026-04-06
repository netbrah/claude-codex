# S-CACHE-TTL — cache_control.ttl Extended Lifetimes

**Priority:** 🟡 P2
**Complexity:** Small (30 min)
**Files:** `codex-rs/core/src/messages_wire.rs`, `codex-rs/core/src/client.rs`
**Upstream Risk:** ZERO — changes in new files + config

## Problem

Both XLI and Apex hardcode `{type: "ephemeral"}` for cache_control without `ttl`. The default TTL is 5 minutes. The Anthropic SDK now supports `ttl: "1h"` for longer-lived caches.

For long Opus sessions with stable system prompts (AGENTS.md + skills + 30 tool definitions), extending the cache from 5 minutes to 1 hour reduces repeated prompt token costs by up to 90%.

## Implementation

```rust
// Update CacheControl struct:
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    pub r#type: String,                         // "ephemeral"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,                    // "1h" | "5m" | None
}

// Config:
// ~/.codex/config.toml:
// [model]
// system_cache_ttl = "1h"
```

## Upstream Compatibility

✅ Changes in new files. Config is additive.

## Tests

1. Test: `ttl: "1h"` set → included in cache_control JSON
2. Test: `ttl` not set → omitted (backward compatible)
