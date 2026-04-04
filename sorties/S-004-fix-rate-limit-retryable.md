# S-004-fix — Rate Limit 429 → Retryable

**Priority:** 🔴 Ship-Blocking
**Complexity:** Small (1-2 hours)
**Files:** `codex-rs/core/src/api_bridge.rs:122-125`
**Upstream Risk:** LOW — small change in a less-churned file

## Problem

ALL HTTP 429 responses are mapped to non-retryable `CodexErr::RetryLimit`, which kills the session immediately. This conflates transient rate limit spikes (retry after 1-5s) with permanent plan limit exhaustion.

Brief rate limit spikes from Vertex/proxy are common under load. Currently XLI crashes the session instead of backing off and retrying.

## Root Cause

Code review finding #9 (MEDIUM):

```rust
// api_bridge.rs:122-125
ApiError::RateLimit(msg) => {
    CodexErr::RetryLimit(msg)  // ← ALL 429s become permanent failures
}
```

## Fix

Distinguish transient 429s from permanent limit errors. Use exponential backoff for transient rate limits:

```rust
ApiError::RateLimit(msg) => {
    // Check if this is a permanent limit (e.g., "plan limit exceeded")
    // vs transient (e.g., "rate limited, retry after N seconds")
    if msg.contains("plan") || msg.contains("quota") || msg.contains("budget") {
        CodexErr::RetryLimit(msg)
    } else {
        // Transient rate limit — surface as retryable
        CodexErr::Retryable(msg)
    }
}
```

Or better: check the `Retry-After` header if available in the response, and use that for backoff timing.

## Upstream Compatibility

LOW risk. `api_bridge.rs` is not in the highest-churn zone. The fix is a localized change to error mapping logic that doesn't affect upstream code paths.

## Tests

1. Test: 429 with "rate limited" message → retryable (not session-ending)
2. Test: 429 with "plan limit exceeded" → non-retryable (session ends)
3. Test: 429 with Retry-After header → backoff uses header value

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-core
cargo test -p codex-core -- rate_limit
cargo test -p codex-core -- api_bridge
```
