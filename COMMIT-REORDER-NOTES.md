# Commit History Rearrangement

This branch contains a rearranged commit history where:
- **Bottom (oldest)**: 5,242 clean upstream openai/codex commits
- **Top (newest)**: 8 logical custom commit groups

## Custom Commit Groups (top → bottom)

1. **chore**: CI workflow updates, sandbox fixes, snapshot updates, misc
2. **docs**: Project context — AGENTS.md, architecture docs, sortie board
3. **feat**: Deploy infrastructure — build scripts, npm package, CI workflows
4. **feat**: Skill harness injection, build integration, core modules
5. **feat**: Custom tools — find_files, loop detection, masking, clang graph
6. **feat**: Rebrand codex → xli across CLI, TUI, config, state, tests
7. **feat**: Model registry, config schema, dependencies
8. **feat**: Anthropic /messages wire protocol — endpoint, SSE, translator

## Verification

- `git diff origin/dev` = empty (tree-identical to current dev)
- No merge commits in custom section (fully linear)
- Upstream tip: `26a28afc6` (Extract realtime input task handlers #17280)
