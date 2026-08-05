# 01 — Recon: `netbrah/claude-codex`

**Branch of record:** `dev` (default).
**Commit sampled:** `63c18a30e` (tip of `dev` at clone time, 2026-04-19).
**Upstream:** `realsweetpaul/claude-codex` — Apache-2.0.
**Posture of this doc:** facts with file:line citations. Open questions in §G. No code written yet.

All paths below are relative to `claude-codex/` unless otherwise noted.

---

## A. Stack

- **Primary language:** Rust. Multi-language monorepo (also TS/JS, Python, Bazel/Starlark, shell, Nix).
- **Rust workspace root:** `codex-rs/Cargo.toml` — `resolver = "2"`, `edition = "2024"`, **88 member crates**.
- **Rust toolchain:** `codex-rs/rust-toolchain.toml` pins `channel = "1.94.1"` with `clippy`, `rustfmt`, `rust-src`. Our codex-agent pins `1.95.0`; **minor version mismatch** — see §G1.
- **Node:** `package.json` engines `node >=22`, `pnpm >=10.29.3`.
- **Build system(s):** cargo (primary); Bazel + MODULE.bazel for the release path; `just` wraps both (`justfile`:1–100).
- **Test runner:** `cargo nextest run --no-fail-fast` (`justfile`:51–53). No `cargo test` at root; nextest is canonical.
- **License:** `LICENSE` is Apache-2.0 v2. Compatible with codex-agent's re-derivation posture. No CLA required; attribution via existing `NOTICE`.
- **Self-version:** `codex-rs/Cargo.toml` workspace-level `package.version` derived via `CARGO_PKG_VERSION` (see `model-provider-info/src/lib.rs:290`). MSRV not separately pinned — governed by the toolchain file.

## B. Provider / wire architecture

### B.1 Provider record

`codex-rs/model-provider-info/src/lib.rs:89–140` defines `ModelProviderInfo`:

```rust
pub struct ModelProviderInfo {
    pub name: String,
    pub base_url: Option<String>,
    pub env_key: Option<String>,
    pub env_key_instructions: Option<String>,
    pub experimental_bearer_token: Option<String>,
    pub auth: Option<ModelProviderAuthInfo>,     // command-backed bearer
    pub wire_api: WireApi,                        // Responses | Messages
    pub query_params: Option<HashMap<String,String>>,
    pub http_headers: Option<HashMap<String,String>>,
    pub env_http_headers: Option<HashMap<String,String>>,
    pub request_max_retries: Option<u64>,
    pub stream_max_retries: Option<u64>,
    pub stream_idle_timeout_ms: Option<u64>,
    pub websocket_connect_timeout_ms: Option<u64>,
    #[serde(default)] pub requires_openai_auth: bool,
    #[serde(default)] pub supports_websockets: bool,
}
```

### B.2 Wire enum

`codex-rs/model-provider-info/src/lib.rs:41–86`:

```rust
pub enum WireApi { Responses, Messages }
```

Only two variants. The OpenAI chat-completions wire (`/v1/chat/completions`) **has no dedicated variant** — `wire_api = "chat"` was removed upstream (same file, :36). See §G2.

### B.3 Wire dispatch seam

`codex-rs/core/src/client.rs:1665–1729` — `ModelClientSession::stream()` matches on `effective_wire_api(model_slug)`:

```rust
match wire_api {
    WireApi::Responses => { /* WebSocket or HTTP → stream_responses_api */ }
    WireApi::Messages  => { stream_messages_api(..).await }
}
```

- `stream_responses_api` (:1206–~1298) hits the provider `base_url` (`/v1/responses`) over `ReqwestTransport` from `codex-rs/codex-api/`.
- `stream_messages_api` (:1304–~1460) builds Anthropic-shaped `messages` + `system` blocks and uses `ApiMessagesClient`.

Both converge on `ResponseStream` which yields `codex_api::ResponseEvent` (`codex-rs/core/src/client_common.rs:1`).

