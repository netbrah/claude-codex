# sorties/deferred/

**Parking lot for viable-but-not-now briefs.** The active sortie board
(`../SORTIE-BOARD.md`) is currently scoped to the Copilot wire (Track 1 HTTP +
Track 2 CLI host + OP2 hardening). Briefs here are real candidates that don't
sit on those tracks today; they remain firable when their scope re-enters
the active mission.

**Stale/broken briefs do not live here** — they go to `../archive/stale/`.
The distinction matters: `deferred/` is a queue, `archive/stale/` is a graveyard.

Move a brief back to `../` (and add a row to `SORTIE-BOARD.md`) when its scope
re-enters the active mission.

## Contents

### Standalone `/messages` wire enhancements (no shared substrate touch)

These target the standalone `/messages` integration without intersecting the
shared substrate that Copilot Claude actually uses today. Briefs that DO
touch the shared substrate (`S-003`, `S-004`, `S-005`, `S-CACHE-TTL`) are
listed under §1 of `SORTIE-BOARD.md` as "Track 1 shared `/messages`
substrate" and remain firable from this folder.

| File | Was | Why deferred |
|---|---|---|
| `S-003-sse-text-delta-gating.md` | Correctness fix on `codex-api/src/sse/messages.rs` | **Cross-listed in §1 shared substrate.** Reactivate when SSE delta cadence becomes the next bottleneck. |
| `S-004-fix-rate-limit-retryable.md` | 429 retryable mapping for native `/messages` | **Cross-listed in §1 shared substrate.** Reactivate when a 429 is observed on Copilot Claude. |
| `S-005-fix-output-to-text-images.md` | Image preservation in `messages_wire.rs::output_to_text` | **Cross-listed in §1 shared substrate.** Reactivates when `FRAG-COPILOT-F` (vision passthrough) fires. |
| `S-CACHE-TTL.md` | `cache_control.ttl: "1h"` on `/messages` | **Cross-listed in §1 shared substrate.** Bundle with `S-MSG-CACHE-PARITY` if both fire together. |
| `S-016-stop-sequences.md` | Wire `stop_sequences` from config | No current driver |
| `S-SERVER-TOOLS-parse.md` | Parse `server_tool_use` blocks | No current driver |
| `S-MEMORY-tool.md` | Wire `memory_20250818` tool | Blocked on `S-SERVER-TOOLS-parse` |

### Apex → XLI ports

TypeScript-to-Rust cross-pollinations from the Apex harness. None of these
touch the Copilot wire directly.

| File | Brief |
|---|---|
| `PORT-LOOP-detection.md` | 5-repetition tool-loop breaker |
| `PORT-OMIT-omission-detector.md` | `// ... existing code ...` placeholder detector |
| `PORT-TRIM-context-budget.md` | Pre-send context budget trim |
| `PORT-MASK-tool-output-masking.md` | Mask tool outputs > 50K tokens |
| `PORT-PARALLEL-read-only-tools.md` | Parallel-safe read-only tool execution |

### Paired with active hardening

_(none — `OP2-subagent-ux-spike.md` was promoted back to the active board on
2026-04-20 after F4 landed in `79cdb3fc33`.)_

## Stale (moved out of `deferred/`)

`XLI-70-30-SPLIT-IMPLEMENTATION.md` was previously here. It has been moved
to `../archive/stale/` because the symbols it references
(`PRESERVE_FRACTION`, `find_compact_split_point`,
`find_next_user_message_boundary`, `is_tool_response_content`) do not exist
in `compact.rs` at HEAD. It needs a full rewrite — not a re-fire — before
it can re-enter `deferred/` or active.

## Rules for this folder

1. **Do not link these from `SORTIE-BOARD.md` as active work** — they are
   parked, not queued. Cross-listing under §1 shared substrate is allowed
   and explicit.
2. **Freshness-only edits are allowed** while parked (e.g., correcting a
   line number, fixing a path, updating a stream-idle value). Do not
   restructure or extend scope without re-activating the brief first.
3. **If a brief becomes broken** (symbols don't exist, file moved, premise
   pre-empted), move it to `../archive/stale/` — do not rot it in place.
4. **To re-activate:** `git mv` the file back to `sorties/`, add a row to the
   correct `§` of `SORTIE-BOARD.md` with a fresh status line.
