---
name: tui-streaming-render
description: Use when working on the path from ResponseEvent to visible text in the TUI. Highest-risk area for flicker and reflow bugs. Owns StreamController, StreamState, MarkdownStreamCollector, AdaptiveChunkingPolicy (two-gear smooth/catch-up system), and commit_tick orchestration.
allowed-tools: Read, Edit, Grep, Bash(cargo nextest run -p codex-tui *), Bash(cargo nextest run -p ansi-escape *)
---

# TUI streaming render

## When to load
Any diff where `ResponseEvent` flows into visible rendered text — the full
path from wire event to terminal output.

## Pipeline stages

### 1. StreamController (`streaming/controller.rs`)
- `push(delta: &str)` → feeds `MarkdownStreamCollector`
- When delta contains `
`, commits complete lines to internal queue
- `finalize()` → drains remaining → produces `HistoryCell`
- `on_commit_tick()` / `on_commit_tick_batch()` → drains queued lines →
  emits `AgentMessageCell`

### 2. StreamState (`streaming/mod.rs`)
- Owns `MarkdownStreamCollector` + `VecDeque<QueuedLine>`
- `step()` drains 1 line, `drain_n()` drains N, `drain_all()` drains all
- Each `QueuedLine` has `enqueued_at: Instant` for age-based policy

### 3. MarkdownStreamCollector (`markdown_stream.rs`)
- Newline-gated: accumulates deltas, renders markdown, emits only complete lines
- `commit_complete_lines()` → renders buffer up to last `
`, returns new
  lines since last commit
- Handles partial markdown gracefully (e.g., incomplete code fence shows raw
  text until fence closes)

### 4. AdaptiveChunkingPolicy (`streaming/chunking.rs`)
Two-gear system that prevents both choppiness and lag:

| Mode | Behavior | Entry | Exit |
|------|----------|-------|------|
| **Smooth** | 1 line per tick | Default | — |
| **CatchUp** | Drain all queued | ≥8 lines OR oldest ≥120ms | ≤2 lines AND age ≤40ms, held 250ms |

- **Hysteresis**: 250ms re-entry hold after exiting CatchUp
- Key types:
  - `ChunkingMode`: `Smooth | CatchUp`
  - `DrainPlan`: `Single | Batch(usize)`
  - `ChunkingDecision`: `{ mode, entered_catch_up, drain_plan }`
  - `QueueSnapshot`: `{ queued_lines, oldest_age }`

### 5. commit_tick (`streaming/commit_tick.rs`)
- `run_commit_tick()` orchestrates: snapshot → policy decide → apply drain plan
- `CommitTickScope`: `AnyMode | CatchUpOnly`
- `CommitTickOutput`: `{ cells: Vec<Box<dyn HistoryCell>>, has_controller, all_idle }`

### Also: PlanStreamController (`controller.rs`)
Styled variant for rendering plan blocks (not just text).

## Key files
| File | Role |
|------|------|
| `streaming/controller.rs` | StreamController — delta → committed lines |
| `streaming/mod.rs` | StreamState — line queue management |
| `streaming/chunking.rs` | AdaptiveChunkingPolicy — smooth/catch-up gears |
| `streaming/commit_tick.rs` | Commit tick orchestration |
| `markdown_stream.rs` | Newline-gated markdown accumulator |
| `history_cell.rs` | `AgentMessageCell` — the rendered output cell |

## Workflow
1. Identify which stage is affected (coalescing, line tracking, markdown
   parsing, chunking policy, or commit tick).
2. Write a test with representative streaming input sequences.
3. Implement the change.
4. Test with real streaming output (fast model) to verify no flicker.
5. `cargo nextest run -p codex-tui` and `cargo nextest run -p ansi-escape` green.

## Anti-patterns
- Rendering each `OutputTextDelta` as a separate `tui.draw()` call.
- Re-parsing the entire output buffer on every delta instead of appending.
- Ignoring ANSI sequences in tool output (use `ansi-escape` crate).
- Force-scrolling to bottom when the user has scrolled up.
- Changing chunking thresholds without testing both smooth typing and
  high-throughput streaming scenarios.
