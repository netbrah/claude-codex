# PR — `feat/codex-copilot-integration` → `dev`

> Paste this file verbatim into the PR body on `netbrah/claude-codex`.
> Target branch is **`dev`** (not `main`). Do **not** squash-merge — the
> four commits are intentionally atomic and the design trail matters for
> future bisects.

---

## Summary

Adds GitHub Copilot as a first-class wire API (`WireApi::Copilot`) by
bridging to the frozen `codex-copilot` crate from
[`netbrah/codex-agent@26ff8f1d`][upstream] through a thin in-tree adapter
crate (`codex-rs/copilot/`, crate name `codex-copilot-adapter`). The
66-test upstream baseline stays authoritative upstream — this branch does
**not** vendor or re-test that layer.

[upstream]: https://github.com/netbrah/codex-agent/commit/26ff8f1d4967360960d0886aee8462bc56b558ff

**Credit:** design + implementation trail lives in
[`netbrah/copilot-codex`](https://github.com/netbrah/copilot-codex) under
`docs/integration/`. Read the five-file trail (`00-seed-prompt.md`
through `05-handoff.md`) for full context.

---

## What changes

### New crate: `codex-rs/copilot/` (`codex-copilot-adapter`)

- `src/mapping.rs` — `Mapper` walks `codex_copilot::ResponseEvent` and
  emits `codex_api::ResponseEvent`. Tool-call frames emit a matched
  `OutputItemAdded` + `OutputItemDone` pair so the claude-codex turn
  loop sees a complete committable function call. `Done` resolves to
  `Completed { stop_reason: "end_turn" | "tool_use", ... }` based on
  whether any tool call was seen during the walk.
- `src/request.rs` — `items_to_chat_messages` flattens
  `Vec<ResponseItem>` into Copilot's minimal `ChatMessage { role,
  content }` shape. Tool calls and outputs ride inline as tagged text
  (upstream's struct has no `tool_calls` / `tool_call_id` fields and is
  frozen per invariants 1/4/5). Image-only messages are dropped; vision
  is out of scope for v1.
- `src/adapter.rs` — public `stream(input, model, tools)` entry point;
  also a `stream_inner(http, auth, input, model)` test seam. Holds
  process-scoped cached `CopilotAuth` + `CopilotHttpClient` in an
  `OnceLock<Arc<SessionState>>` so `slow_down` +5s back-off arithmetic
  persists across turns (invariant 9). TTY guard via
  `codex_copilot::is_interactive_tty` fires here, not on
  `ModelClient::new` (invariant 3). `print_tos_banner` is gated by an
  `AtomicBool` for at-most-once behavior.

### Existing crates touched

- `codex-rs/Cargo.toml` — add `copilot` to workspace members and a path
  dep for `codex-copilot-adapter`.
- `codex-rs/model-provider-info/src/lib.rs` — add `WireApi::Copilot`
  enum variant + serde alias. 5 unit tests cover round-tripping and
  round-trip through `ModelProviderInfo`.
- `codex-rs/core/src/client.rs` — dispatch arm for `WireApi::Copilot`
  routes to the adapter. `effective_wire_api` does **not**
  auto-upgrade Copilot to anything else (config errors surface at
  request time rather than as silent transport swaps).
- `codex-rs/core/Cargo.toml` — add workspace dep on
  `codex-copilot-adapter`.

### Adapter tests (new)

- `copilot/src/mapping.rs` — 4 unit tests.
- `copilot/src/request.rs` — 3 unit tests (inline assistant tag; tool
  role mapping; image-only drop).
- `copilot/src/adapter.rs` — 2 unit tests (max-tokens default; synth
  response id uniqueness + shape).
- `copilot/tests/stream_happy_path.rs` — 2 wiremock integ tests (content
  → `Completed{end_turn}`; content + tool call → `Completed{tool_use}`).
- `copilot/tests/stream_401_retry.rs` — 1 wiremock integ test that
  exercises invariant 10 end-to-end through the adapter.

**Net new test count:** 12. Zero existing tests migrated or modified.

---

## What does not change

- **`netbrah/codex-agent@26ff8f1d` is not edited.** The adapter pulls
  it in via `git + rev = 26ff8f1d4967360960d0886aee8462bc56b558ff` in
  `copilot/Cargo.toml`. The 66-test upstream baseline stays the
  authoritative Copilot signal (invariant 1).
- **No changes to Responses / Messages wire code paths.** The
  `effective_wire_api` auto-upgrade logic for Messages → Responses on
  non-Anthropic models is untouched.
- **No changes to the turn loop in `core/src/codex.rs`.** The adapter
  hands back a standard `ResponseStream`; everything downstream is
  identical to the Responses/Messages arms.

---

## Invariants preserved (all 10)

See `docs/integration/02-design.md` §8 and `03-validation.md` §1.3 in
`netbrah/copilot-codex` for the full matrix. Highlights:

- **#2 (no anyhow):** adapter uses `thiserror` only; upstream untouched.
- **#3 (TTY guard on first stream):** `adapter::stream` is the only
  entry that probes TTY; `ModelClient::new` is oblivious to Copilot.
- **#9 (slow_down across turns):** `SessionState` caches
  `CopilotHttpClient` in an `OnceLock`.
- **#10 (401 retry owned by upstream):** adapter calls
  `chat_stream_with_auth`, never `chat_stream_raw`.

---

## How to test locally

```bash
# Format + lint.
cargo fmt --check -p codex-copilot-adapter
cargo clippy -p codex-copilot-adapter --all-targets -- -D warnings

