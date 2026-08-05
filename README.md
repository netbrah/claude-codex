<p align="center">
  <h1 align="center">xli</h1>
  <p align="center">A multi-provider Rust CLI agent harness with native wire protocol fidelity.</p>
  <p align="center">
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Rust-2024%20edition-dea584" alt="Rust 2024" /></a>
    <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="License: Apache-2.0" /></a>
    <img src="https://img.shields.io/badge/crates-80+-orange" alt="80+ crates" />
  </p>
</p>

---

## Why This Exists

Most CLI coding agents are either provider-locked (Claude Code, Cursor), proxy-dependent (they route everything through an OpenAI-compatible shim), or opaque about wire behavior. You can't inspect what's actually sent to the model, and you can't extend the protocol without reverse-engineering a closed binary.

**xli** is a from-scratch Rust reimplementation of the agent runtime that speaks each provider's **native wire protocol** — Anthropic `/v1/messages`, OpenAI `/responses`, and Gemini `generateContent` — with no translation layer in between. The wire your model receives is the wire you configured. Every field, every header, every SSE event is first-party.

Built on the [OpenAI codex-rs](https://github.com/openai/codex) workspace (Apache-2.0), with the provider abstraction, Anthropic wire, Gemini wire, tool orchestration, context management, and config system rebuilt for multi-provider fidelity.

---

## Architecture

```text
┌─────────────────────────────────────────────────────┐
│  CLI (cli/)  ·  TUI (tui/)  ·  App Server (app-server/)│
└──────────────────────┬──────────────────────────────┘
                       │
              ┌────────▼────────┐
              │  Session Runtime │  (core/)
              │  · turn loop     │
              │  · compaction    │
              │  · tool dispatch │
              └────────┬────────┘
                       │
         ┌─────────────▼──────────────┐
         │  Provider Abstraction      │  (codex-api/)
         │  · wire selection          │
         │  · streaming SSE           │
         │  · history translation     │
         └──┬──────────┬──────────┬──┘
            │          │          │
   ┌────────▼──┐ ┌────▼─────┐ ┌─▼────────┐
   │ Anthropic │ │  OpenAI  │ │  Gemini   │
   │ /messages │ │/responses│ │generateCt │
   │  SSE      │ │   SSE    │ │   SSE     │
   └───────────┘ └──────────┘ └──────────┘

  Models (models-manager/)    Config (config/)     Tools (tools/)
  · registry & caps           · TOML profiles      · apply_patch
  · LiteLLM enrichment        · per-provider keys  · grep / find
  · context window            · env_http_headers   · shell exec
  · max output tokens          · sandbox policy     · MCP client
```

### Crate Highlights

| Crate | Role |
|---|---|
| `core/` | Session runtime — turn loop, tool dispatch, history compaction, streaming aggregation |
| `codex-api/` | Wire protocol layer — `/messages` and `/responses` endpoints, SSE parsers, request builders |
| `protocol/` | Shared types — `ModelInfo`, `ProviderCaps`, message history, config types |
| `models-manager/` | Model registry — slug resolution, provider caps, LiteLLM enrichment, context window cascade |
| `config/` | TOML config schema — profiles, overrides, per-provider auth, sandbox policy, MCP config |
| `tools/` | Tool registry — apply_patch, grep, find_files, shell exec, dynamic tools, MCP bridge |
| `sandboxing/` | Process isolation — Linux seccomp, macOS seatbelt, Windows sandbox |
| `execpolicy/` | Execution policy DSL — declarative command allow/deny rules |
| `tui/` | Terminal UI — streaming output, context gauge, approval prompts |
| `mcp-server/` | MCP (Model Context Protocol) server — exposes tools to external MCP clients |
| `rmcp-client/` | MCP client — connects to external MCP servers as tool sources |

---

## Feature Matrix

| Capability | Anthropic `/messages` | OpenAI `/responses` | Gemini |
|---|:---:|:---:|:---:|
| Streaming (SSE) | ✓ | ✓ | ✓ |
| Tool calls | ✓ | ✓ | ✓ |
| Reasoning / thinking blocks | ✓ | ✓ | ✓ |
| Prompt caching (`cache_control`) | ✓ | — | — |
| Multi-turn history translation | ✓ | — | ✓ |
| Configurable profiles | ✓ | ✓ | ✓ |
| Context window management | ✓ | ✓ | ✓ |
| Auto-compaction | ✓ | ✓ | ✓ |

---

## Quick Start

### Build

```bash
git clone https://github.com/netbrah/claude-codex.git
cd claude-codex/codex-rs
cargo build --release
# Binary: target/release/codex
```

### Configure

xli reads `~/.codex/config.toml` (or `$CODEX_HOME/config.toml`). Example configs are in [`examples/`](examples/):

```bash
# Copy an example and edit
cp examples/anthropic.toml ~/.codex/config.toml
export ANTHROPIC_API_KEY="sk-ant-..."
```

<details>
<summary><strong>Anthropic example</strong></summary>

```toml
model = "claude-sonnet-4-20250514"
model_provider = "anthropic"

[model_providers.anthropic]
name = "Anthropic"
base_url = "https://api.anthropic.com/v1"
wire_api = "messages"
env_http_headers = { "x-api-key" = "ANTHROPIC_API_KEY" }

[model_providers.anthropic.http_headers]
"anthropic-version" = "2023-06-01"
```
</details>

<details>
<summary><strong>Gemini example</strong></summary>

```toml
model = "gemini-2.5-pro"
model_provider = "gemini"

[model_providers.gemini]
name = "Google Gemini"
base_url = "https://generativelanguage.googleapis.com/v1beta"
wire_api = "gemini"
env_http_headers = { "x-goog-api-key" = "GEMINI_API_KEY" }
```
</details>

<details>
<summary><strong>Multi-provider with profiles</strong></summary>

```toml
# Default profile
model = "claude-sonnet-4-20250514"
model_provider = "anthropic"

[model_providers.anthropic]
# ... as above ...

[model_providers.gemini]
# ... as above ...

# Switch with: codex -p opus
[profiles.opus]
model = "claude-opus-4-20250514"
model_provider = "anthropic"

# Switch with: codex -p gemini-pro
[profiles.gemini-pro]
model = "gemini-2.5-pro"
model_provider = "gemini"
```
</details>

See [`examples/README.md`](examples/README.md) for full configuration details including environment variables, profile switching, and auth notes.

### Run

```bash
# Interactive TUI
cargo run --bin codex

# One-shot
cargo run --bin codex exec "Explain the provider abstraction in codex-api/"

# With a profile
cargo run --bin codex -- -p opus
```

---

## Engineering Highlights

### Provider-Native Wire Fidelity

Each provider speaks its own protocol — no OpenAI-compatible shim. The Anthropic `/messages` implementation ([`codex-api/src/endpoint/messages.rs`](codex-rs/codex-api/src/endpoint/messages.rs), [`codex-api/src/sse/messages.rs`](codex-rs/codex-api/src/sse/messages.rs)) handles:

- Native `system` / `user` / `assistant` role mapping (not the OpenAI `developer` role)
- `thinking` content blocks with `signature` redaction
- `cache_control` breakpoints for prompt caching (5-minute ephemeral and 1-hour extended TTLs)
- Stop reasons mapped to a unified `ResponseEvent` enum shared with the OpenAI wire

The OpenAI `/responses` wire is inherited from the upstream codex-rs codebase and extended for multi-provider routing.

### Context Window Management

The model resolution cascade resolves context windows through five stages:

```text
bundled models.json → LiteLLM live enrichment → hardcoded family fallback
  → user config override (clamped downward) → 95% effective haircut
```

Auto-compaction triggers at 90% of the effective window. The TUI context gauge and compaction threshold share the same base — what you see is what the compactor thresholds on. See [`models-manager/src/model_info.rs`](codex-rs/models-manager/src/model_info.rs) and [`protocol/src/openai_models.rs`](codex-rs/protocol/src/openai_models.rs).

### Tool Orchestration

Tools are registered through a typed registry ([`core/src/tools/`](codex-rs/core/src/tools/)) with:

- Parallel tool dispatch with configurable concurrency
- Per-tool output token budgets
- Sandbox integration — tools that execute commands go through the execution policy DSL ([`execpolicy/`](codex-rs/execpolicy/))
- Dynamic tool injection via MCP ([`rmcp-client/`](codex-rs/rmcp-client/))

### Sandbox & Execution Policy

Platform-native process isolation:

- **Linux**: seccomp filter via `linux-sandbox/`
- **macOS**: sandbox-exec (seatbelt) profiles
- **Windows**: job object sandbox via `windows-sandbox-rs/`

The execution policy ([`execpolicy/`](codex-rs/execpolicy/)) is a declarative DSL for command allow/deny rules — no shell parsing, just pattern matching on executable names and argument structures.

### Config System

TOML-based with profile support, per-provider auth, and layered overrides:

- `~/.codex/config.toml` — user defaults
- `.codex/config.toml` — project-level overrides
- `-c key=value` — CLI overrides
- `-p <profile>` — named profile switch

Schema defined in [`config/src/types.rs`](codex-rs/config/src/types.rs) with validation in [`config/src/schema.rs`](codex-rs/config/src/schema.rs).

---

## Testing

```bash
# Full workspace
cargo test --workspace

# Provider and protocol crates
cargo test -p codex-protocol -p codex-models-manager -p codex-core

# Wire protocol conformance
cargo test -p codex-api
```

---

## Project Structure

```text
claude-codex/
├── codex-rs/                # Rust workspace (80+ crates)
│   ├── core/                # Session runtime, tool dispatch, compaction
│   ├── codex-api/           # Wire protocols (/messages, /responses)
│   │   ├── src/endpoint/    # HTTP request builders
│   │   └── src/sse/         # Streaming event parsers
│   ├── protocol/            # Shared types, model info, config types
│   ├── models-manager/      # Model registry, provider caps, LiteLLM
│   ├── config/              # TOML config schema, profiles, overrides
│   ├── tools/               # Tool registry and handlers
│   ├── sandboxing/          # Platform-native process isolation
│   ├── execpolicy/          # Execution policy DSL
│   ├── tui/                 # Terminal UI
│   ├── cli/                 # CLI entry point
│   ├── mcp-server/          # MCP server (expose tools)
│   ├── rmcp-client/         # MCP client (consume external tools)
│   └── ...
├── examples/                # Example configs (Anthropic, Gemini, multi-provider)
└── LICENSE                  # Apache-2.0
```

---

## Known Limitations

- **Anthropic and Gemini are not built-in providers** — they require explicit `[model_providers.*]` config. Built-in presets (OpenAI, Bedrock, Ollama, LM Studio) ship by default.
- **Auth header style** — Anthropic requires `x-api-key` and Gemini requires `x-goog-api-key`, but the built-in `env_key` path emits `Authorization: Bearer`. Use `env_http_headers` in config until native header support lands.
- **Gemini `?key=` query param** produces a malformed URL when combined with `?alt=sse` for streaming — use the `x-goog-api-key` header style instead.
- **Telemetry** defaults to on in release builds (inherited from upstream). Set `[otel] metrics_exporter = "none"` in your config to disable.

---

## Roadmap

- [ ] Built-in `anthropic` and `gemini` provider presets with native auth headers
- [ ] Native `x-api-key` / `x-goog-api-key` emission via `env_key`
- [ ] Fix Gemini `?key=` query param URL construction
- [ ] Telemetry opt-out by default in release builds
- [ ] Provider conformance test suite
- [ ] Binary releases for macOS / Linux

---

## Attribution

Built on [OpenAI's codex-rs](https://github.com/openai/codex) (Apache-2.0). The provider abstraction, Anthropic `/messages` wire, Gemini wire, context window management, and multi-provider config system are original work.

## License

[Apache-2.0](LICENSE)
