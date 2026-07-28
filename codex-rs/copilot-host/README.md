# codex-copilot-host

Rust host for GitHub Copilot CLI style extensions — the parent side of the
`@github/copilot-sdk/extension` contract.

## What it does

Given a workspace and a session id, this crate:

1. **Discovers** extensions at `<workspace>/.github/extensions/*/extension.mjs`
   and optionally a user-global `~/.xli/extensions/` directory.
2. **Forks** each extension as a `node` child with `SESSION_ID`,
   `EXTENSION_PATH`, and (optionally) `COPILOT_SDK_PATH` in the environment,
   connected over piped stdio.
3. **Speaks vscode-jsonrpc framing** (`Content-Length: N\r\n\r\n<body>`) and
   JSON-RPC 2.0, the same wire Copilot CLI uses.
4. **Answers `session.resume`** (the child's handshake call), capturing every
   registered tool into a host-side registry.
5. **Dispatches `tool.call`** back into the owning child when the agent
   invokes a registered tool.
6. **Shuts down cleanly**: SIGTERM with a 5-second SIGKILL grace period,
   matching the Copilot CLI contract.

This is the reverse direction of FRAG-COPILOT-C (XLI as a Copilot guest).
Here XLI is the **host**; any extension authored against
`@github/copilot-sdk` can, in principle, run inside XLI unchanged.

## Status

Sortie: `thoughtful-raja`. Implements the MVP host loop validated by a real
Node end-to-end test (`tests/real_extension.rs`). Not yet wired into the
`codex-core` agent tool dispatch — integration with XLI's `Tool` enum is a
follow-up frag.

## Wire coverage

**Answered inbound** (child → host):

| Method            | Behavior |
|-------------------|----------|
| `session.resume`  | Parses params; stores tool + command registrations; returns `{workspacePath, capabilities}`. |
| `ping`            | Returns `{protocolVersion, message}`. |
| `tools.list`      | Returns the aggregated registry. |
| *(everything else)* | `-32601 Method not found`. |

**Outbound** (host → child):

| Method            | When |
|-------------------|------|
| `tool.call`       | Host calls `invoke_tool()` for a registered tool. |

Adding more methods is a routing-table change in `host.rs` — the framing and
correlator handle it transparently.

## Running the e2e

```bash
cargo test -p codex-copilot-host
```

The real-extension test spawns a Node fixture that frames vscode-jsonrpc,
performs `session.resume`, registers an `echo` tool, and answers `tool.call`.
It is skipped automatically when `node` is not on `PATH`.

## Legal posture

- **We do not vendor `@github/copilot-sdk`**. It is proprietary
  (`SEE LICENSE IN LICENSE.md`) and the license permits use, not
  redistribution. Our host simply implements the documented wire.
- Our test fixture (`tests/fixtures/echo-ext/extension.mjs`) is an
  independent reimplementation of the wire contract — no copied bytes.
- Extensions that `import "@github/copilot-sdk/extension"` are still usable:
  point `ExtensionHostConfig::sdk_path` at the SDK installed on the user's
  machine (e.g., `~/.npm-global/lib/node_modules/@github/copilot/copilot-sdk`)
  and set `bootstrap` to a loader that registers the resolver hook.