### B.4 The event enum `ResponseEvent`

`codex-rs/codex-api/src/common.rs:66–99`:

```rust
pub enum ResponseEvent {
    Created,
    OutputItemDone(ResponseItem),
    OutputItemAdded(ResponseItem),
    ServerModel(String),
    ServerReasoningIncluded(bool),
    Completed { stop_reason: Option<String>, response_id: String, token_usage: Option<TokenUsage> },
    OutputTextDelta(String),
    ReasoningSummaryDelta { delta: String, summary_index: i64 },
    ReasoningContentDelta  { delta: String, content_index: i64 },
    ReasoningSummaryPartAdded { summary_index: i64 },
    RateLimits(RateLimitSnapshot),
    ModelsEtag(String),
}
```

This is **different from both** of our two enums:

- `codex_copilot::sse::ResponseEvent { ContentDelta | ToolCall | Done }` (frozen, 66-test baseline).
- `provider_sse::ResponseEvent { TextDelta | ToolCallStart | ToolCallArgsDelta | ToolCallEnd | Usage | Done(StopReason) }`.

Mapping is one-way (Copilot → claude-codex). Notable gaps for adapter:

- Copilot emits `ToolCall` in one shot (no incremental Start/Args/End) at the `codex_copilot::sse` layer; `provider-sse` does emit incrementally. The adapter must reassemble tool-use as `ResponseItem::FunctionCall` wrapped in `OutputItemAdded` / `OutputItemDone`.
- No direct equivalent of `RateLimits` or `ModelsEtag` — we will omit those (they're server-populated on their side; absence is fine).
- `Completed.stop_reason` is the Anthropic shape (`end_turn | tool_use | ...`). For OpenAI-style finish_reason we map `"stop"→"end_turn"`, `"tool_calls"→"tool_use"`, `"length"→"max_tokens"`.

### B.5 `ModelClient` construction

`codex-rs/core/src/client.rs:304–322` — constructor takes `provider: ModelProviderInfo`, `Arc<AuthManager>`, and state. It is the session-scoped entry. Any Copilot integration plugs in either:

1. **Upstream:** a new `WireApi::ChatCompletions` variant + new `stream_chat_completions_api` branch, or
2. **Adapter:** route via a new `WireApi::Copilot` with a fully bespoke `stream_copilot_api` that delegates to `codex_copilot::CopilotHttpClient` and bridges event types.

(Design choice deferred to `02-design.md`. Recommendation is option 2 — minimal surface on core/client.rs; see §G3.)

### B.6 Auth

`codex-rs/login/` holds `auth_env_telemetry.rs`, `device_code_auth.rs`, `pkce.rs`, `provider_auth.rs`. Two auth paths in practice:

- **OpenAI/ChatGPT** — `auth.json` in `XLI_HOME` (`codex-rs/utils/home-dir/src/lib.rs:12–67`). ChatGPT login via device code or PKCE.
- **Generic provider** — `env_key` → env var (validated in `model-provider-info/src/lib.rs:233–249`), or `auth.command` (command-backed bearer).

Neither path reads `~/.config/github-copilot/` — **our integration must keep that discovery inside `codex-copilot`** (invariant 4, 5). Auth surfaces to `ModelClient` as `Arc<AuthManager>`; we will construct a stand-in `AuthManager` that is a no-op for Copilot and keep the Copilot token cache private to the adapter.

### B.7 Existing Copilot references

Only one hit in tree, unrelated: `codex-rs/codex-mcp/src/mcp_connection_manager.rs:1764` references `https://api.githubcopilot.com/mcp/` for MCP-over-Copilot wiring. **No LLM-level Copilot code today.** Clean landing zone.

## C. The `-p` profile system

### C.1 Flag surface

- `codex-rs/exec/src/cli.rs:47` — `#[arg(long = "profile", short = 'p')]` on the exec subcommand.
- `codex-rs/cli/src/main.rs:1452–1453` — `if let Some(profile) = subcommand_cli.config_profile { interactive.config_profile = Some(profile); }` pushes subcommand `-p` up to the top-level struct.
- `codex-rs/cli/src/main.rs:1786,1797` — unit test `"-p", "my-profile"` asserts the canonical parse.
- No subcommand literally binds `-p`; `-p` resolves to `--profile` from the top-level `InteractiveCli`.

### C.2 Profile schema

`codex-rs/config/src/profile_toml.rs:24–79`. **TOML** under a `[profiles.<name>]` table. Fields that matter for this integration:

```toml
[profiles.copilot]
model            = "claude-sonnet-4-5"   # frozen-name string; passed to Copilot API as-is
model_provider   = "copilot"             # key into model_providers map
approval_policy  = "on-request"          # AskForApproval enum
sandbox_mode     = "workspace-write"     # SandboxMode enum
# reasoning / verbosity / features optional
```

The `model_provider` field is the join key into the `model_providers` map (either built-in or user-defined in `config.toml`).

### C.3 Profile resolution

- `codex-rs/core/src/config/edit.rs:858` — `ConfigBuilder::with_profile(Option<&str>)` merges the named profile on top of global defaults.
- `codex-rs/cli/src/main.rs:1099,1112` — both interactive and exec paths call `with_profile(interactive.config_profile.as_deref())`.
- No environment default; the default profile is whichever one matches your `config.toml` top-level defaults if `-p` is omitted (there is a `default_profile` key documented in upstream but not mandatory).

### C.4 What a profile actually selects

Resolving `-p copilot` picks:

- `model` string (passed to the provider wire directly).
- `model_provider` key → resolves through `built_in_model_providers()` (`model-provider-info/src/lib.rs:331–355`) then overlaid by `model_providers` from `config.toml`.
- Reasoning/verbosity/approval/sandbox/tool toggles.

For our integration: a user-defined `[model_providers.copilot]` entry plus a `[profiles.copilot]` entry is enough to wire things — **no core change to profile_toml.rs required.** The only core change is the wire dispatch and the adapter (see §B.5, B.3).

## D. Tool / session architecture

### D.1 Turn loop

`codex-rs/core/src/codex.rs` (8 122 LOC). Dispatch goes through `ModelClientSession::stream` → `ResponseStream` → handled by a turn loop (`codex_delegate.rs`, `codex_thread.rs`). The actual tool execution routes through `core/src/tools/orchestrator.rs` and `core/src/tools/registry.rs`.

### D.2 Tool registry

`codex-rs/core/src/tools/registry.rs:190–216`:

```rust
pub struct ToolRegistry {
    handlers: HashMap<String, Arc<dyn AnyToolHandler>>,
}
```

Handlers keyed by `tool_handler_key(name, namespace)`. Registry is **closed** today (`register` is commented as `TODO(jif) for dynamic tools.`) — we cannot add Copilot-specific tools at runtime, and we do not need to. Tools are model-side; Copilot just needs to emit tool-call frames the existing registry understands.

### D.3 `apply_patch`

`codex-rs/core/src/apply_patch.rs` wraps `codex_apply_patch::{ApplyPatchAction, ApplyPatchFileChange}`. Gated by approval policy + sandbox. **No changes required** — we emit function-call frames for `apply_patch` identically to the current OpenAI flow.

### D.4 Shell sandboxing

`codex-rs/sandboxing/`, `codex-rs/linux-sandbox/`, `codex-rs/process-hardening/`. Seatbelt on macOS (see `sandboxing/src/seatbelt.rs:489`). Bypass via `--dangerously-bypass-approvals-and-sandbox` (upstream convention).

## E. Test posture

- **Runner:** `just test` → `cargo nextest run --no-fail-fast` (`justfile`:51–53).
- **Online/offline split:** Offline is the norm. WireMock-style fixtures live under `codex-rs/core/src/client_tests.rs` and crate-local `*_tests.rs` files (naming convention).
- **Heavy-dep test trap:** **Unknown.** No explicit "do not run cargo test at root" in claude-codex's AGENTS.md or the justfile. The workspace has 88 crates; running `cargo nextest run` at root may or may not compile all of them into a test binary. **Flag this in §G4.** For the integration we will prefer `cargo nextest run -p <crate>` per-crate, matching codex-agent's discipline until proven safe.
- **Green baseline:** **Unknown from local inspection.** Not counted in this recon pass — requires running the suite. Placeholder; will be filled in Phase 4 (`03-validation.md`).
- **Fixture replay:** `codex-rs/core/src/client.rs:1218` references `CODEX_RS_SSE_FIXTURE` — SSE can be replayed from a static fixture path. Useful for the ported Copilot SSE tests.

## F. Config surface overlap

Top-level config file: `$XLI_HOME/config.toml` (default `~/.xli/config.toml`). Keys already owned by claude-codex that touch provider selection:

| Key | Shape | Notes |
|---|---|---|
| `model` | `String` | Global default model slug. |
| `model_provider` | `String` | Global default provider key. |
| `model_providers.<id>` | `ModelProviderInfo` | Provider definitions. **We add `copilot` here.** |
| `profiles.<id>` | `ConfigProfile` | Named bundles. **We add `copilot` here.** |
| `model_reasoning_effort` | enum | Ignored for Copilot (no reasoning param). |
| `model_verbosity` | enum | Maps to `verbosity` if Copilot supports it; otherwise drop. |

**No direct conflicts.** Copilot's own config state stays inside `codex-copilot` via:

- `CopilotConfig` (editor/plugin strings, TOS banner state).
- `CopilotAuth` (`~/.config/github-copilot/` discovery — kept verbatim, invariant 4).
- `CopilotHttpClient` (13 verbatim headers — invariant 4, 5).

The only key the integration introduces at the claude-codex schema level is `[model_providers.copilot]`, which is an instance of an already-deny-unknown-fields struct. We do **not** add new fields to `ModelProviderInfo` in this pass. Copilot-specific knobs (editor version override, TTY guard bypass for tests) stay private inside the adapter.

## G. Open questions for the operator

These are the recon-time unknowns. Weapons-free per your 0317Z call; I proceed with the recommendations below unless you countermand.

1. **Rust toolchain mismatch.** claude-codex pins `1.94.1`; codex-agent pins `1.95.0`. The Copilot crate builds clean on both (we do not depend on 1.95-only features). **Plan:** honor claude-codex's `rust-toolchain.toml` inside its tree and keep codex-agent on 1.95.0. No crate pin shift.

2. **Wire enum extension vs routing.** `WireApi` has only `Responses | Messages` today. Copilot speaks OpenAI chat-completions SSE. Two options:
   - **(A) Add `WireApi::Copilot`** — explicit, clean match arm in `client.rs:1677`, isolates the adapter. Touches `model-provider-info/src/lib.rs` + `core/src/client.rs`.
   - **(B) Piggyback on `WireApi::Responses`** — lie about the wire and intercept in `stream_responses_api` based on provider name. Rejected: couples the adapter to the Responses transport's WebSocket logic (`client.rs:1679`).
   - **Recommendation:** **(A)**. This is the smallest surgical change and it keeps `effective_wire_api`'s non-Anthropic auto-upgrade logic from firing on Copilot.

3. **Adapter crate placement.** Two sub-options:
   - **(A) New crate `codex-rs/copilot/`** inside claude-codex's workspace; depends on `codex-copilot` via path dep to `/codex-agent/codex-copilot` (local during development) or git dep (for PRs that land without the parent repo).
   - **(B) Vendor `codex-copilot/src/**`** into a new `codex-rs/copilot/` crate in-tree.
   - **Recommendation:** **(A) with a git dep** pinned to `netbrah/codex-agent` at a SHA, so the 66-test Copilot baseline stays authoritatively at `netbrah/codex-agent` (invariant 1 preserved upstream by reference, not by copy). Vendor is a fallback if `openai/codex` upstream doesn't accept git deps.

4. **Workspace-root test safety.** No explicit "do not run cargo test at root" notice exists in claude-codex. Risk: the workspace drags v8-sys or similar heavies. **Plan:** run `cargo nextest run -p <crate>` per-crate during Phase 3/4. Before Phase 4 I run a single `cargo metadata --format-version 1 | rg v8-sys` check to confirm; if present, we stay per-crate. If absent, we still prefer per-crate for speed.

5. **Auth surface to `ModelClient`.** Constructor requires `Option<Arc<AuthManager>>`. Copilot token refresh lives entirely inside `CopilotHttpClient` (invariant 10 — `chat_stream_with_auth` owns 401-retry). **Plan:** pass `None` as `auth_manager` when the provider is Copilot. The adapter's own client already handles refresh; claude-codex's `AuthManager` would only double-count and fight it.

6. **Event mapping fidelity.** claude-codex's `ResponseEvent` distinguishes `OutputItemAdded` (streaming start) vs `OutputItemDone` (complete). `codex_copilot::sse::ResponseEvent::ToolCall` fires once per tool-call when fully assembled (invariant 8 — `process_body_chunked` is what makes this work). **Plan:** the adapter issues `OutputItemAdded(FunctionCall{partial})` at first tool chunk and `OutputItemDone(FunctionCall)` at the terminator. Incremental arg streaming is **not** re-exposed — acceptable because the turn loop only commits function-calls at `OutputItemDone`. This mirrors how `provider_sse::sse_to_response_events` already reassembles.

7. **TOS banner + TTY guard.** Invariant 3 requires `is_interactive_tty()` gates production construction. claude-codex's `cli/src/main.rs` already owns TTY detection for its own login flow. **Plan:** the adapter calls `codex_copilot::is_interactive_tty()` once at `ModelClient::new(...)` for Copilot providers and prints the banner via `print_tos_banner` (already in `codex_copilot::auth`). Skipped in tests via the offline construction path.

---

## Summary table

| Thing | Claude-codex location | Codex-agent counterpart | Adapter action |
|---|---|---|---|
| Wire enum | `model-provider-info/src/lib.rs:43` | n/a | **Add** `WireApi::Copilot` |
| Wire dispatch | `core/src/client.rs:1665` | `codex-core::ProviderClient::chat_stream` | New arm → `stream_copilot_api` |
| Event type | `codex-api/src/common.rs:67` | `provider_sse::ResponseEvent` | One-way mapper in adapter |
| Provider record | `model-provider-info/src/lib.rs:91` | — | `[model_providers.copilot]` TOML |
| Profile struct | `config/src/profile_toml.rs:26` | — | `[profiles.copilot]` TOML (no struct change) |
| Tool registry | `core/src/tools/registry.rs:190` | `tools-core::ToolRegistry` | **No change** |
| `apply_patch` | `core/src/apply_patch.rs` | `tools-core::apply_patch` | **No change** |
| Auth manager | `login/` + `AuthManager` | `codex_copilot::CopilotHttpClient` | Pass `None`; Copilot self-auths |
| Config root | `~/.xli/config.toml` | — | Same file, new keys |
| Test runner | `cargo nextest run --no-fail-fast` | `cargo test -p <crate> --release` | Per-crate only |
| License | Apache-2.0 | — | Compatible; keep NOTICE |

## Deliverable status

- [x] §A Stack
- [x] §B Provider / wire architecture
- [x] §C `-p` profile system
- [x] §D Tool / session architecture
- [x] §E Test posture (baseline count deferred to Phase 4)
- [x] §F Config surface overlap
- [x] §G Open questions with recommendations

**Next:** `02-design.md` — integration design locking (A) `WireApi::Copilot`, (A) new crate with git dep, event mapper sig, test plan, no-regression checklist.
