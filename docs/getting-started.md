# Getting started with XLI / Codex CLI

This guide is the fastest way to get from a fresh clone to a working local
session.

> This repository produces two closely related entrypoints:
>
> - `xli` is the packaged distribution used by this fork.
> - `codex` is the upstream/source-build binary name used inside `codex-rs/`.

## 1. Install or build

Choose the path that matches how you want to run the tool:

- **Build from source:** follow [install.md](install.md).
- **Use the packaged launcher:** install the `xli` distribution and use the same
  config format documented in this directory.

If you build from source in this repo, the binary lands at
`codex-rs/target/release/codex`.

## 2. Understand where state lives

The runtime reads config and local state from the active Codex home directory:

- **XLI default:** `~/.xli`
- **Upstream/default engine path:** `~/.codex`
- **Override:** set `CODEX_HOME`; the XLI launcher also honors `XLI_HOME`

In practice, the main file you will care about is:

```text
$CODEX_HOME/config.toml
```

## 3. Create a config file

The quickest path is to copy one of the shipped examples from
[`../examples/`](../examples/):

```bash
mkdir -p ~/.xli
cp examples/anthropic.toml ~/.xli/config.toml
```

For multi-provider setups or more detailed examples, read
[example-config.md](example-config.md) and [`../examples/README.md`](../examples/README.md).

## 4. Configure authentication

There are two common patterns:

- **OpenAI / ChatGPT managed auth:** use `xli login` or `codex login`
- **Provider-native auth (Anthropic, Gemini, custom endpoints):** export the
  env vars referenced by `env_http_headers` in your config

Example:

```bash
export ANTHROPIC_API_KEY="your-key"
```

See [authentication.md](authentication.md) for the full breakdown.

## 5. Run the CLI

### Interactive TUI

```bash
xli
```

or, from a source build:

```bash
cargo run --manifest-path codex-rs/Cargo.toml --bin codex
```

### Run with a profile

```bash
xli -p sonnet
xli -p gemini-pro
```

### One-shot / non-interactive mode

```bash
xli exec "Explain the provider abstraction in codex-api/"
```

## 6. Learn the core controls

The most important first-run commands are:

| Command | What it does |
|---|---|
| `xli` | Launch the interactive TUI |
| `xli -p <profile>` | Switch model/provider bundles defined in `config.toml` |
| `xli exec <prompt>` | Run a non-interactive task |
| `xli login` | Start managed OpenAI/ChatGPT auth |
| `xli logout` | Remove managed auth state |
| `xli sandbox <cmd>` | Run a command inside the host sandbox |
| `xli resume --last` | Resume the most recent saved session |

Inside the TUI, type `/` to open the slash-command picker. Start with
`/model`, `/permissions`, `/status`, `/review`, and `/compact`.

## 7. Know the safety model

The agent's runtime behavior is shaped by two separate controls:

- **Sandbox mode** determines where commands may run and what they may access.
- **Approval policy** determines when the user must approve actions.

Read [sandbox.md](sandbox.md) before using `danger-full-access` or broad write
permissions.

## 8. Where to go next

- [config.md](config.md) for layered config, profiles, hooks, and MCP
- [skills.md](skills.md) for reusable task guidance
- [slash_commands.md](slash_commands.md) for TUI controls
- [agents_md.md](agents_md.md) for repository-scoped instructions
- [architecture/turn-lifecycle-xli.md](architecture/turn-lifecycle-xli.md) for
  runtime internals
