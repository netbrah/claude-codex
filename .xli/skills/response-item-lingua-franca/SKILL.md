---
name: response-item-lingua-franca
description: Use when working on any cross-wire mapping or touching the ResponseItem[] enum contract. Owns the invariant that adapters never leak wire-specific shapes inward and that all wire protocols converge to the same internal representation. Knows exact variant lists and where types are defined.
allowed-tools: Read, Edit, Grep, Bash(cargo test -p codex-core -p codex-api)
---

# ResponseItem[] lingua franca

## When to load
Any diff that touches cross-wire mapping, adds a new `ResponseItem` variant,
or modifies how wire-specific types are converted to the internal representation.

## Type locations

| Type | Crate | File |
|------|-------|------|
| `ResponseItem` | `protocol` | `codex-rs/protocol/src/models.rs` |
| `ResponseEvent` | `codex-api` | `codex-rs/codex-api/src/common.rs` |
| `WireApi` | `model-provider-info` | `codex-rs/model-provider-info/src/lib.rs` |
| `Prompt` | `core` | `codex-rs/core/src/client_common.rs` |

## ResponseItem variants (`protocol/src/models.rs`)

Tagged enum (`#[serde(tag = "type")]`):

| Variant | Key fields |
|---------|-----------|
| `Message` | `role`, `content: Vec<ContentItem>`, `phase: Option<MessagePhase>` |
| `Reasoning` | `summary`, `content`, `encrypted_content`, `raw_wire_block` |
| `LocalShellCall` | `call_id`, `status: LocalShellStatus`, `action: LocalShellAction` |
| `FunctionCall` | `name`, `arguments: String`, `call_id`, `namespace` |
| `ToolSearchCall` | `call_id`, `execution`, `arguments: Value` |
| `FunctionCallOutput` | `call_id`, `output: FunctionCallOutputPayload` |
| `CustomToolCall` | `call_id`, `name`, `input: String` |
| `CustomToolCallOutput` | `call_id`, `output: FunctionCallOutputPayload` |
| `ToolSearchOutput` | `call_id`, `status`, `execution`, `tools` |
| `WebSearchCall` | `id`, `status`, `action: Option<WebSearchAction>` |
| `ImageGenerationCall` | `id`, `status`, `revised_prompt`, `result` |
| `GhostSnapshot` | `ghost_commit: GhostCommit` |
| `Compaction` | `encrypted_content: String` |
| `Other` | `#[serde(other)]` catch-all |

## ResponseEvent variants (`codex-api/src/common.rs`)

| Variant | Purpose |
|---------|---------|
| `Created` | Stream started |
| `OutputItemDone(ResponseItem)` | Complete item emitted |
| `OutputItemAdded(ResponseItem)` | Item added (partial) |
| `ServerModel(String)` | Server-picked model |
| `ServerReasoningIncluded(bool)` | Reasoning flag |
| `Completed { stop_reason, response_id, token_usage }` | Turn finished |
| `OutputTextDelta(String)` | Streaming text chunk |
| `ReasoningSummaryDelta { delta, summary_index }` | Streaming reasoning summary |
| `ReasoningContentDelta { delta, content_index }` | Streaming reasoning content |
| `ReasoningSummaryPartAdded { summary_index }` | New reasoning summary part |
| `RateLimits(RateLimitSnapshot)` | Rate limit info |
| `ModelsEtag(String)` | Models etag |

## WireApi variants (`model-provider-info/src/lib.rs`)

```rust
pub enum WireApi {
    Responses,   // OpenAI /v1/responses (default)
    Messages,    // Anthropic /v1/messages
    Copilot,     // GitHub Copilot (routes via CopilotWire subtype)
}
```

## Wire dispatch — `core/src/client.rs`

`ModelClientSession::stream()` dispatches based on `effective_wire_api()`:

| Wire | Method | Adapter location |
|------|--------|-----------------|
| `Responses` | `stream_responses_api()` | `core/src/client.rs` |
| `Messages` | `stream_messages_api()` | `core/src/client.rs` |
| `Copilot` | `stream_copilot_api()` | delegates to `codex-rs/copilot/` crate |

`effective_wire_api()` adds dynamic routing:
- `Messages` + non-Anthropic model → auto-upgrades to `Responses`
- `Copilot` → consults `route_for_model()` → may route via `CopilotWire::{Messages,Responses,ChatCompletions}`

## Turn loop consumption — `core/src/codex.rs`

`run_turn()` at ~L6086 consumes `ResponseEvent` via a match at ~L7798.
The turn loop operates exclusively on `ResponseEvent` / `ResponseItem[]` —
it never inspects which wire produced them.

## The contract
1. All wire adapters produce identical `ResponseItem[]` for semantically
   equivalent responses.
2. Adding a new `ResponseItem` variant requires updating ALL wire adapters.
3. Core code never branches on `WireApi` to interpret `ResponseItem[]`.
4. Each wire adapter must have tests proving round-trip equivalence for
   text output + tool calls.

## Prompt struct (`core/src/client_common.rs`)
```rust
pub struct Prompt {
    pub input: Vec<ResponseItem>,
    pub tools: Vec<ToolSpec>,
    pub parallel_tool_calls: bool,
    pub base_instructions: BaseInstructions,
    pub personality: Option<Personality>,
    pub output_schema: Option<Value>,
}
```

## Anti-patterns
- A wire adapter returning raw wire types past its boundary.
- Adding a `ResponseItem` variant without updating all adapters.
- Core code branching on `WireApi` to interpret items differently.
- Leaking `CopilotResponseEvent`, `ContentBlock`, or Gemini types into core.
