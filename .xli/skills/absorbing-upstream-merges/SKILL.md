---
name: absorbing-upstream-merges
description: Load before running `git fetch upstream && git merge upstream/main` or when triaging conflicts from an upstream `openai/codex` sync. Encodes the post-Sortie-1 provider topology (model-provider trait + provider-registry + provider-anthropic + provider-copilot impl crates + core orchestration), names the narrow set of files that still collide with upstream, and gives the resolution recipe that preserves the XLI wedge without re-fighting the same conflicts every merge.
allowed-tools: Read, Edit, Grep, Bash(git *), Bash(cargo check --workspace), Bash(cargo test -p codex-core -p codex-model-provider -p codex-provider-anthropic -p codex-provider-copilot -p codex-provider-registry -p codex-prompt)
---

# Absorbing upstream/main merges post-Sortie-1

## When to load

Any time the operator is about to run, or is mid-way through, an upstream
sync from `openai/codex`. Concrete triggers:

- "Absorb the latest upstream", "fetch upstream and merge", "upstream sync",
  "catch up to openai/codex", "bump upstream".
- A merge commit with conflicts in `codex-rs/core/src/client.rs`.
- Any `<<<<<<< HEAD` marker anywhere under `codex-rs/`.

Also load when planning a new wire (Gemini `generateContent`, future
providers) so the operator doesn't re-wedge the exact code that Sortie 1
just extracted.

## Post-Sortie-1 architecture (keep this map)

```
                 codex-api                 (wire types: Messages/Responses
                  (leaf)                    request/response structs,
                                            transport client shells)
                     |
                 codex-prompt              (Prompt + ResponseStream —
                  (leaf)                    pure value types)
                     |
                 codex-model-provider      (ModelProvider trait, default
                  (leaf)                    ConfiguredModelProvider, Bedrock,
                                            auth helpers, MessagesBackend
                                            seam, ProviderStreamRequest +
                                            Builder)
                  /            \
   codex-provider-anthropic   codex-provider-copilot
       (wire impl crate)          (wire impl crate)
                  \            /
              codex-provider-registry      (THE enumerator — one match arm
                  (leaf)                    per wire; single place that
                                            knows all providers exist)
                         |
        codex-core, codex-cli, codex-models-manager, ...
```

Rules this topology enforces:

1. **New wires become new crates.** Adding Gemini `generateContent` means
   a new `codex-provider-gencontent` impl crate plus one `match` arm in
   `codex-provider-registry::create_model_provider`. No new code in
   `codex-core`. No new branch in `client.rs::stream()`.
2. **codex-model-provider has no dep on any impl crate.** The impl
   crates depend on it for the trait; reversing that creates a cycle.
   The registry sits above both layers specifically to resolve this.
3. **codex-core owns transport orchestration, not wire shape.** The 401
   retry loop, `current_client_setup`, telemetry composition,
   `ApiMessagesClient` dispatch, `map_response_stream`, auth recovery
   all live in core. The `MessagesBackend` trait is the narrow seam;
   provider crates call it, core implements it.
4. **Responses wire is native upstream.** `ConfiguredModelProvider`
   inherits the `Err(UnsupportedOperation)` default for
   `ModelProvider::stream`. `client.rs::stream()` has an explicit
   `if Responses` arm that calls `stream_responses_api` directly —
   that's NOT a wedge, it's core staying native on the OpenAI path.

## What upstream will churn (and how)

### Files that still collide with upstream

| File | Frequency | Why |
|------|-----------|-----|
| `codex-rs/core/src/client.rs` | HIGH | Upstream's highest-churn file. We still own `stream()`'s Messages arm, `stream_messages_api`, `run_messages_turn`, `MessagesBackendAdapter`, `stream_copilot_api` (until 5c), `effective_wire_api` (until 5c), Copilot state on `ModelClient`, `current_client_setup`'s Copilot branch. |
| `codex-rs/core/src/codex.rs` | HIGHEST | Session orchestration. We don't own a wedge here intentionally — if conflicts surface, take upstream and re-apply only surgical XLI additions. |
| `codex-rs/core/src/tools/spec.rs` | MEDIUM | Tool registry. Take upstream; audit for provider-aware tool gating if upstream adds any. |
| `codex-rs/Cargo.lock` | EVERY MERGE | Regenerable — delete conflict markers, then run `cargo check --workspace` which rewrites it deterministically. |
| `codex-rs/Cargo.toml` | LOW | Workspace members/deps. We've added `prompt`, `model-provider`, `provider-anthropic`, `provider-copilot`, `provider-registry`. Re-apply those entries inside the workspace arrays after taking upstream. |

