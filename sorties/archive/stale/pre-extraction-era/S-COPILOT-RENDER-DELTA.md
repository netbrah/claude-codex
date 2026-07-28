# S-COPILOT-RENDER-DELTA — GPT vs Opus UI rendering parity on the Copilot wire

**Track:** 1 (HTTP wire) · **Status:** ACTIVE — Wave 0 recon · **Filed:** 2026-04-19 · **Promoted:** 2026-04-20 (duck strategic pass)

## Symptom

Field-observed UI rendering delta between models served over the Copilot wire:
- `gpt-5*` requests route through `CopilotWire::Responses` (`/responses`)
- `claude-*` (Opus / Sonnet) requests route through `CopilotWire::Messages` (`/v1/messages`)

The two paths render differently in the TUI. Hypothesis: SSE event → internal
event mapping diverges between the two arms, producing different text-delta /
reasoning / tool-use cadence even when the model output is semantically
equivalent.

## Suspected blast radius

Field-observed delta is on the **native** wires, so the suspect surface is
`codex-api` event translation + the shared `core` mapping, **not** the
chat-completions fallback in `copilot/src/mapping.rs`:

- `codex-api/src/sse/messages.rs` — Anthropic `/v1/messages` SSE parser
  (`content_block_*`, `message_delta` → `ResponseEvent`). Feeds Opus.
- `codex-api/src/sse/responses.rs` — OpenAI `/responses` SSE parser. Feeds
  GPT-5.x.
- `core/src/client.rs::map_response_stream` (lines `2063-2146`) — the shared
  post-translation pump that both arms feed into. If the divergence is in
  cadence/shape of `OutputItemDone` / `Completed` events, this is the
  funnel where it's most observable.
- The two caller paths that produce events for `map_response_stream`:
  - `stream_responses_api` arm: `core/src/client.rs:1341` (Responses entry)
  - `stream_messages_api` arm: `core/src/client.rs:1387, 1575` (Messages entry)
- TUI consumer — second-order: confirm whether the TUI is rendering identical
  `ResponseEvent` sequences differently, or whether the upstream events are
  already different.

The 3-wire router itself (`copilot/src/endpoints.rs::route_for_model`) is the
bifurcation point but not the bug. `copilot/src/mapping.rs` is the
chat-completions fallback path and is **not in scope** for this brief — both
GPT and Opus on Copilot go through native arms.

## Anchor commits (where this surface was last touched)

- `378680dba3` — native `/v1/messages` + `/responses` routes added.
- `086af1ed85` — static wire route table.
- `96205a1b2b` — progressive streaming on the legacy chat wire (precedent for
  the kind of cadence work this sortie may need on the native arms).

## Recon checklist (do this first, before greenlight)

1. Capture two real fixtures with `CODEX_COPILOT_LIVE=1`:
   - `gpt-5*` → `/responses` raw SSE bytes
   - `claude-opus-*` → `/v1/messages` raw SSE bytes
   for a prompt that produces (a) plain text, (b) reasoning, (c) a tool call.
2. Walk both fixtures through the appropriate `codex-api` SSE parser and the
   matching `map_response_stream` arm in `core/src/client.rs`; dump the
   resulting `ResponseEvent` sequence side-by-side.
3. Identify the first divergence in cadence/shape that the TUI would render
   differently (per-chunk vs per-line, reasoning frames, tool-call delta
   ordering, finish_reason placement).
4. Decide: fix mapping symmetrically, or normalise at the consumer (TUI).

## Out of scope

- `copilot/src/mapping.rs` and the rest of the chat-completions fallback —
  Copilot now routes both GPT and Opus through native wires.
- Token / auth shape — F3 (`2a33156645`) just landed.

## Promotion

Promote from PARKED to active when:
- recon fixtures + side-by-side analysis are attached, and
- a specific delta in the SSE parser (`codex-api/src/sse/{messages,responses}.rs`)
  or in the shared `map_response_stream` funnel (`core/src/client.rs:2063-2146`)
  is identified — with the divergent `ResponseEvent` field named explicitly.

Until then this brief is intentionally light — no premature implementation.
