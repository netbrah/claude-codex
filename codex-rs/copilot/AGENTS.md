# codex-copilot-adapter — AGENTS.md

Adapter crate that routes claude-codex's `WireApi::Copilot` through
`codex_copilot::CopilotHttpClient`. Orient here before touching this crate.

## What this crate is

A single-purpose bridge. It:

1. Builds an OpenAI chat-completions body from a claude-codex `Prompt`.
2. Calls `codex_copilot::CopilotHttpClient::chat_stream_with_auth` (which
   owns the 13 verbatim headers, TLS posture, 401 retry, and `slow_down`
   arithmetic).
3. Maps `codex_copilot::sse::ResponseEvent` into `codex_api::ResponseEvent`
   so the existing turn loop in `core/src/codex.rs` is unchanged.

Nothing else. If you are adding auth, headers, or retry logic here,
**stop** — that logic lives upstream in `codex-copilot`.

## Upstream source of truth

- Repo: this repo (`netbrah/xli`), branch `dev`. The Copilot integration push
  has **landed in-tree**; there is no separate upstream integration repo for
  this work.
- Pin: `Cargo.toml` holds the exact `codex-copilot` dependency revision.
  Bumping the pin is a one-line change + a green test run. Do not pin against
  a branch.
- Design: `../../docs/integration/02-design.md` (in-tree).
- Recon: `../../docs/integration/01-recon.md` (in-tree).
- 3-wire OPROD (operative ROE for the routing layer):
  `../../docs/integration/04-oprod-3wire-router.md`.
- Hardening recon (OP2 F1-F8): `../../docs/integration/06-op2-hardening-recon.md`.
- CLI / SDK research (Track 2): `../../docs/integration/07-research-copilot-cli.md`.

## Invariants inherited from `codex-copilot`

These are non-negotiable. See `netbrah/codex-agent/AGENTS.md` §"No-regression
invariants" for the authoritative list.

1. 83-test upstream baseline stays green. We do not edit `codex-copilot/src/`.
2. No `anyhow` in this adapter either (keeps the boundary clean).
3. TTY guard (`codex_copilot::is_interactive_tty()`) fires on the adapter's
   first stream call, not on `ModelClient::new`. Non-TTY callers must set
   `CODEX_COPILOT_ALLOW_HEADLESS=1`.
4. 13 Copilot headers verbatim — we **never** construct these ourselves.
5. `CopilotTokenCache` stays in the upstream crate with its redacted `Debug`.
6. No heavy deps. No `chrono`, no `secrecy`, no `native-tls`.
7. `rustls-tls` feature on `reqwest`.
8. `process_body_chunked` + `flush` stays upstream; we consume through it.
9. `slow_down` +5s arithmetic stays — cache the `CopilotHttpClient` across
   turns so the clock isn't reset every call.
10. `chat_stream_with_auth` owns 401 retry; **do not call `chat_stream_raw`
    from this adapter**. The upstream crate already has 401-retry on
    `with_auth`; the adapter would double-retry or miss it otherwise.

## How to test

```bash
cargo nextest run -p codex-copilot-adapter --no-fail-fast
```

No workspace-root runs from this crate. See `docs/integration/02-design.md`
§7 for the 14-test delta and what stays upstream.

## Commit posture

This adapter has **landed on `dev`** and is shipping. Active work happens on
sortie branches off `dev` (currently `sortie/summer-prose`). Each commit
should still be small and bisect-friendly — no mega-commits — and the branch
must stay green at every commit.
