# Non-interactive mode

Use `xli exec` (or `codex exec` in a source build) when you want the agent to
run to completion without opening the fullscreen TUI.

## Basic usage

```bash
xli exec "Summarize the provider abstraction in this repository"
```

The command runs a single session, prints output directly to the terminal, and
exits when the task is complete.

## Prompt input modes

`exec` supports three common input patterns:

### Positional prompt

```bash
xli exec "Review the diff and summarize risks"
```

### Prompt from stdin

```bash
printf 'Summarize this changelog' | xli exec -
```

### Prompt argument plus piped stdin context

When you provide both, stdin is appended as a `<stdin>` block after the main
prompt.

```bash
git diff | xli exec "Summarize this patch"
```

## High-value flags

| Flag                         | Purpose                                      |
| ---------------------------- | -------------------------------------------- |
| `--ephemeral`                | Run without persisting session rollout files |
| `--json`                     | Emit JSONL events to stdout                  |
| `--output-last-message FILE` | Write the final assistant message to a file  |
| `--ignore-user-config`       | Skip loading `$CODEX_HOME/config.toml`       |
| `--ignore-rules`             | Skip user/project execpolicy `.rules` files  |
| `--skip-git-repo-check`      | Allow execution outside a Git repo           |

## Review and resume subcommands

The non-interactive surface also exposes focused subcommands for automation
workflows:

- `xli exec review ...`
- `xli exec resume ...`

Use these when you want review-only or resume-oriented flows without entering
the TUI.

## Sandbox and approvals still apply

`exec` is still governed by the same sandbox and approval settings as the
interactive UI. Use the same config fields and CLI flags you would use for a
normal session, such as `--sandbox workspace-write`.

## Good use cases

- CI-friendly summarization or analysis runs
- scripted repository reviews
- automation that wants machine-readable event streams with `--json`
- one-shot prompts that do not need an interactive thread

For installation and local development commands, see [install.md](install.md).
For approval and sandbox behavior, see [sandbox.md](sandbox.md).
