# Configuration

The CLI uses TOML configuration layered from the active Codex home directory,
project-level overrides, profiles, and command-line overrides.

## Config locations

The runtime resolves config from the active home directory:

- **XLI launcher default:** `~/.xli/config.toml`
- **Engine/default path:** `~/.codex/config.toml`
- **Override:** `CODEX_HOME=/path/to/home`

A project can also provide a local override file at:

```text
.codex/config.toml
```

That local file is useful for repository-specific defaults such as model,
sandbox, hooks, or MCP configuration.

## Configuration layers

In practice, the final runtime config is composed from:

1. user config in the active Codex home
2. project config in `.codex/config.toml`
3. profile selection via `-p <name>`
4. individual CLI overrides via `-c key=value`

This makes profiles the best place for repeatable presets and `-c` the best
place for one-off experiments.

## Core fields

These are the settings most users touch first:

| Field                     | Purpose                                 |
| ------------------------- | --------------------------------------- |
| `model`                   | Default model slug                      |
| `model_provider`          | Which provider definition to use        |
| `model_reasoning_effort`  | Reasoning budget / thinking depth       |
| `model_reasoning_summary` | Whether reasoning summaries are emitted |
| `approval_policy`         | When the user must approve actions      |
| `sandbox_mode`            | Filesystem/process sandbox profile      |
| `web_search`              | Search mode for models that support it  |

## Model providers

Provider definitions live under `[model_providers.<name>]`.

Typical fields include:

- `name`
- `base_url`
- `wire_api`
- `requires_openai_auth`
- `env_key`
- `env_http_headers`
- `http_headers`
- `query_params`

Use [`../examples/`](../examples/) for working Anthropic and Gemini examples.

## Profiles

Profiles are named presets under `[profiles.<name>]`.

They are the safest way to switch both model and provider together:

```toml
[profiles.opus]
model = "claude-opus-4-7"
model_provider = "anthropic"

[profiles.gemini-pro]
model = "gemini-3.1-pro-preview"
model_provider = "gemini"
```

Run them with:

```bash
xli -p opus
xli -p gemini-pro
```

## Command-line overrides

Use `-c key=value` for one-off changes without editing the file:

```bash
xli -c model_reasoning_effort=high
xli -c approval_policy=on-request -c sandbox_mode=workspace-write
```

## Notify

You can run a notification hook when the agent finishes a turn. The exact
command lives in config and can be tailored to your desktop environment.

Common use cases:

- desktop notifications when a background task finishes
- audible alerts for approval prompts
- operator workflows that react to completed turns

On macOS, `terminal-notifier` is a common choice. Under WSL2 in Windows
Terminal, the TUI can fall back to native Windows toast notifications.

## Connecting to MCP servers

The CLI can connect to Model Context Protocol servers defined in config.
MCP servers let you surface external tools and data sources inside the same
agent workflow.

Common patterns include:

- local command-based MCP servers
- remote HTTP MCP servers
- per-tool approval overrides for sensitive tools

The relevant config table is `mcp_servers`. If you are editing these entries
programmatically, the workspace also contains helpers for serializing them in
`codex-rs/config/src/mcp_edit.rs`.

## Features

Experimental and optional behavior can be toggled under `[features]`. Feature
flags are also allowed inside profiles for per-profile behavior changes.

Examples from this repo include feature flags for collaboration modes and the
`child_agents_md` behavior described in [agents_md.md](agents_md.md).

## Lifecycle hooks

Hooks can be configured to run at specific lifecycle events. They are useful
for notifications, auditing, or enforcing local workflow rules.

Admins can also set top-level `allow_managed_hooks_only = true` in
`requirements.toml` to ignore user, project, and session hook configs while
still allowing managed hooks from requirements and managed config layers.
This setting is supported in `requirements.toml`, not in `config.toml`.

## Related files

| Need                          | Read                                             |
| ----------------------------- | ------------------------------------------------ |
| Copy-paste examples           | [example-config.md](example-config.md)           |
| Provider-native examples      | [`../examples/README.md`](../examples/README.md) |
| Sandbox and approval behavior | [sandbox.md](sandbox.md)                         |
| Execpolicy rules              | [execpolicy.md](execpolicy.md)                   |
