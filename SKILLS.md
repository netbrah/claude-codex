# XLI Agent Skills — Index

Human-readable reference. Native skills live under `.xli/skills/<slug>/SKILL.md`
and are progressively disclosed (load only when triggered).

> Rule: Load **max 3–4 skills** per task. Prefer retrieval over recall —
> when a skill applies, read its `SKILL.md`, don't paraphrase from memory.

## Load-by-task (quick reference)

| Task | Load |
|------|------|
| TUI widget / layout change | `tui-ratatui-rendering`, `tui-snapshot-testing` |
| Chat composer / paste / IME | `tui-state-machines`, `tui-event-loop` |
| Streaming token render bug | `tui-streaming-render`, `tui-event-loop`, `wire-responses-api` (or `wire-messages-anthropic`) |
| New wire protocol (Gemini) | `wire-generate-content-gemini`, `response-item-lingua-franca`, `wire-messages-anthropic` (as template) |
| Copilot adapter | `copilot-adapter-discipline`, `response-item-lingua-franca` |
| Sandbox / exec change | `sandbox-and-execpolicy`, `test-gate-xli` |
| Release / packaging | `release-and-deploy`, `home-isolation-xli` |
| Linux binary build / dogfood | `linux-build-scs`, `release-and-deploy` |
| Upstream sync from openai/codex | `absorbing-upstream-merges`, `sortie-branch-discipline`, `rust-workspace-hygiene` |
| Any PR | `sortie-branch-discipline`, `test-gate-xli` |

## APEX integration (per-operator, regenerable)

The APEX agent reads skills from `<repo>/.apex/skills/`. The source of
truth is `.xli/skills/<slug>/SKILL.md` (tracked in git); `.apex/skills/`
is a symlink farm pointing back at those (gitignored because it's
per-operator and regenerable on any fresh clone). To rebuild after a
`git clone`:

```bash
mkdir -p .apex/skills
cd .apex/skills
for d in ../../.xli/skills/*/; do
  ln -sfn "../../.xli/skills/$(basename "$d")" "$(basename "$d")"
done
```

This keeps XLI-specific skills *project-local*. Never symlink them into
`~/.apex/skills/` — that pollutes the global skill namespace across
every workspace.

## Catalog

### TUI (Tier 1)
- `.xli/skills/tui-ratatui-rendering/SKILL.md`
- `.xli/skills/tui-state-machines/SKILL.md`
- `.xli/skills/tui-event-loop/SKILL.md`
- `.xli/skills/tui-streaming-render/SKILL.md`
- `.xli/skills/tui-snapshot-testing/SKILL.md`
- `.xli/skills/tui-accessibility-and-terminfo/SKILL.md`

### Wire / Harness (Tier 2)
- `.xli/skills/wire-responses-api/SKILL.md`
- `.xli/skills/wire-messages-anthropic/SKILL.md`
- `.xli/skills/wire-generate-content-gemini/SKILL.md`
- `.xli/skills/response-item-lingua-franca/SKILL.md`
- `.xli/skills/copilot-adapter-discipline/SKILL.md`
- `.xli/skills/mcp-server-and-tools/SKILL.md`

### Safety / Build / Process (Tier 3)
- `.xli/skills/sandbox-and-execpolicy/SKILL.md`
- `.xli/skills/rust-workspace-hygiene/SKILL.md`
- `.xli/skills/bazel-cargo-dual-build/SKILL.md`
- `.xli/skills/sortie-branch-discipline/SKILL.md`
- `.xli/skills/test-gate-xli/SKILL.md`
- `.xli/skills/home-isolation-xli/SKILL.md`
- `.xli/skills/release-and-deploy/SKILL.md`
