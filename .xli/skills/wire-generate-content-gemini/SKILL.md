---
name: wire-generate-content-gemini
description: Bootstrap skill for the Gemini generateContent wire protocol. Use when beginning or working on the Google Gemini / Vertex AI adapter. Pattern-mirrors the shipped provider-anthropic crate as a template.
allowed-tools: Read, Edit, Grep, Bash(cargo test -p codex-provider-gemini), Bash(cargo test -p codex-core -p codex-api), Bash(cargo check --workspace)
---

# Wire: generateContent (Gemini)

## When to load
When beginning or working on the Gemini / Vertex AI wire protocol adapter.

## Status
**Planning.** Sortie spec at `sorties/gemini-wire/SORTIE-SPEC.md`.
Implementation template: `codex-rs/provider-anthropic/` (shipped Sortie 1).

## Architecture (post-Sortie-1)

The Gemini adapter is a **leaf provider crate** with zero `codex-core` deps.
It implements the `ModelProvider` trait from `codex-model-provider` and plugs
into the dispatch via `codex-provider-registry`.

```
provider-gemini/          ← NEW leaf crate
├── src/
│   ├── lib.rs            ← GeminiModelProvider + re-exports
│   ├── provider.rs       ← ModelProvider trait impl (stream, auth, info)
│   ├── request.rs        ← Prompt → GenerateContentRequest mapping
│   ├── wire.rs           ← SSE → ResponseEvent mapping + tests
│   ├── auth.rs           ← API key + Vertex AI service account JWT
│   └── model.rs          ← Gemini model family metadata + context windows
└── Cargo.toml

codex-api/src/sse/
└── generate_content.rs   ← spawn_generate_content_stream() SSE parser
```

### Trait surface to implement

From `codex-model-provider/src/provider.rs`:

```rust
#[async_trait]
impl ModelProvider for GeminiModelProvider {
    fn info(&self) -> &ModelProviderInfo;
    fn auth_manager(&self) -> Option<Arc<AuthManager>>;
    async fn auth(&self) -> Option<CodexAuth>;
    fn account_state(&self) -> ProviderAccountResult;
    fn models_manager(&self, codex_home, catalog) -> SharedModelsManager;
    fn effective_wire_api(&self, model_slug: &str) -> WireApi;

    // The main entry point — stream a turn
    async fn stream(&self, req: ProviderStreamRequest<'_>)
        -> Result<codex_prompt::ResponseStream>;
}
```

From `codex-model-provider/src/stream.rs`:

```rust
#[async_trait]
trait MessagesBackend: Send + Sync {
    async fn execute_messages_turn(
        &self,
        request: MessagesApiRequest,  // Gemini needs its own request type
        extra_headers: HeaderMap,
    ) -> Result<codex_prompt::ResponseStream>;
}
```

**Key decision**: Gemini's request shape is different enough from Anthropic
that `MessagesApiRequest` won't work. Options:
1. Bypass `MessagesBackend` entirely — implement `stream()` directly
   (simpler, recommended for Phase 1)
2. Add a `GeminiBackend` trait parallel to `MessagesBackend`

Recommendation: **option 1** for Phase 1. The `stream()` method on
`ModelProvider` is the only contract that matters.

### WireApi variant

Add to `codex-rs/model-provider-info/src/lib.rs`:
```rust
pub enum WireApi {
    Responses,
    Messages,
    Copilot,
    GenerateContent,  // ← NEW
}
```

### Provider registry dispatch

In `codex-rs/provider-registry/src/lib.rs`:
```rust
pub fn create_model_provider(info, auth_manager) -> SharedModelProvider {
    if info.wire_api == WireApi::GenerateContent {
        return Arc::new(GeminiModelProvider::new(info, auth_manager));
    }
    // ... existing dispatch
}
```

## Wire Protocol Quick Reference

### Streaming endpoint
```
POST /v1beta/models/{model}:streamGenerateContent?alt=sse
```

### SSE chunk shape
```json
{"candidates":[{"content":{"parts":[{"text":"delta"}],"role":"model"}}],
 "usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5}}
```

### Tool call shape
```json
{"candidates":[{"content":{"parts":[
  {"functionCall":{"name":"shell","args":{"command":["ls"]}}}
],"role":"model"},"finishReason":"STOP"}]}
```

### Tool result (sent back in next contents[])
```json
{"role":"user","parts":[
  {"functionResponse":{"name":"shell","response":{"content":"file1.txt\n"}}}
]}
```

## ResponseEvent Mapping Rules

| Gemini shape | ResponseEvent |
|-------------|---------------|
| `parts[].text` | `OutputTextDelta(text)` |
| `parts[].functionCall` | `ToolCallInputDelta` → `OutputItemDone(FunctionCall)` |
| `parts[].thought: true` | `ReasoningContentDelta` (Gemini 2.5+) |
| `finishReason: "STOP"` | `Completed { stop_reason: Some("end_turn"), end_turn: Some(true) }` |
| `finishReason: "MAX_TOKENS"` | `Completed { stop_reason: Some("max_tokens"), end_turn: None }` |
| `usageMetadata` | `TokenUsage { input_tokens, output_tokens, ... }` |

## Anti-patterns
- Leaking `GenerateContentResponse` past the adapter boundary
- Hardcoding `generativelanguage.googleapis.com` — use configurable base URL
- Skipping the `ResponseItem[]` mapping
- Using `MessagesApiRequest` — Gemini has its own request shape
- Importing `codex-core` from the provider crate

## Related skills
- `response-item-lingua-franca` — the ResponseItem[] contract
- `wire-messages-anthropic` — the shipped /messages implementation (template)
- `rust-workspace-hygiene` — Cargo.toml, feature flags, deny.toml
- `sortie-branch-discipline` — branching and commit conventions