### Files that NEVER collide anymore

These used to conflict regularly before Sortie 1 and shouldn't now:

- `codex-rs/core/src/client_common.rs` — shrank to 26-line re-export shim.
- Anthropic translator files — moved to `codex-provider-anthropic` entirely.
- Copilot helpers (`ensure_v1_prefix`, `stamp_copilot_shared_headers`) —
  live in `codex-provider-copilot`.

If one of these collides, the upstream diff is an upstream-wide refactor
that predates our 2026-04 absorption; take upstream, re-audit our
extraction crate for any drift.

## The sync recipe

1. **Preflight** (ALWAYS before `git merge`):
   ```bash
   cd ~/Projects/xli
   git checkout dev
   git pull origin dev
   git status                                   # must be clean
   git fetch upstream
   git rev-list --left-right --count upstream/main...dev
   git log --oneline dev..upstream/main | head -40
   git diff --stat upstream/main...dev -- codex-rs/core/src/client.rs
   ```
   Tag the pre-merge state: `git tag pre-upstream-merge-<YYYY-MM-DD>`.

2. **Merge (never rebase)**:
   ```bash
   git merge upstream/main
   ```
   Merging preserves XLI commit history which matters for sortie
   traceability. Rebasing rewrites SHAs and breaks every in-flight
   sortie branch.

3. **Resolve `codex-rs/Cargo.lock`** first (trivial — `cargo check
   --workspace` regenerates it after all other conflicts are resolved).

4. **Resolve `codex-rs/core/src/client.rs`** next. Resolution rule:
   **take upstream's version**, then re-apply the XLI additions inside
   the right structural slot:

   - `WireApi::Messages` arm of `ModelClientSession::stream()` — keep
     ours (calls `stream_messages_api`).
   - `WireApi::Copilot` arm — keep ours (calls `stream_copilot_api`
     until 5c lands; then calls `provider.stream(req)`).
   - Private methods `stream_messages_api`, `run_messages_turn`,
     `stream_copilot_api`, `effective_wire_api`, `current_client_setup`
     Copilot branch — keep ours verbatim unless upstream changed the
     enclosing `impl ModelClientSession` surface.
   - `MessagesBackendAdapter<'a>` and the trailing
     `pub(crate) use codex_provider_anthropic::*` re-exports at end of
     file — keep ours.
   - If upstream moved a function we also moved: take upstream's move
     and re-verify our call site still resolves. Expect this on
     `current_client_setup` (we added a Copilot branch inline); if
     upstream refactored it into a new helper, re-apply the Copilot
     branch there.

5. **Re-apply workspace membership** in `codex-rs/Cargo.toml` if it
   collided. Our members (after Sortie 1): `prompt`, `model-provider`,
   `provider-anthropic`, `provider-copilot`, `provider-registry`. Our
   `[workspace.dependencies]` entries: same five, each with
   `{ path = "<crate>" }`.

6. **Verify** before committing the merge:
   ```bash
   cd codex-rs
   cargo check --workspace                      # regenerates Cargo.lock
   cargo test -p codex-core -p codex-api \
              -p codex-model-provider \
              -p codex-provider-anthropic \
              -p codex-provider-copilot \
              -p codex-provider-registry \
              -p codex-prompt
   ```
   Expected baseline (may drift with upstream changes — capture delta):
   - `codex-model-provider`: ~13 tests
   - `codex-provider-anthropic`: ~96 tests
   - `codex-provider-copilot`: ~16 tests
   - `codex-provider-registry`: ~5 tests
   - `codex-prompt`: 1 test
   - `codex-core --lib`: passing tests should MATCH the pre-merge
     count; any regression is either an upstream change we must adopt
     or a conflict we resolved wrong.

7. **Live smoke** on Copilot Claude turn before publishing to `dev2`.
   Exercises the `MessagesBackend` seam end-to-end (see
   `wire-messages-anthropic` skill for which behaviors to watch).

8. **Tag the post-merge state**: `git tag post-upstream-merge-<YYYY-MM-DD>`.

9. **Push `dev`** to `origin` (landing zone only).

10. **Merge `dev` → `dev2`** with `--no-ff` so the merge commit is a
    bisect anchor. `dev2` is the integration branch releases cut from.

