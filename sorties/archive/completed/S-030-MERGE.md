> **STATUS: ✅ COMPLETE** — This sortie has been implemented and merged. Kept for reference.

# S-030-MERGE — Merge S-030 feat branch into dev

**Priority:** 🟡 P2
**Complexity:** Medium
**Dependency:** None
**Blocks:** S-031 (codex-lsp-server)

## What S-030 Brings

From `origin/feat/moa-depth2-bfs-call-graph`:
- Manifest-backed search infrastructure (`workspace_index`, `manifest_builder`, `dir_stats`, `search_rg`)
- `analyze_symbol_source` tool and tests
- `clang_graph/` stack (BFS + compile db + edge extraction)
- Tool schema additions in `spec.rs`

## Merge Strategy

Use an isolated worktree:
```bash
git -C ~/Projects/xli fetch origin --prune
git -C ~/Projects/xli worktree add /tmp/xli-s030 origin/dev
cd /tmp/xli-s030
git checkout -b xli/sortie/S-030-merge origin/dev
git merge --no-ff origin/feat/moa-depth2-bfs-call-graph \
  -m "merge: S-030 grep/search manifest + analyze_symbol_source + clang_graph"
```

## Conflict Zones

| File | Risk | Resolution |
|------|------|-----------|
| `codex-rs/core/Cargo.toml` | HIGH | Keep both dependency additions |
| `codex-rs/core/src/tools/handlers/mod.rs` | HIGH | Add our modules |
| `codex-rs/core/src/tools/spec.rs` | HIGH | Merge tool registrations |
| `codex-rs/Cargo.lock` | MEDIUM | Regenerate after Cargo.toml resolved |

## Validation Gate

```bash
cd codex-rs
cargo test -p codex-core
cargo build -p codex-core --features clang-graph
cargo clippy -p codex-core
cargo test -p codex-core analyze_symbol_source
```

## Upstream Compatibility

MEDIUM risk. `spec.rs` is a high-churn upstream file. After this merge, S-042 should move our tool spec to `codex-tools` crate to reduce future conflict surface.

## Full playbook at: `cli-ops/sortie-board/S-030-MERGE-PLAYBOOK.md`
