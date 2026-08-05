---
name: rust-workspace-hygiene
description: Use when editing Cargo.toml files, feature flags, deny.toml, clippy.toml, rustfmt.toml, or when the no-anyhow rule at adapter boundaries applies. Owns workspace-level Rust configuration consistency.
allowed-tools: Read, Edit, Grep, Bash(cargo check), Bash(cargo clippy --workspace), Bash(cargo deny check)
---

# Rust workspace hygiene

## When to load
Any diff touching `Cargo.toml` (workspace or crate-level), feature flags,
`deny.toml`, `clippy.toml`, `rustfmt.toml`, or when working at adapter
boundaries where the `anyhow` rule applies.

## Key files
- `codex-rs/Cargo.toml` — workspace root, defines members and shared deps.
- `codex-rs/deny.toml` — `cargo-deny` config for license/advisory/duplicate checks.
- `codex-rs/clippy.toml` — workspace clippy configuration.
- `codex-rs/rustfmt.toml` — formatting rules.
- `codex-rs/rust-toolchain.toml` — pinned Rust toolchain version.

## Rules
1. **No `anyhow` at adapter boundaries**: Adapter crates (copilot, wire
   adapters) use typed errors, not `anyhow::Result`. `anyhow` is fine in
   application-level code (CLI, TUI) but not at library boundaries.
2. **Feature flags**: Don't add feature flags unless necessary. If you do,
   document them in the crate's `Cargo.toml` `[features]` section.
3. **Dependency additions**: Run `cargo deny check` after adding any new
   dependency. Check for license compatibility and duplicate versions.
4. **Workspace deps**: Prefer `workspace = true` inheritance for shared
   dependencies to keep versions aligned.

## Workflow
1. Make the `Cargo.toml` change.
2. `cargo check --workspace` — verify it compiles.
3. `cargo clippy --workspace` — no new warnings.
4. `cargo deny check` — no license/advisory violations.
5. If adding a dep, verify it's not duplicating an existing workspace dep.

## Anti-patterns
- Adding `anyhow` to a library/adapter crate.
- Pinning a dependency version that conflicts with workspace-level pins.
- Adding heavy dependencies without justification (see copilot adapter rule:
  no `chrono`, no `secrecy`, no `native-tls`).
- Ignoring `cargo deny` failures.