## Conflict-resolution patterns

### Pattern: upstream renamed a type we depend on

Example: upstream changes `ReasoningEffortConfig` → `ReasoningLevel`.

Resolution steps:
1. Search our crates: `rg 'ReasoningEffortConfig' codex-rs/`.
2. Update impl crates first (`provider-anthropic`, `provider-copilot`,
   `provider-registry`, `model-provider`).
3. Update `codex-core`'s `stream_messages_api` + `run_messages_turn`.
4. Update tests that pattern-match on the enum.
5. Re-run the workspace test gate.

### Pattern: upstream added a new field to `ProviderStreamRequest`'s semantic counterpart

Upstream doesn't know about our `ProviderStreamRequest`. They'll grow
`ModelClientSession::stream()`'s argument list or a `StreamParams`
struct. Resolution:
1. Accept their new field into our `ModelClientSession::stream()`
   signature.
2. Thread it through to `stream_messages_api` (and `stream_copilot_api`
   until 5c).
3. Add a matching field to `ProviderStreamRequest` in
   `codex-model-provider/src/stream.rs` (the struct is
   `#[non_exhaustive]` so cross-crate impact is zero; internal adds
   are free).
4. Add a `.with_*` setter to `ProviderStreamRequestBuilder`.
5. Wire the builder call in `stream_messages_api`'s new construction
   site.
6. Decide if the provider consumes it. Anthropic likely ignores
   anything Responses-shaped (e.g. `summary`, `service_tier`); wire it
   through but no-op in `build_messages_request`.

### Pattern: upstream changed how auth flows into the transport

Our `run_messages_turn` calls `self.client.current_client_setup()`
which owns the auth-manager → `CurrentClientSetup` transformation
(including the Copilot `CopilotCtx` branch). If upstream rewrites this
boundary:
1. Take upstream's new `current_client_setup` shape.
2. Re-apply the Copilot branch (`ensure_copilot_ctx` → snapshot →
   `stamp_copilot_shared_headers` + `stream_idle_timeout = 1800s` +
   `CoreAuthProvider` construction). That branch is scheduled to move
   to `CopilotModelProvider` in Commit 5c — if 5c has already landed,
   the branch shouldn't exist in `current_client_setup` anymore.

### Pattern: upstream added a new `WireApi` variant

(Unlikely — `WireApi` lives in `codex-model-provider-info` which we
also fork, but upstream could add variants in a future refactor.)
Resolution:
1. `codex-model-provider-info` absorbs the new variant.
2. `codex-provider-registry::create_model_provider` grows a new
   `match` arm. If no impl exists, it falls through to
   `ConfiguredModelProvider` which refuses via the trait default.
3. Consumers that pattern-match on `WireApi` (there are a few in
   `codex-core` tests) need the new arm.

## Checklist (print-and-go)

- [ ] Pre-merge: working tree clean, on `dev`, `git fetch upstream` run.
- [ ] Tagged `pre-upstream-merge-<date>`.
- [ ] `git merge upstream/main` executed.
- [ ] `Cargo.lock` conflicts: deleted markers, `cargo check --workspace`.
- [ ] `client.rs` conflicts: took upstream, re-applied the Messages
      arm + `MessagesBackendAdapter` + helper methods.
- [ ] `Cargo.toml` conflicts: re-applied workspace members + deps.
- [ ] `cargo check --workspace` clean (warnings-only).
- [ ] Provider crate test gate green.
- [ ] `codex-core --lib` passing-count matches pre-merge baseline.
- [ ] Live Copilot Claude turn smoke-tested.
- [ ] Tagged `post-upstream-merge-<date>`.
- [ ] Pushed `origin/dev`.
- [ ] Merged `dev → dev2` with `--no-ff`.

## Related skills

- `wire-messages-anthropic` — deep dive on the Anthropic `/messages`
  wire, the `MessagesBackend` seam, and `stream_messages_api`'s
  current shape.
- `copilot-adapter-discipline` — invariants on `codex-copilot-adapter`
  (still in place; Commit 5c moves its ownership but not its contract).
- `sortie-branch-discipline` — commit hygiene, branch model
  (`dev` landing zone / `dev2` integration / `apex/sortie/*` work).
- `rust-workspace-hygiene` — new-crate registration pattern for when
  the sync adds a new codex crate that XLI also needs.
