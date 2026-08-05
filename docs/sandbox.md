# Sandbox & approvals

Codex/XLI separates **where commands may run** from **when the user must approve
them**.

- **Sandbox mode** controls the execution environment.
- **Approval policy** controls consent and escalation behavior.

You usually want to think about both together.

## Sandbox modes

| Mode | Intended use |
|---|---|
| `read-only` | Inspect the repo without changing files |
| `workspace-write` | Allow edits inside the workspace while keeping stronger isolation |
| `danger-full-access` | Disable the normal sandbox; only use inside another trusted isolation boundary |

The CLI also exposes a dedicated flag:

```bash
xli --sandbox read-only
xli --sandbox workspace-write
xli --sandbox danger-full-access
```

In `workspace-write`, the runtime also includes the active memories path in the
writable roots so memory maintenance does not require a separate approval step.
In the XLI launcher, that usually means `~/.xli/memories`.

## Approval policies

Common policies include:

| Policy | Behavior |
|---|---|
| `on-request` | Ask when a command needs escalation or broader permissions |
| `on-failure` | Try in the sandbox first, then escalate if needed |
| `unless-trusted` | Escalate most commands except a small allowlist of safe reads |
| `never` | Do not request approvals; stay within the configured sandbox envelope |

Exact behavior can vary with the environment, but this is the right mental
model when you are choosing a session profile.

## Try the sandbox directly

The `sandbox` subcommand is useful when you want to see how a command behaves
under the host sandbox without running a full agent session.

```bash
xli sandbox -- pwd
xli sandbox --log-denials -- cat /etc/hosts
```

The subcommand also accepts `--profile NAME` so you can test with a named
config overlay.

## Choosing a good default

- Use `read-only` for review, reconnaissance, and architecture questions.
- Use `workspace-write` for normal implementation work.
- Reserve `danger-full-access` for environments that are already isolated by
  some other control, such as a disposable container.

## Related controls

- [execpolicy.md](execpolicy.md) for command-specific policy rules
- [config.md](config.md) for `approval_policy` and `sandbox_mode`
- [`../codex-rs/README.md`](../codex-rs/README.md) for the CLI surface
