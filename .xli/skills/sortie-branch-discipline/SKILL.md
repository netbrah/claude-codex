---
name: sortie-branch-discipline
description: Load before the first commit of any session. Encodes conventional commit prefixes, one-sortie-branch rule, no merges to dev or feat/xli-embed-assets, and no reverting other agents' commits.
allowed-tools: Read, Bash(git *)
---

# Sortie branch discipline

## When to load
Before the first commit of any working session. Re-read if unsure about
branch or commit conventions.

## Rules (from AGENTS.md — non-negotiable)
1. **Work ONLY on your assigned sortie branch** (`apex/sortie/...`).
2. **Do NOT merge** to `dev` or `feat/xli-embed-assets` — that is C2's job.
3. **Do NOT revert** commits from other agents.
4. **Every commit must have a conventional prefix**:
   - `feat:` — new feature
   - `fix:` — bug fix
   - `test:` — test addition or fix
   - `refactor:` — code restructuring without behavior change
   - `docs:` — documentation only
   - `chore:` — build/tooling/config changes

## Commit style
- Keep commits small and bisect-friendly — no mega-commits.
- The branch must stay green at every commit.
- Prefer clear "why" over "what" in commit messages.

## Branch lifecycle
```
dev (public engine)
  └── apex/sortie/<name>  ← you work here
        ↓ (C2 merges when complete)
      dev
```

## Pre-commit checklist
1. `git status` — only expected files are staged.
2. `git diff --staged` — review the diff.
3. Commit message follows conventional prefix.
4. Tests pass (see `test-gate-xli` skill).

## Anti-patterns
- Committing directly to `dev`.
- Mega-commits that touch 10+ files across unrelated concerns.
- Commit messages like "fix stuff" or "wip".
- Reverting another agent's commit without explicit instruction.
