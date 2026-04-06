> **STATUS: ✅ COMPLETE** — This sortie has been implemented and merged. Kept for reference.

# S-040 — XLI Rebrand / Home Isolation (~/.xli)

**Priority:** 🟠 P1
**Complexity:** Small (2-4 hours)
**Scope:** Proprietary branch ONLY (`feat/xli-embed-assets`)
**Upstream Risk:** ZERO — proprietary branch, no engine changes

## Objective

Make the proprietary XLI distribution default its runtime state to `~/.xli` instead of `~/.codex`, so daily use never collides with stock Codex installs.

## Branch Plan

```bash
git checkout feat/xli-embed-assets
git checkout -b xli/sortie/S-040-home-isolation
```

## Files to Modify (Proprietary Branch Only)

| File | Change |
|------|--------|
| `deploy/npm/bin/xli.js` | Set `XLI_HOME` default to `~/.xli`, bridge `CODEX_HOME` to `XLI_HOME` when not explicitly set |
| `deploy/npm/package.json` | Update path references if any mention `~/.codex` |
| `AGENTS.md` (proprietary branch) | Document `XLI_HOME` env var |
| `CLAUDE.md` (proprietary branch) | Update path references |

## Files NOT to Touch

- **NOTHING under `codex-rs/`** — the Rust engine. Zero changes to the public engine.
- **`dev` branch files** — this is proprietary-only

## Implementation

In `deploy/npm/bin/xli.js`:
```javascript
// Default XLI_HOME to ~/.xli
const XLI_HOME = process.env.XLI_HOME || path.join(os.homedir(), '.xli');

// Bridge: if CODEX_HOME is not explicitly set, point it at XLI_HOME
if (!process.env.CODEX_HOME) {
    process.env.CODEX_HOME = XLI_HOME;
}

// Preserve explicit CODEX_HOME (user override)
// process.env.XLI_HOME is always set for reference
process.env.XLI_HOME = XLI_HOME;
```

## Acceptance Criteria

1. `XLI_HOME` unset + `CODEX_HOME` unset → runtime uses `~/.xli`
2. `XLI_HOME` set + `CODEX_HOME` unset → runtime uses `$XLI_HOME`
3. `CODEX_HOME` explicitly set → runtime preserves explicit `CODEX_HOME`
4. Stock `codex` CLI remains unaffected (uses `~/.codex`)

## Test

```bash
# After changes:
node deploy/npm/bin/xli.js --help        # should reference ~/.xli paths
XLI_HOME=/tmp/test-xli node deploy/npm/bin/xli.js --help  # verify override
```

## Upstream Compatibility

✅ PERFECT. This is proprietary branch only. Zero public engine changes. Stock Codex behavior completely unaffected.
