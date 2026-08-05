# OP2 — Subagent UX Spike (Scoping Only)

**Sortie**: OP2-Hardening / Recon  
**Author**: Iceman (synthesized from 4-track recon at HEAD abd205ad73)  
**Status**: SCOPING — no code changes  
**Date**: 2026-04-19

**Canonical paired doc**: `../docs/integration/06-op2-hardening-recon.md` holds the consolidated hardening decision matrix; this file is the supporting UX spike focused on the subagent-rendering branch of that recon.

---

## a) Status Quo — What Happens Today

### Spawn Chain

When a Copilot-wire parent session spawns a subagent, the flow is:

1. **Tool invocation** — Model calls `spawn_agent` (v1 or v2).  
   Handler: `core/src/tools/handlers/multi_agents_v2/spawn.rs:106-137`

2. **AgentControl::spawn_agent_internal** — `core/src/agent/control.rs:180-324`  
   - Inherits parent's shell snapshot (`:189-191`)  
   - Inherits parent's exec policy (`:192-194`)  
   - Fork mode optionally copies parent rollout history (`:219-229`, `:381-388`)  
   - Calls `ThreadManagerState::spawn_new_thread_with_source` (`:231-244`)

3. **Codex::spawn** — `core/src/codex.rs:445-703`  
   - Creates async channels: `tx_sub` (bounded 512), `tx_event` (unbounded) (`:490-491`)  
   - **Auth**: Shared `Arc<AuthManager>` — same reference, not copied credentials (`:420, 666`)  
   - Spawns tokio submission loop (`:687-693`)

### Token/Auth Handoff

- Parent and child share `Arc<AuthManager>` via `CodexSpawnArgs` (`:420`).  
- **Copilot wire has a separate problem**: `OnceLock<Arc<SessionState>>` at `copilot/src/adapter.rs:91` is *process-scoped*. Subagents spawned as tokio tasks (same process) reuse it. If subagents were ever separate processes, they'd need independent token discovery.

### Stdio Routing

**No shell pipes.** All I/O is channel-based:
- Parent → child: `Op` via `async_channel::bounded(512)` (`:490`)
- Child → parent: `ResponseEvent` via `async_channel::unbounded()` (`:491`)

### Lifecycle

- **Status watch**: `control.rs:777-784` — parent subscribes via `tokio::sync::watch::Receiver<AgentStatus>`
- **Completion**: `control.rs:912-921` — polls status changes until final state
- **Notification**: `control.rs:934-968` — sends `InterAgentCommunication` (v2) or injects user message into parent (v1)

### TUI Rendering (Current)

Subagent events render as **inline `PlainHistoryCell` entries** in the parent's transcript:

| Event | Render | Source |
|-------|--------|--------|
| `CollabAgentSpawnEnd` | `"Spawned agent-name [role] (#thread-id)"` + prompt preview (160 graphemes) | `tui/src/multi_agents.rs:174-207` |
| `CollabAgentInteractionEnd` | `"Sent input to agent-name"` + prompt preview | `tui/src/multi_agents.rs:209-235` |
| `CollabWaitingBegin` | `"Waiting for agent-name"` or `"Waiting for N agents"` + status list | `tui/src/multi_agents.rs:237-280` |
| `CollabWaitingEnd` | `"Received from agent-name"` + response preview (240 graphemes) | `tui/src/multi_agents.rs` |

**Navigation**: `AgentNavigationState` (`tui/src/app/agent_navigation.rs:39-195`) tracks all threads in insertion order. Operator cycles with Next/Previous keybindings, or `/subagents` picker. Each thread gets its own `ChatWidget` viewport — but **only one is visible at a time** (no split panes).

**Key insight**: Today's UX is **Option D** (status line + final result) for the parent view, with an escape hatch to **switch into** the subagent's thread to see its full transcript. The subagent itself has no independent TUI — it's a headless thread whose output is only visible when the operator explicitly navigates to it.

---

## b) UX Options Matrix

### Option A: Nested Session Pane (Split View)

| Dimension | Detail |
|-----------|--------|
| **What operator sees** | Terminal splits horizontally or vertically. Parent stream in top/left pane, subagent stream in bottom/right. Each pane independently scrollable. |
| **Stream attachment** | Each pane owns its own `StreamController` + `MarkdownStreamCollector`. Both drain on the same frame tick via `run_commit_tick()`. |
| **Interrupt propagation** | Active pane receives Ctrl+C. Needs a focus indicator (highlighted border or status bar). Tab or keybinding to switch focus. Unfocused pane ignores keyboard. |
| **Context distinction** | Border + header bar per pane showing agent name/role/thread-id. Color-coding (e.g., parent = default, subagent = dimmed or tinted). |
| **Complexity** | **HIGH.** ratatui layout is single `Rect` per `render()` call. Would need: (1) new layout splitter in `app.rs:4048`, (2) multiple `ChatWidget` instances rendered per frame, (3) focus management layer, (4) resize logic. Estimated 800-1200 LOC. |

