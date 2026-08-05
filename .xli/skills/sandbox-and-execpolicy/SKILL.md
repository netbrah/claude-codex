---
name: sandbox-and-execpolicy
description: Use when touching linux-sandbox, windows-sandbox-rs, execpolicy, shell-escalation, or process-hardening. These are non-negotiable safety invariants. Any change requires extra review rigor and must not weaken the security boundary.
allowed-tools: Read, Edit, Grep, Bash(cargo nextest run -p linux-sandbox *), Bash(cargo nextest run -p execpolicy *), Bash(cargo nextest run -p process-hardening *)
---

# Sandbox and execpolicy

## When to load
Any diff touching `codex-rs/linux-sandbox/`, `codex-rs/windows-sandbox-rs/`,
`codex-rs/execpolicy/`, `codex-rs/shell-escalation/`, or
`codex-rs/process-hardening/`.

## These are safety-critical
Changes to sandbox and exec policy code are **non-negotiable invariants**.
A weakened sandbox = arbitrary code execution by the model. Treat every
change here with the same rigor as a cryptographic library change.

## Key crates
- **linux-sandbox**: Landlock + seccomp-based sandboxing for Linux.
- **windows-sandbox-rs**: Windows-specific sandbox implementation.
- **execpolicy**: Policy engine that decides what commands the model can run
  (allow, deny, ask-user). Configured via `execpolicy.toml`.
- **shell-escalation**: Detects and prevents shell escape attempts.
- **process-hardening**: OS-level process hardening (e.g., no-new-privs).

## Invariants
1. The sandbox MUST default to deny-all. Allowlists are explicit.
2. Exec policy changes must not silently widen the allowed command set.
3. Shell escalation detection must not have false negatives (false positives
   are acceptable — they just prompt the user).
4. Process hardening must be applied before any model-triggered command runs.

## Workflow
1. Read the current policy/sandbox code thoroughly.
2. Identify the exact security boundary being modified.
3. Write a test that exercises the boundary (both allowed and denied cases).
4. Implement the change.
5. Verify: `cargo nextest run` for all affected crates.
6. Consider: "Could a malicious model input bypass this?"

## Anti-patterns
- Widening the sandbox without explicit justification.
- Adding new allowed commands to execpolicy without tests.
- Disabling security features "temporarily" for debugging.
- Using `unsafe` in sandbox code without a `// SAFETY:` comment.
