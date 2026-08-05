# Documentation Index

This directory is the main documentation hub for the `netbrah/claude-codex`
repository.

> In this fork, the packaged distribution is branded as `xli` and typically keeps
> state in `~/.xli`. Source-built binaries and upstream docs may still refer to
> `codex` and `~/.codex`; they use the same config format and runtime model.

## Start here

| If you want to... | Read |
|---|---|
| Install or build the CLI | [install.md](install.md) |
| Get productive quickly | [getting-started.md](getting-started.md) |
| Set up auth | [authentication.md](authentication.md) |
| Understand config files and overrides | [config.md](config.md) |
| Copy a working config example | [example-config.md](example-config.md) |
| Run the agent non-interactively | [exec.md](exec.md) |
| Understand sandboxing and approvals | [sandbox.md](sandbox.md) |
| Author or validate execpolicy rules | [execpolicy.md](execpolicy.md) |
| Work with skills | [skills.md](skills.md) |
| Learn the built-in slash commands | [slash_commands.md](slash_commands.md) |
| Understand `AGENTS.md` scope and precedence | [agents_md.md](agents_md.md) |
| Contribute changes | [contributing.md](contributing.md) |

## Architecture and deep dives

| Topic | Read |
|---|---|
| Turn lifecycle and runtime flow | [architecture/turn-lifecycle-xli.md](architecture/turn-lifecycle-xli.md) |
| Anthropic wire audit | [audit-2026-04-29/anthropic-wire-fidelity-opus-47.md](audit-2026-04-29/anthropic-wire-fidelity-opus-47.md) |
| Integration engineering trail | [integration/README.md](integration/README.md) |

## Repository landmarks

| Path | Purpose |
|---|---|
| [`../README.md`](../README.md) | Product overview, architecture, and quick-start examples |
| [`../examples/README.md`](../examples/README.md) | Provider-native sample configs for Anthropic and Gemini |
| [`../sorties/README.md`](../sorties/README.md) | Sortie workflow, archive, and operational notes |
| [`../sdk/python/docs/`](../sdk/python/docs/) | Python SDK user documentation |
| [`../codex-rs/README.md`](../codex-rs/README.md) | Rust workspace overview and CLI surface details |

## Suggested reading paths

### New user

1. [install.md](install.md)
2. [getting-started.md](getting-started.md)
3. [authentication.md](authentication.md)
4. [example-config.md](example-config.md)

### Power user or operator

1. [config.md](config.md)
2. [sandbox.md](sandbox.md)
3. [execpolicy.md](execpolicy.md)
4. [skills.md](skills.md)

### Contributor

1. [contributing.md](contributing.md)
2. [agents_md.md](agents_md.md)
3. [architecture/turn-lifecycle-xli.md](architecture/turn-lifecycle-xli.md)
4. [integration/README.md](integration/README.md)