### Option B: Inline Interleaved (Collapsed Blocks)

| Dimension | Detail |
|-----------|--------|
| **What operator sees** | Subagent turns appear as collapsible blocks in the parent transcript: `▸ agent-name [worker]: "Analyzing auth module..."`. Expand to see full response. |
| **Stream attachment** | Single `StreamController` for parent. Subagent messages injected as `HistoryCell` entries (already the pattern for `CollabAgentSpawnEnd`). Progressive streaming would require a secondary `StreamController` that interleaves into the parent's transcript. |
| **Interrupt propagation** | Same as today — Ctrl+C goes to parent's active stream. Subagent interruption via parent model deciding to close/cancel. |
| **Context distinction** | Indented or prefixed lines. Expand/collapse toggle (Enter on the block). Different text style (italic/dimmed). |
| **Complexity** | **MEDIUM.** Extends existing `PlainHistoryCell` pattern. Needs: (1) collapsible `HistoryCell` variant with expand state, (2) subagent stream → parent transcript bridging, (3) height recalculation on toggle. Estimated 300-500 LOC. |

### Option C: Phantom Fork (Separate Log, Summary Only)

| Dimension | Detail |
|-----------|--------|
| **What operator sees** | Parent sees only: `"⏳ agent-name working..."` → `"✓ agent-name: <summary>"`. Full transcript written to `~/.xli/logs/agent-<thread-id>.log`. |
| **Stream attachment** | No TUI stream for subagent. Events written to file via `tracing` or dedicated log sink. Parent sees only begin/end events (already shipped). |
| **Interrupt propagation** | Parent Ctrl+C propagates to all children via `CancellationToken`. No subagent-specific interrupt UX needed. |
| **Context distinction** | Spinner animation on the status line. Log path shown on completion for post-mortem. |
| **Complexity** | **LOW.** Mostly already implemented — today's `CollabWaitingBegin/End` events are this pattern. Would add: (1) file logger for subagent transcript, (2) log path in completion cell. Estimated 100-200 LOC. |

### Option D: Status Line Only (Headless + Final Result) — **STATUS QUO**

| Dimension | Detail |
|-----------|--------|
| **What operator sees** | `"Spawned agent-name"` → `"Waiting for agent-name"` → `"Received from agent-name: <240-char preview>"`. Navigate to full transcript via `/subagents` or Next/Prev keys. |
| **Stream attachment** | Subagent runs on its own tokio submission loop. Parent sees only protocol events. Full transcript visible only after explicit thread switch. |
| **Interrupt propagation** | Parent Ctrl+C interrupts parent turn. Subagent continues unless explicitly closed. Thread switch + Ctrl+C interrupts subagent. |
| **Context distinction** | Thread switch replaces entire viewport. Header shows active thread name. No simultaneous visibility. |
| **Complexity** | **ZERO** (already shipped). Navigation exists via `AgentNavigationState`. |

---

## c) Copilot-Specific Concerns

### TTY Policy (`enforce_tty_policy`, adapter.rs:316-331)

**Critical constraint.** Subagents spawned as tokio tasks within the same process inherit the process's TTY and the `OnceLock<Arc<SessionState>>` — no issue. But:

- If subagents were ever spawned as **separate processes** (e.g., for isolation), `enforce_tty_policy()` would fire on first `stream()` call.
- Gate 3 (`discover_github_token().is_some()`) allows headless if token is cached on disk — this is the **subagent escape hatch**.
- ⚠️ **HOLD constraint**: Any option that changes process topology MUST preserve this escape path or provide an equivalent.

| Option | Needs TTY change? | Risk |
|--------|-------------------|------|
| A (Split) | No — same process, same TTY | None |
| B (Inline) | No — same process | None |
| C (Phantom) | No — same process | None |
| D (Status quo) | No | None |

**All four options are feasible** under current in-process spawning. Risk only materializes if we move to out-of-process subagents.

### Stream Idle Timeout

