# Execution policy

Execpolicy rules let you describe which commands should be allowed,
prompted, or forbidden when the agent tries to execute tools on the host.

## What execpolicy controls

Execpolicy is command-centric rather than shell-centric. The current engine is
built around prefix rules, so policies match tokenized command prefixes rather
than raw shell strings.

The main decisions are:

- `allow`
- `prompt`
- `forbidden`

## Rule shape

The supported authoring format is Starlark-like:

```starlark
prefix_rule(
    pattern = ["git", "push"],
    decision = "forbidden",
    justification = "Push through the approved release workflow instead.",
    match = ["git push"],
    not_match = ["git status"],
)
```

You can also declare host executable metadata to constrain basename fallback:

```starlark
host_executable(
    name = "git",
    paths = ["/usr/bin/git", "/opt/homebrew/bin/git"],
)
```

## Validate a command against rules

Use the built-in checker to test a ruleset:

```bash
xli execpolicy check --rules path/to/policy.rules git status
```

Helpful variants:

```bash
xli execpolicy check --rules path/to/policy.rules --pretty git status
xli execpolicy check \
  --rules path/to/policy.rules \
  --resolve-host-executables \
  /usr/bin/git status
```

The checker prints JSON describing which rules matched and what effective
decision was chosen.

## How rules are used at runtime

Rules are part of the broader safety envelope alongside sandbox mode and
approval policy:

- **sandbox mode** limits what the process can touch
- **approval policy** decides whether to ask the user first
- **execpolicy** lets you express command-specific intent

The non-interactive CLI also exposes `--ignore-rules` when you need to bypass
user/project `.rules` files intentionally.

## Authoring guidance

- Prefer narrow prefixes over broad catch-all patterns.
- Include `justification` whenever a rule exists for human process reasons.
- Treat `match` and `not_match` examples as executable documentation.
- Use `forbidden` for truly disallowed commands and include a safe alternative
  in the justification when possible.

## More detail

The implementation-specific reference for the current engine lives in
[`../codex-rs/execpolicy/README.md`](../codex-rs/execpolicy/README.md).
