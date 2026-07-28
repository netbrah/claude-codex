---
name: tui-ratatui-rendering
description: Use when editing anything under codex-rs/tui/src/ that draws widgets, layouts, or viewport math. The TUI uses a custom Renderable trait (NOT ratatui's Widget/StatefulWidget). Owns Renderable contract, HistoryCell trait, Rect arithmetic, Buffer writes, and the 20+ cell type implementations.
allowed-tools: Read, Edit, Grep, Bash(cargo nextest run -p codex-tui *)
---

# TUI ratatui rendering

## When to load
Any diff under `codex-rs/tui/src/` that touches widget rendering, layout
calculations, or viewport geometry.

## CRITICAL: Renderable, not Widget

This codebase does **NOT** use ratatui's `Widget` or `StatefulWidget` traits.
All rendering goes through a custom trait:

**`Renderable`** (`render/renderable.rs`):
```rust
pub trait Renderable {
    fn render(&self, area: Rect, buf: &mut Buffer);
    fn desired_height(&self, width: u16) -> u16;
    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> { None }
}
```
Also: `FlexRenderable`, `RenderableItem<'a>` (owned/borrowed dispatch).

## HistoryCell trait

**`HistoryCell`** (`history_cell.rs`): the rendering unit for chat history.
```rust
pub trait HistoryCell: Debug + Send + Sync + Any {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>>;
    fn desired_height(&self, width: u16) -> u16;
    fn transcript_lines(&self, width: u16) -> Vec<Line>;
    fn desired_transcript_height(&self, width: u16) -> u16;
}
```

### HistoryCell implementors (20+)
| Cell | Purpose |
|------|---------|
| `UserHistoryCell` | User messages |
| `AgentMessageCell` | Streamed agent responses |
| `ReasoningSummaryCell` | Reasoning/thinking blocks |
| `PlainHistoryCell` | Plain text |
| `PrefixedWrappedHistoryCell` | Prefixed wrapped text |
| `UnifiedExecInteractionCell` | Exec approval interactions |
| `UnifiedExecProcessesCell` | Process status display |
| `PatchHistoryCell` | File change patches |
| `CompletedMcpToolCallWithImageOutput` | MCP tool results with images |
| `TooltipHistoryCell` | Tooltips/announcements |
| `SessionInfoCell` | Session info display |
| `CompositeHistoryCell` | Grouped cells |
| `McpToolCallCell` | MCP tool call display |
| `WebSearchCell` | Web search results |
| `ExecCell` | Command execution display (`exec_cell/render.rs`) |

## Main widget structs
| Struct | File | Role |
|--------|------|------|
| `ChatWidget` | `chatwidget.rs` | Main chat surface — owns BottomPane, active_cell, streaming controllers |
| `BottomPane` | `bottom_pane/mod.rs` | Footer container — composer + view stack |
| `ChatComposer` | `bottom_pane/chat_composer.rs` | Prompt input state machine |
| `StatusIndicatorWidget` | `status_indicator_widget.rs` | Spinner + status while task running |
| `Tui` | `tui.rs` | Terminal wrapper — owns `FrameRequester`, `EventBroker`, `Terminal` |

## Key files
| File | Size | Role |
|------|------|------|
| `app.rs` | ~11K lines | Main app struct + event loop |
| `chatwidget.rs` | ~11.5K lines | Chat surface widget |
| `history_cell.rs` | ~4.7K lines | HistoryCell trait + 20+ implementations |
| `diff_render.rs` | Large | Diff rendering + syntax highlight |
| `pager_overlay.rs` | Large | Transcript overlay (`Ctrl+T`) |
| `markdown_stream.rs` | ~726 lines | Newline-gated markdown accumulator |

## Workflow
1. Identify whether you're adding a new `HistoryCell` type or modifying
   an existing `Renderable` implementation.
2. Implement `display_lines()` + `desired_height()` for any new cell type.
3. Write or adjust a snapshot test using `VT100Backend`.
4. Verify with `cargo nextest run -p codex-tui`.
5. Edge cases to check: empty content, single-line, overflow, terminal
   resize to very small dimensions.

## Anti-patterns
- Implementing ratatui `Widget` or `StatefulWidget` — use `Renderable` instead.
- Raw `x + width` arithmetic instead of `Rect` methods.
- Using `str::len()` for column-width calculations (use `unicode-width`).
- Assuming fixed terminal size in layout code.
