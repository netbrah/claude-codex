# AGENTS.md — NetApp XLI (Proprietary Layer)

> This file exists ONLY on `feat/xli-embed-assets`. It MUST NOT appear on `dev`.
> `dev` = public engine. This branch = thin NetApp veneer (~5 commits on top of dev).

---

## Architecture

```
feat/xli-embed-assets  <- YOU ARE HERE (NetApp XLI distribution)
        ^ rebase
      dev               <- public engine (netbrah/codex, open-source)
        ^ absorbs
  upstream/main (openai/codex)
```

- **Lingua franca**: `ResponseItem[]` enum (Codex internal type)
- **Wire dispatch**: `WireApi::Responses` (native) or `WireApi::Messages` (our addition)
- **Hub**: `~/Projects/cli-ops/` — C2 coordination, sortie board, feature registry

### Wire Protocol Status

| Wire | Status | Implementation |
|------|--------|----------------|
| /responses | Native | openai/codex default |
| /messages | Shipped (CA-2..CA-8) | stream_messages_api() in client.rs |
| generateContent | Not started | — |

---

## What's Proprietary (This Branch Only)

| Path | Content |
|------|---------|
| `deploy/build.sh` | Release build script |
| `deploy/npm/` | npm package scaffolding for XLI distribution |
| `deploy/skills/ontap-dev-guide/` | ONTAP-specific skill |
| `AGENTS.md` (this file) | Proprietary context + proxy config |
| `CLAUDE.md` | Corp-specific agent instructions with proxy URLs |
| `deploy/npm/test/` | Home isolation env-bridging tests (S-040) |

---

## Home Isolation (S-040)

XLI defaults its runtime state to `~/.xli` so it never collides with
stock Codex `~/.codex` installs.  The launcher (`deploy/npm/bin/xli.js`)
bridges `CODEX_HOME` transparently — zero Rust engine changes required.

| Env Var | Default | Effect |
|---------|---------|--------|
| `XLI_HOME` | `~/.xli` | Where XLI stores history, logs, config |
| `CODEX_HOME` | (bridged to `XLI_HOME`) | If unset, auto-set to `XLI_HOME`; if explicitly set, preserved |

### Behavior Matrix

| `XLI_HOME` | `CODEX_HOME` | Effective `CODEX_HOME` |
|------------|-------------|----------------------|
| unset | unset | `~/.xli` |
| `/custom/path` | unset | `/custom/path` |
| unset | `/explicit/codex` | `/explicit/codex` |
| `/custom/path` | `/explicit/codex` | `/explicit/codex` |

### Quick Test

```bash
# Default — state goes to ~/.xli
node deploy/npm/bin/xli.js --help

# Override XLI_HOME
XLI_HOME=/tmp/test-xli node deploy/npm/bin/xli.js --help

# Explicit CODEX_HOME is preserved
CODEX_HOME=/tmp/my-codex node deploy/npm/bin/xli.js --help

# Run env-bridging unit tests
node --test deploy/npm/test/test-home-isolation.mjs
```

### Branding Surfaces

The XLI launcher (`deploy/npm/bin/xli.js`) provides these branding
surfaces without any Rust engine changes:

| Surface | How | Suppression |
|---------|-----|-------------|
| ASCII banner (cyan) | Printed to stderr on interactive launch | `--quiet` or `-q` |
| `--version` / `-V` | Intercepted by launcher, prints `xli <version>` | — |
| Terminal title | Driven by `CODEX_APP_NAME` env (future) | — |
| Welcome screen text | Would require engine change | N/A (deferred) |

The banner only appears when both stdin and stdout are TTYs and no
prompt argument is provided (i.e., the operator typed bare `xli`).


---

## Proxy Configuration

```bash
# Set in ~/.bashrc — already configured on corp machines
export CODEX_LLM_PROXY_KEY="<from NetApp vault>"
export CODEX_PROXY_BASE_URL="https://llm-proxy-api.ai.eng.netapp.com"

# Also works for direct Anthropic auth via proxy:
export ANTHROPIC_API_KEY="$CODEX_LLM_PROXY_KEY"
export ANTHROPIC_BASE_URL="https://llm-proxy-api.ai.eng.netapp.com"
```

---

## Sortie Instructions — ALL AGENTS READ THIS

Read `CLAUDE.md` for the full test mandate, test patterns, e2e instructions, and sortie completion gate. This file has the architecture context; CLAUDE.md has the operational runbook.

### Rules
1. Work ONLY on your assigned sortie branch (`apex/sortie/...`)
2. Do NOT merge to dev or feat/xli-embed-assets — that is C2's job
3. Do NOT revert commits from other agents
4. Every commit must have a conventional prefix: `feat:`, `fix:`, `test:`, `refactor:`

### Quick Test Reference

```bash
cd codex-rs

# Unit tests (every change)
cargo test -p codex-core -p codex-api

# Live /messages e2e (wire-layer sorties)
cargo test -p codex-core --test all -- live_messages --ignored --test-threads=1

# Proxy e2e (full stack — LiteLLM -> Vertex AI -> Claude)
CODEX_PROXY_E2E=1 CODEX_PROXY_BASE_URL="https://llm-proxy-api.ai.eng.netapp.com" \
  cargo test -p codex-exec --test proxy_e2e_messages -- --test-threads=1
```

---

## Hub Reference

| What you need | Where to find it |
|---------------|-----------------|
| Big picture | `~/Projects/cli-ops/AGENTS.md` |
| Feature registry (CA-1..CA-9) | `~/Projects/cli-ops/AGENTS.md` Feature Registry |
| Next sorties | `~/Projects/cli-ops/sortie-board/SORTIE-BOARD.md` |
| Active branch status | `~/Projects/cli-ops/sortie-board/active-sorties.md` |
| XLI null-space gaps | `~/Projects/cli-ops/docs/null-space-ops/reference/` |
| Features to port to Apex | `~/Projects/cli-ops/docs/feature-delta/03-cross-implementation.md` |
| XLI feature delta | `~/Projects/cli-ops/docs/feature-delta/02-codex-harness-additions.md` |
| How ops work | `~/Projects/cli-ops/docs/03-OPERATIONAL-MODEL.md` |

---

## Rebase Procedure

```bash
git checkout feat/xli-embed-assets
git fetch origin
git rebase dev
# deploy/ files never conflict with codex-rs/ changes
git push origin feat/xli-embed-assets --force-with-lease
```

---

## Build + Deploy Pipeline

```
push to dev
    ↓
GitHub Actions builds both binaries (~15 min, on their runners)
    ↓
you get notified, then locally:

    gh run download <run-id> -D /tmp/xli-artifacts
    cp /tmp/xli-artifacts/xli-darwin-arm64/xli deploy/npm/vendor/aarch64-apple-darwin/xli/xli
    cp /tmp/xli-artifacts/xli-linux-amd64/xli deploy/npm/vendor/x86_64-unknown-linux-gnu/xli
    deploy/upload.sh ship
```

End users install with:
```bash
curl -fsSL https://generic.repo.eng.netapp.com/user/palanisd/xli/release/install.sh | bash
```