- Provider default: `DEFAULT_STREAM_IDLE_TIMEOUT_MS = 300_000` (5 min) in `model-provider-info/src/lib.rs:25`.
- **Copilot native override: 1800 s (30 min)** at `core/src/client.rs:773` (raised from 60 s in commit `d027e4af57`). The 60 s ceiling killed legitimate long Opus / fanned-out subagent thinks; 30 min is the "really dead" ceiling now.
- HTTP read timeout (chat fallback): 90 seconds at `copilot/src/adapter.rs:287`.

| Option | Timeout concern |
|--------|----------------|
| A (Split) | Both panes need independent timeout tracking. If subagent stream stalls, its pane shows stale content while parent continues. Need per-pane staleness indicator. |
| B (Inline) | Single transcript — timeout applies to whichever stream is active. Interleaving complicates "which stream timed out?" UX. |
| C (Phantom) | No TUI stream — timeout is backend-only, no UX impact. |
| D (Status quo) | Current behavior — timeout on active thread only. |

### Progressive Streaming Refactor

The streaming pipeline (`tui/src/streaming/`) uses:
- `StreamController` — newline-gated accumulator (`controller.rs:15-80`)
- `MarkdownStreamCollector` — renders deltas to ratatui `Line<'static>` (`markdown_stream.rs:9-107`)
- Adaptive chunking: smooth (1 line/tick) vs catch-up (drain all) (`chunking.rs:79-100`)

| Option | Streaming impact |
|--------|-----------------|
| A (Split) | **Wire-layer change**: Need to demux `ResponseEvent` stream by thread-id and route to correct pane's `StreamController`. Currently single-stream assumption baked into `chatwidget.rs:4194-4195`. |
| B (Inline) | **TUI change only**: Subagent deltas → new `StreamController` → inject cells into parent transcript at correct position. |
| C (Phantom) | **No streaming change.** |
| D (Status quo) | **No change.** |

### Token Sharing / Compact-on-Subagent

- Auth: `Arc<AuthManager>` shared — no token duplication.
- Each `Codex` instance (parent + subagent) has independent `RateLimitSnapshot` tracking (`state/session.rs:27`).
- Compaction (70/30 split in `compact.rs`) runs per-session. Subagent compaction does NOT affect parent context window.
- `MIN_ITEMS_FOR_SPLIT = 20` — short subagent conversations won't trigger compaction.

**No concerns for any option.** Token budget is per-thread, not shared.

### Wire-Layer vs Pure TUI Changes

| Option | Wire changes needed? | Files touched |
|--------|---------------------|---------------|
| A (Split) | **YES** — event demux by thread-id, multi-stream render | `app.rs`, `chatwidget.rs`, `streaming/controller.rs`, new `split_layout.rs` |
| B (Inline) | **No** — extends existing `HistoryCell` pattern | `multi_agents.rs`, `history_cell.rs`, `chatwidget.rs` |
| C (Phantom) | **No** — backend log sink only | New `agent_logger.rs`, minor `multi_agents.rs` |
| D (Status quo) | **No** | None |

---

## d) Recommendation

**Option B (Inline Interleaved)** with a fallback to **Option D** (current thread-switch for full transcript).

**Justification**: Option B is the only choice that gives the operator real-time visibility into subagent work *without* the complexity of split-pane layout management — it extends the existing `PlainHistoryCell` pattern that already ships collapsed `CollabAgentSpawnEnd/WaitingBegin/WaitingEnd` events, adding only a collapsible expand/collapse variant and a secondary `StreamController` bridge. No wire-layer changes needed; it's pure TUI.

### Scope Estimate

| Metric | Estimate |
|--------|----------|
| **LOC** | 300-500 new/modified |
| **Files touched** | 4-5: `multi_agents.rs`, `history_cell.rs`, `chatwidget.rs`, `streaming/controller.rs`, possibly new `collapsible_cell.rs` |
| **Hours** | 8-12 (one sortie, ~2 days) |
| **Risk** | Low — no wire changes, no auth changes, no process topology changes |

### What the sortie would deliver

1. New `CollapsibleHistoryCell` implementing `HistoryCell` trait with expand/collapse state
2. Subagent streaming bridge: secondary `StreamController` that feeds cells into parent transcript
3. Keybinding (Enter on collapsed block) to toggle expansion
4. Height recalculation on expand/collapse
5. Retain existing `/subagents` + Next/Prev navigation as full-transcript escape hatch

### What it would NOT touch

- `enforce_tty_policy()` — no change (HOLD constraint respected)
- `discover_github_token()` — no change
- Wire protocol / `WireApi` enum — no change
- Process topology — subagents remain in-process tokio tasks
- Compaction semantics — unchanged

---

*End of design spike. Read-only recon — no files modified, no commits created.*
