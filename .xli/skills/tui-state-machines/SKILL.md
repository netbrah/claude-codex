---
name: tui-state-machines
description: Use when editing codex-rs/tui/src/bottom_pane/, chat_composer.rs, paste_burst.rs, or any keyboard-driven FSM in the TUI. Owns Enter/newline semantics, retro-capture, paste-burst flush/clear rules, disable_paste_burst, non-ASCII/IME invariants. Keeps docs/tui-chat-composer.md and module docstrings in sync with code.
allowed-tools: Read, Edit, Grep, Bash(cargo nextest run -p codex-tui *)
---

# TUI state machines

## When to load
Any diff under `codex-rs/tui/src/bottom_pane/` or touching key-handling FSMs.

## Invariants (from bottom_pane/AGENTS.md — do not violate)
1. Module docs in `chat_composer.rs` / `paste_burst.rs` stay a readable top-down
   explanation of current behavior.
2. `docs/tui-chat-composer.md` is updated whenever Enter handling, retro-capture,
   flush/clear rules, `disable_paste_burst`, or IME handling change.
3. Implementations and docstrings stay aligned unless divergence is intentional
   and documented in the same PR.

## BottomPane architecture

**`BottomPane`** (`bottom_pane/mod.rs`):
```rust
pub struct BottomPane {
    composer: ChatComposer,
    view_stack: Vec<Box<dyn BottomPaneView>>,
    status: Option<StatusIndicatorWidget>,
    unified_exec_footer: UnifiedExecFooter,
    pending_input_preview: PendingInputPreview,
    // ... flags: has_input_focus, is_task_running, esc_backtrack_hint
}
```

**`BottomPaneView`** trait (`bottom_pane/bottom_pane_view.rs`):
```rust
pub trait BottomPaneView: Renderable {
    fn handle_key_event(&mut self, key_event: KeyEvent) {}
    fn is_complete(&self) -> bool { false }
    fn on_ctrl_c(&mut self) -> CancellationEvent { ... }
    fn handle_paste(&mut self, pasted: String) -> bool { false }
    fn flush_paste_burst_if_due(&mut self) -> bool { false }
    fn is_in_paste_burst(&self) -> bool { false }
}
```

## ChatComposer FSM

**`ChatComposer`** (`bottom_pane/chat_composer.rs`):
- Owns `TextArea`, `ChatComposerHistory`, `ActivePopup`, `PasteBurst`
- Key state: `footer_mode: FooterMode`, `quit_shortcut_expires_at`,
  `pending_pastes`, `remote_image_urls`

**`InputResult`** — what the composer produces on submit:
```rust
pub enum InputResult {
    Submitted { text, text_elements },
    Queued { text, text_elements },
    Command(SlashCommand),
    CommandWithArgs(SlashCommand, String, Vec<TextElement>),
    None,
}
```

**`ActivePopup`** — popup state:
- `None`, `Command(CommandPopup)`, `File(FileSearchPopup)`, `Skill(SkillPopup)`

**`FooterMode`** — footer display state:
- `QuitShortcutReminder`, `ShortcutOverlay`, `EscHint`, `ComposerEmpty`, `ComposerHasDraft`

## PasteBurst FSM

**`PasteBurst`** (`bottom_pane/paste_burst.rs`):

### Conceptual states
1. **Idle** — no buffered text, no pending char
2. **Pending first char** — holds one ASCII char for `PASTE_BURST_CHAR_INTERVAL`
   while watching for burst
3. **Active buffer** — accumulating paste-like rapid input
4. **Enter suppress window** — `burst_window_until` keeps Enter as newline
   briefly after burst ends

### Core types
```rust
pub struct PasteBurst {
    last_plain_char_time: Option<Instant>,
    consecutive_plain_char_burst: u16,
    burst_window_until: Option<Instant>,
    buffer: String,
    active: bool,
    pending_first_char: Option<(char, Instant)>,
}
```

**`CharDecision`**: `BeginBuffer { retro_chars }` | `BufferAppend` |
  `RetainFirstChar` | `BeginBufferFromPending`

**`FlushResult`**: `Paste(String)` | `Typed(char)` | `None`

**`RetroGrab`**: `{ start_byte, grabbed }` — for retro-capturing chars
already in the TextArea.

### Timing constants
- `PASTE_BURST_MIN_CHARS`: 3
- `PASTE_BURST_CHAR_INTERVAL`: 8ms (30ms on Windows)
- `PASTE_BURST_ACTIVE_IDLE_TIMEOUT`: 8ms (60ms on Windows)
- `PASTE_ENTER_SUPPRESS_WINDOW`: 120ms

## Workflow
1. Read current module doc + `docs/tui-chat-composer.md` before editing.
2. Write/adjust a failing test in `codex-rs/tui/tests/` or inline `#[cfg(test)]`.
3. Implement the smallest change.
4. Update module docstring + narrative doc in the SAME commit.
5. `cargo nextest run -p codex-tui` green.
6. Sanity: grep the narrative doc for any API it mentions; every symbol must exist.

## Anti-patterns
- Adding a new key path without extending the state diagram in the module doc.
- Splitting code + doc updates across commits (breaks bisect narrative).
- Using `anyhow` in the FSM layer.
- Modifying timing constants without testing on both fast (Linux) and slow (Windows) paths.
