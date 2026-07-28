# SORTIE — FRAG-COPILOT-D (thoughtful-raja worktree)

**Frag**: FRAG-COPILOT-D — Copilot extension host (reverse).
**Origin**: `summer-prose/docs/integration/07-research-copilot-cli.md#9` — "any Copilot-targeted
extension works inside XLI unchanged."

## What shipped

A new workspace crate, `codex-rs/copilot-host/`, implementing the MVP parent
side of the `@github/copilot-sdk/extension` wire:

- `src/framing.rs` — vscode-jsonrpc stdio framing (Content-Length headers).
- `src/protocol.rs` — JSON-RPC 2.0 envelope + typed `session.resume` params.
- `src/discovery.rs` — scans `.github/extensions/` and user dir.
- `src/extension.rs` — fork + reader/writer/correlator + SIGTERM→SIGKILL
  lifecycle.
- `src/host.rs` — discovery → spawn → tool registry → `tool.call` dispatch.
- `tests/fixtures/echo-ext/extension.mjs` — independent reimplementation of
  the wire; no proprietary bytes copied.
- `tests/real_extension.rs` — forks real Node, does `session.resume` +
  `tool.call` round-trip.

## Coverage

- 12 unit tests (framing, protocol, discovery).
- 2 integration tests — including a real Node end-to-end round-trip.
- `cargo clippy -p codex-copilot-host --tests -- -D warnings` clean.
- `cargo check --workspace` clean.

## What is deliberately deferred

- Wiring into `codex-core`'s tool dispatch. That is a separate frag — this
  crate is self-contained and exposes `ExtensionHost::tools()` +
  `ExtensionHost::invoke_tool()` for consumers to bridge.
- Implementing the remaining 60+ RPC methods. The router returns
  `-32601 Method not found` for everything outside MVP. Adding any method
  is a pure routing-table entry in `host.rs`.
- Out-of-process / TCP transport. Only stdio fork is implemented (matches
  the Copilot CLI default for extensions).
- `hooks.invoke`, `permission.request`, `session.event` notifications —
  scaffolded in `protocol.rs` docs but not routed yet.

## Legal

`@github/copilot-sdk` is proprietary (license permits use, not redistribution).
We drive it; we do not vendor it. Our test fixture is an independent
reimplementation of the documented wire.
