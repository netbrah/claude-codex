---
name: tui-event-loop
description: Use when working on crossterm input handling, paste bracketing, resize events, tick cadence, or async bridging into the render thread. Owns the 4-branch tokio select! loop in app.rs, TuiEvent enum, EventBroker, FrameRequester, and backpressure rules.
allowed-tools: Read, Edit, Grep, Bash(cargo nextest run -p codex-tui *)
---

# TUI event loop

## When to load
Any diff touching crossterm event reading, terminal resize handling, paste
bracket detection, tick/refresh cadence, or the async bridge between the
model stream and the render loop.

## The main loop — `app.rs` (~L3926)

The event loop is a `tokio::select!` with **4 branches**:

```rust
loop {
    let control = select! {
        // 1. Internal app events (from AppEventSender)
        Some(event) = app_event_rx.recv() => {
            app.handle_event(tui, &mut app_server, event).await
        }

        // 2. Active thread protocol events (ResponseEvent from codex-core)
        active = async { app.active_thread_rx.as_mut()?.recv().await },
            if App::should_handle_active_thread_events(...) => {
            app.handle_active_thread_event(tui, &mut app_server, event).await
        }

        // 3. TUI/terminal events (crossterm keyboard, paste, draw ticks)
        Some(event) = tui_events.next() => {
            app.handle_tui_event(tui, &mut app_server, event).await
        }

        // 4. App-server events (embedded server protocol)
        app_server_event = app_server.next_event(),
            if listen_for_app_server_events => {
            app.handle_app_server_event(&app_server, event).await
        }
    };
    match control {
        AppRunControl::Continue => {}
        AppRunControl::Exit(reason) => break Ok(reason),
    }
}
```

### Control flow types
- `AppRunControl`: `Continue | Exit(ExitReason)`
- `ExitReason`: `UserRequested | Fatal(String)`

## TuiEvent — `tui.rs`

```rust
pub enum TuiEvent {
    Key(KeyEvent),     // keyboard input
    Paste(String),     // bracketed paste
    Draw,              // frame tick / redraw request
}
```

### TuiEvent dispatch in `handle_tui_event()` (`app.rs`):
- `TuiEvent::Key` → `handle_key_event()`
- `TuiEvent::Paste` → `chat_widget.handle_paste()` (normalizes `` → `
`)
- `TuiEvent::Draw` → paste burst tick → pre_draw_tick → `tui.draw()`

## EventBroker — `tui/event_stream.rs`

Wraps a shared crossterm `EventStream` that can be **paused/resumed**
(drop/recreate). This allows stdin to be fully relinquished when spawning
external editors or subprocesses.

## FrameRequester — `tui/frame_requester.rs`

Decouples draw requests from the actual render. Components call
`request_frame()` to signal that a redraw is needed. The frame rate limiter
(`tui/frame_rate_limiter.rs`) coalesces rapid requests.

## Key files
| File | Role |
|------|------|
| `app.rs` | Main app struct + event loop (~11K lines) |
| `tui.rs` | `Tui` struct, terminal init/restore, `TuiEvent` enum |
| `tui/event_stream.rs` | `EventBroker` — crossterm EventStream wrapper |
| `tui/frame_rate_limiter.rs` | Coalesces rapid draw requests |
| `tui/frame_requester.rs` | `request_frame()` API |
| `tui/job_control.rs` | SIGTSTP/SIGCONT handling |
| `app_event.rs` | `AppEvent` enum — internal message bus |

## Backpressure rules
- When the model streams tokens faster than the TUI can render, the
  `StreamController` coalesces chunks (see `tui-streaming-render` skill).
- **Never call `tui.draw()` more than once per tick.**
- Process all pending crossterm events before model events to keep input
  responsive — the `select!` ordering matters.

## Workflow
1. Identify which `select!` branch is affected.
2. Write a test that simulates the event sequence (use `VT100Backend` +
   synthetic events).
3. Implement the change.
4. Verify no tearing: run with a fast-streaming model and resize rapidly.
5. `cargo nextest run -p codex-tui` green.

## Anti-patterns
- Calling `tui.draw()` from multiple `select!` branches in the same iteration.
- Blocking the event loop with synchronous I/O.
- Processing `KeyEvent`s individually during a bracketed paste (use
  `TuiEvent::Paste` path instead).
- Using `thread::sleep` instead of `tokio::time::sleep` in async contexts.
- Forgetting to pause `EventBroker` before spawning an external editor.
