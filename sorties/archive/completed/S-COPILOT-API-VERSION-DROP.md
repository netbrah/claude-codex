# S-COPILOT-API-VERSION-DROP — Stop sending `X-GitHub-Api-Version` on Copilot LLM-plane paths

**Track:** 1 (HTTP wire, native + fallback) · **Status:** ACTIVE — recon-then-fix · **Filed:** 2026-04-19 · **Severity:** Potentially critical (silent wire-shape divergence)

## Symptom

Every Copilot wire request stamps `X-GitHub-Api-Version: 2025-04-01` on
`api.githubcopilot.com` paths (`/v1/messages`, `/v1/chat/completions`,
`/responses`). That header is part of **github.com REST API** versioning
(used on `api.github.com`), not the Copilot LLM plane. At best it's
ignored; at worst it influences routing, WAF rules, or quirks-mode in
ways we don't observe.

C2 observation: real `vscode-copilot-chat` only stamps this on
`api.github.com` paths, not on Copilot LLM endpoints. The
`docs/integration/03-validation.md §3.7` claim of parity with
`openAIEndpoint.ts` is suspect.

## Trace

| Layer | File:line | Behavior |
|---|---|---|
| Upstream constant | `codex-copilot/src/auth.rs:25` (rev `26ff8f1d`) | `pub const GITHUB_API_VERSION: &str = "2025-04-01";` |
| Upstream stamp | `codex-copilot/src/client.rs:139` | Always inserts `("x-github-api-version", GITHUB_API_VERSION)` in `CopilotHeaders::into_header_map()` |
| Our adapter | `codex-rs/copilot/src/wire.rs:380-385` (`build_headers()`) | Always starts from `CopilotHeaders::new(bearer).into_header_map()` — inherits the header verbatim |
| All three sub-wires | `endpoints.rs::route_for_model` → `wire.rs::POST` | Header rides on `/v1/messages`, `/v1/chat/completions`, `/responses` requests |

Correctly-scoped use of the header (do NOT touch):
- `codex-rs/core/src/plugins/startup_sync.rs:918` — hits `api.github.com`
  for plugin/release fetch. That's the intended scope.

## Recon checklist (before code change)

1. Capture real `vscode-copilot-chat` traffic — confirm whether `/v1/messages` and
   `/v1/chat/completions` requests include `X-GitHub-Api-Version`. Two viable methods:
   - mitmproxy on the vscode extension
   - Read `microsoft/vscode-copilot-chat` source at the rev cited in
     `03-validation.md:195` (`9e668cb12144...`/`openAIEndpoint.ts`) and
     verify which header set is sent on which URL.
2. Hit `api.githubcopilot.com/v1/messages` once with the header and once
   without (controlled live test); record any difference in:
   - response status / latency
   - `cf-ray` / `via` / Server headers (WAF or routing fingerprint)
   - usage attribution in `account.getQuota`
3. If vscode does NOT send it on `/v1/*`, the fix is unambiguous: strip.
4. If vscode DOES send it on `/v1/*`, escalate — the header may be
   load-bearing for some Copilot-side counter we can't observe.

## Fix shape (assuming recon confirms it should be stripped)

The header is added inside the upstream frozen crate, so we cannot stop it
at the source — we must strip it in our adapter. Two equivalent locations:

**Option A — strip in `build_headers()` after `into_header_map()` returns:**

```rust
// codex-rs/copilot/src/wire.rs::build_headers()
let mut map = CopilotHeaders::new(bearer)
    .with_initiator_agent(true)
    .into_header_map()?;
map.remove("x-github-api-version");  // not for the LLM plane
// ... existing intent override + interaction id insertion
```

**Option B — strip per-route in the POST path** if we ever discover an
`api.github.com` Copilot path that genuinely needs it (unlikely — `startup_sync.rs`
already handles its own headers).

Option A is simpler and matches Maverick's framing ("drop entirely from /v1/*").

## Test mandate

- **Unit (regression guard):** assert `build_headers()` output does NOT
  contain `x-github-api-version`. Add to
  `codex-rs/copilot/tests/wire_router.rs` or `wire.rs::tests::build_headers_*`.
- **Unit (positive):** assert `startup_sync.rs::github_request()` DOES
  send the header (existing behavior preserved).
- **Live e2e (gated):** extend `core/tests/suite/live_copilot.rs` to assert
  no observable behavior change after the strip — same response status,
  same usage delta on `account.getQuota`.

## Files touched

- `codex-rs/copilot/src/wire.rs` — `build_headers()` strips the header.
- `codex-rs/copilot/tests/wire_router.rs` — regression guard.
- `docs/integration/03-validation.md` §3.7 — rewrite from "documented drift"
  to "removed; LLM plane does not consume this header".

## Out of scope

- The `2025-04-01` vs `2025-05-01` value drift discussion in §3.7 — moot
  once the header is stripped from the LLM plane.
- The `Copilot-Integration-Id: vscode-chat` hardcode (§3.8) — separate item.
- `startup_sync.rs` plumbing — that one is correctly scoped to
  `api.github.com` and stays untouched.

## Promotion

Promote from ACTIVE-RECON to ACTIVE-FIX once recon step 3 confirms vscode
does not send the header on `/v1/*`. C2 holds the trigger.
