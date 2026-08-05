# Skills

Skills are reusable instruction bundles that help the agent perform recurring
tasks more consistently.

A skill is usually a directory containing a required `SKILL.md` file plus
optional supporting resources.

## What a skill can contain

```text
skill-name/
├── SKILL.md
├── agents/
│   └── openai.yaml
├── scripts/
├── references/
└── assets/
```

## Core concepts

- **`SKILL.md`** is the entrypoint and contains the skill description plus the
  operative instructions.
- **`agents/openai.yaml`** is optional UI metadata for surfacing the skill.
- **`scripts/`** holds deterministic helpers.
- **`references/`** holds detailed docs that can be loaded only when needed.
- **`assets/`** holds non-context files used by the workflow.

## Where skills live

The runtime resolves skills relative to the active home directory. In normal
upstream usage that is `$CODEX_HOME/skills`; in this fork's packaged XLI flow
it is usually `$XLI_HOME/skills`, which defaults to `~/.xli/skills`.

This repository also includes:

- repo-local XLI skills under [`../.xli/skills/`](../.xli/skills/)
- sample skills under
  [`../codex-rs/skills/src/assets/samples/`](../codex-rs/skills/src/assets/samples/)

## When to use a skill

Skills are most useful when a task has:

- a repeatable workflow
- domain-specific rules or constraints
- helper scripts or references that should travel with the instructions
- enough complexity that a one-off prompt would be brittle

## Authoring guidance

Keep the entrypoint focused and move bulky detail into references.

Good practices from the in-repo skill authoring guidance:

- make the `name` and `description` specific so the skill triggers correctly
- keep `SKILL.md` concise
- move long reference material into `references/`
- keep generated or UI metadata in `agents/openai.yaml`
- store executable helpers in `scripts/`

## Learn by example

The sample skill creator documentation is the best in-repo reference for skill
anatomy and authoring conventions:

- [`../codex-rs/skills/src/assets/samples/skill-creator/SKILL.md`](../codex-rs/skills/src/assets/samples/skill-creator/SKILL.md)
- [`../SKILLS.md`](../SKILLS.md) for the XLI-specific skill catalog in this repo

## Related topics

- [agents_md.md](agents_md.md) for repository-scoped instructions
- [slash_commands.md](slash_commands.md) for the `/skills` command
- [config.md](config.md) for feature flags and broader runtime customization