# Targeted test runs (never workspace-root).
cargo nextest run -p codex-copilot-adapter --no-fail-fast
cargo nextest run -p codex-model-provider-info --no-fail-fast
cargo nextest run -p codex-core --no-fail-fast

# Full workspace compile gate.
cargo check --workspace
```

Full command table is in
[`03-validation.md`](https://github.com/netbrah/copilot-codex/blob/main/docs/integration/03-validation.md)
§2.

> **Note.** The authoring environment had no `cargo` / `rustc` — all
> compile and test signals are deferred to the reviewer's local
> checkout. If anything fails compile, the top suspects are: (a) a stale
> `codex-tools` feature set, (b) the `Arc` import in `adapter.rs`
> (remove if unused), or (c) a `tokio::sync::Mutex` deref mismatch at
> `&mut *auth_guard`. All three are mechanical fixes.

---

## Known gaps (tracked in `03-validation.md` §3)

1. No incremental streaming — `chat_stream_with_auth` materializes the
   whole response first. Documented trade for v1.
2. Tool schemas are not forwarded (upstream has no `tools` param).
   Function-call tool semantics ride inline in text.
3. `session_telemetry`, `sampling`, `turn_metadata_header` are dropped.
4. `response_id` is process-local (atomic counter, 16 hex chars).
5. No vision input; image content items are dropped.

---

## Commits (4)

| SHA (short) | Title |
| --- | --- |
| `4b1d3f6ca` | copilot: scaffold adapter crate (no wiring) |
| `400f4bd1e` | copilot: add WireApi::Copilot variant and core dispatch arm |
| `2fc566a0b` | copilot: wire Mapper, request builder, and real adapter stream |
| `608ca19dd` | copilot: add wiremock integration tests for adapter stream |

---

## Checklist for the maintainer

- [ ] Local `cargo check --workspace` passes.
- [ ] `cargo nextest run -p codex-copilot-adapter` passes (12 tests).
- [ ] `cargo nextest run -p codex-model-provider-info` stays green.
- [ ] `cargo nextest run -p codex-core` stays green.
- [ ] The upstream 66-test baseline on `codex-agent@26ff8f1d` is still
      green in your own checkout (sanity only — not gated here).
- [ ] Reviewed `docs/integration/02-design.md` §4.4 (stream model) and
      agree with the `Vec<ResponseEvent>` materialization trade.
- [ ] Branch is merged with **no squash** (retain four-commit history).

Any red on the first four gates blocks merge.
