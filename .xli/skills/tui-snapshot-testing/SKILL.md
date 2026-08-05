---
name: tui-snapshot-testing
description: Use when adding or updating TUI tests. Owns VT100Backend test infrastructure, insta snapshot patterns, deterministic terminal setup, and the chatwidget test module structure (12+ submodules). Knows what NOT to snapshot.
allowed-tools: Read, Edit, Grep, Bash(cargo nextest run -p codex-tui *), Bash(cargo insta review)
---

# TUI snapshot testing

## When to load
Adding or updating any test for the TUI crate.

## Test backend — VT100Backend

**`VT100Backend`** (`test_backend.rs`):
- Wraps `CrosstermBackend<vt100::Parser>` to mock a real terminal
- Implements ratatui `Backend` trait
- Implements `fmt::Display` → dumps `screen.contents()`
- Usage: `insta::assert_snapshot!(terminal.backend())`

**NOT ratatui's `TestBackend`** — this codebase uses `VT100Backend` which
provides more realistic terminal emulation including ANSI escape processing.

## Snapshot framework: insta

Pattern:
```rust
insta::assert_snapshot!("test_name", terminal.backend());
```

Snapshot files live in `snapshots/` directories alongside test files:
- `src/snapshots/`
- `src/bottom_pane/snapshots/`
- `src/chatwidget/snapshots/`

## Test module structure

### Integration tests (`tests/`)
| File | Tests |
|------|-------|
| `all.rs` | Single binary, aggregates `suite/` modules |
| `suite/no_panic_on_startup.rs` | Smoke test |
| `suite/status_indicator.rs` | Status widget tests |
| `suite/vt100_history.rs` | History rendering |
| `suite/vt100_live_commit.rs` | Live streaming commit behavior |
| `suite/model_availability_nux.rs` | Model availability UX |
| `test_backend.rs` | Re-exports `VT100Backend` for integration use |

### In-crate unit tests — `chatwidget/tests/` (12+ submodules)
| Module | Covers |
|--------|--------|
| `app_server.rs` | App server integration |
| `approval_requests.rs` | Exec approval flows |
| `background_events.rs` | Background event handling |
| `composer_submission.rs` | Input submission |
| `exec_flow.rs` | Execution flow |
| `guardian.rs` | Safety guardian |
| `helpers.rs` | Test utilities |
| `history_replay.rs` | History replay |
| `mcp_startup.rs` | MCP initialization |
| `permissions.rs` | Permission flows |
| `plan_mode.rs` | Plan mode |
| `popups_and_settings.rs` | Popup state machines |
| `review_mode.rs` | Review mode |
| `slash_commands.rs` | Slash command parsing |
| `status_and_layout.rs` | Status + layout rendering |

## What NOT to snapshot
- Timestamps, cursor blink state, spinner animation frames
- Any time-dependent or animation-dependent content
- Mock or freeze these in test setup

## Workflow
1. Create a `VT100Backend`-backed terminal (fixed size).
2. Set up the widget/component state with representative data.
3. Render and snapshot: `insta::assert_snapshot!(terminal.backend())`.
4. Review with `cargo insta review`.
5. Commit the `.snap` file alongside the test.

## Anti-patterns
- Using ratatui `TestBackend` instead of `VT100Backend`.
- Using the real terminal size (`crossterm::terminal::size()`) in tests.
- Snapshotting timestamps or animated content.
- Accepting snapshot changes without reviewing (`cargo insta accept --all`).
