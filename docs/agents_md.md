# AGENTS.md

`AGENTS.md` files let humans attach durable instructions to a directory tree so
the agent sees project-specific rules without repeating them in every prompt.

## Scope rules

The runtime uses a simple scope model:

1. An `AGENTS.md` file applies to the directory that contains it.
2. It also applies to every child directory beneath that point.
3. If multiple files apply, the deeper one wins for conflicting guidance.
4. System, developer, and user instructions still override everything in
   `AGENTS.md`.

This means a repository can have a root-wide file plus narrower files for a
specific crate, package, or subsystem.

## What belongs in `AGENTS.md`

Good examples include:

- build and test commands
- review expectations
- directory-specific coding conventions
- repository workflow rules
- wording requirements for PRs or handoffs

Avoid duplicating transient task instructions that belong in the current prompt.

## How it is surfaced to the model

The runtime may include relevant `AGENTS.md` content automatically in the
instruction context for a task. This is why a nested file can change behavior
for files under its subtree even when the operator did not mention it explicitly.

The canonical in-repo description of the rule set lives in:

- [`../codex-rs/core/hierarchical_agents_message.md`](../codex-rs/core/hierarchical_agents_message.md)

## `child_agents_md` feature flag

When the `child_agents_md` feature flag is enabled via `[features]` in
`config.toml`, Codex appends additional guidance about `AGENTS.md` scope and
precedence to the user-instructions message and emits that message even when no
`AGENTS.md` file is present.

## Practical guidance for this repo

- Read the repository root `AGENTS.md` first.
- Before editing a nested area, check whether that subtree has its own
  `AGENTS.md`.
- Treat `AGENTS.md` as standing policy, not as a substitute for task-specific
  review.

Related topics:

- [skills.md](skills.md) for reusable task-level instructions
- [config.md](config.md) for feature flags, hooks, and broader customization
