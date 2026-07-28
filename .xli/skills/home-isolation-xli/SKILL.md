---
name: home-isolation-xli
description: Use when touching runtime state paths, the XLI launcher, or anything related to XLI_HOME vs CODEX_HOME bridging. Owns the S-040 home isolation rules from AGENTS.md — zero Rust engine changes, launcher-only bridging.
allowed-tools: Read, Edit, Grep, Bash(node --test deploy/npm/test/test-home-isolation.mjs)
---

# Home isolation — XLI (S-040)

## When to load
Any diff touching runtime state paths, `deploy/npm/bin/xli.js`, or
environment variable bridging for `XLI_HOME` / `CODEX_HOME`.

## The rule
XLI defaults its runtime state to `~/.xli` so it never collides with stock
Codex `~/.codex` installs. The launcher (`deploy/npm/bin/xli.js`) bridges
`CODEX_HOME` transparently — **zero Rust engine changes required**.

## Behavior matrix

| `XLI_HOME` | `CODEX_HOME` | Effective `CODEX_HOME` |
|------------|-------------|----------------------|
| unset | unset | `~/.xli` |
| `/custom/path` | unset | `/custom/path` |
| unset | `/explicit/codex` | `/explicit/codex` |
| `/custom/path` | `/explicit/codex` | `/explicit/codex` |

Key: explicit `CODEX_HOME` always wins. `XLI_HOME` is the default fallback.

## Branding surfaces (launcher-only)

| Surface | How | Suppression |
|---------|-----|-------------|
| ASCII banner (cyan) | Printed to stderr on interactive launch | `--quiet` or `-q` |
| `--version` / `-V` | Intercepted by launcher | — |
| Terminal title | `CODEX_APP_NAME` env (future) | — |

Banner only appears when both stdin and stdout are TTYs and no prompt arg
is provided.

## Workflow
1. Read `deploy/npm/bin/xli.js` before changing env bridging logic.
2. Make the change in the launcher only — not in Rust engine code.
3. Run: `node --test deploy/npm/test/test-home-isolation.mjs`.
4. Manual sanity check with the behavior matrix above.

## Anti-patterns
- Modifying Rust code to handle `XLI_HOME` (it's launcher-only).
- Breaking the "explicit CODEX_HOME wins" precedence.
- Showing the banner in non-interactive (piped) contexts.
