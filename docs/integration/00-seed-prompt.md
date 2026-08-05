# Seed Prompt — Cross-Repo Integration Agent

**Mission:** drop `codex-copilot` (from `netbrah/codex-agent`, main, commit `c6d5129` or later) into `netbrah/claude-codex` (already forked from `realsweetpaul/claude-codex`, private, default branch `dev`) without regressing either side. Deliver a per-phase plan, a PR-ready diff, and updated docs — all artifacts land under `netbrah/copilot-codex/docs/integration/`.

**Workspace:** `/home/user/workspace/copilot-codex/` is the canonical multiroot. Both child repos are already cloned inside it as plain independent clones (gitignored in the parent). See `copilot-codex/AGENTS.md` and `README.md` for topology.

**Posture:** questions are better than answers right now. The user has asked for **recon first, integration second.** Do not assume claude-codex internals. The only things the operator has asserted about claude-codex are:

1. It has an **Anthropic wire** already.
2. It has a **`-p <profile>` system** for selecting providers/models.

Everything else — language, build system, crate topology, tool seam, session loop, config schema — must be verified from the source tree before any code is written.

---

## Inputs you will receive

- **Source repo (complete, 83 tests green):** `netbrah/codex-agent` at commit `c6d5129` or later.
  - Working tree at `/home/user/workspace/copilot-codex/codex-agent/`.
  - Orient from `AGENTS.md` at that repo's root. Do not skip it.
  - The Copilot layer you are porting is the crate `codex-copilot/` — frozen public API, 66 tests.
  - The bridge layer `codex-core/` already defines the `ProviderClient` trait that abstracts Copilot and Anthropic.

- **Target repo (already forked):** `netbrah/claude-codex`, private, default branch `dev`, parent `realsweetpaul/claude-codex`.
  - Working tree at `/home/user/workspace/copilot-codex/claude-codex/`.
  - Use `bash` with `api_credentials=["github"]` for any gh calls.
  - Do **not** run `gh auth status` or print credentials.

- **Non-negotiable invariants from codex-agent** (verbatim — see `codex-agent/AGENTS.md` §"No-regression invariants"):
  1. 83-test baseline stays green at every commit.
  2. No `anyhow` in `codex-copilot`.
  3. TTY guard preserved.
  4. 13 Copilot headers verbatim, with `copilot-api/src/lib/api-config.ts:<line>` citations.
  5. Redacted `Debug` on `CopilotTokenCache`.
  6. No heavy deps (no `chrono`, no `secrecy`, no `native-tls`).
  7. `rustls-tls` on `reqwest`.
  8. `process_body_chunked` + `flush` stays.
  9. `slow_down` arithmetic stays (`+5s` to base interval).
  10. `chat_stream_with_auth` owns 401-retry; `chat_stream_raw` does not.

- **Equivalent invariants from claude-codex:** unknown. Recon and catalog them.

---

## Phase 0 — Recon (the point of this pass)

**You may not write any integration code until Phase 0 is complete and the user has reviewed your findings.** Deliverable: `docs/integration/01-recon.md` in `codex-agent`. Answer, with file-and-line citations:

### A. Stack

- What language(s) is `realsweetpaul/claude-codex` written in? Rust / TS / Python / mixed?
- Build system: `cargo`, `npm`/`pnpm`, `uv`, `pyproject`, `just`, `make`? Show the top-level manifest.
- How does it declare its own version and MSRV / engines?
- LICENSE file — what license? Is reuse compatible with codex-agent's re-derivation posture (see `codex-agent/docs/REFERENCES.md`)?

### B. Provider / wire architecture

- Where is the Anthropic wire implemented? File path, entry function, transport library (`reqwest` / `axios` / `httpx` / …), streaming shape.
- What event / token enum does it stream into the session loop? Name it and paste the definition.
- Is there a provider **trait/interface** (as opposed to a hand-wired Anthropic call)? If yes, show it. If no, what's the extension seam?
- How does auth flow in today? API key env var, keychain, file on disk, OAuth?

### C. The `-p` profile system

- Where is `-p` parsed (CLI flag definition)?
- What is the profile schema (TOML / YAML / JSON)? Paste an example profile.
- What does a profile actually select — model id only, provider + model, full transport config?
- Where does the profile get resolved into a concrete provider instance?
- Is there a default profile? How is it chosen?

### D. Tool / session architecture

- Is there a turn loop? Show its entry point.
- Is there a tool registry equivalent to `tools-core::ToolRegistry`? What shape?
- Is there an `apply_patch` or filesystem-edit tool? How are edits gated?
- How does it sandbox shell execution (if at all)?

### E. Test posture

- Test runner, offline/online split, mock transport library.
- Is there a forbidden `cargo test`-at-root equivalent (e.g., pulls heavy deps)?
- What's the current green baseline (number of tests passing on `main`)?

### F. Config surface overlap

- Enumerate every config key the user sets today on claude-codex.
- Flag any that conflicts or overlaps with how `codex-copilot` is constructed (`CopilotConfig`, `CopilotAuth`, `CopilotHttpClient`).

### G. Open questions to bring to the user

List them explicitly. Do not guess. The user has said "questions are better than answers right now" — this is the section they want.

---

## Phase 1 — Integration design (after user review of Phase 0)

Deliverable: `docs/integration/02-design.md`. Must answer:

- **Adapter shape.** Does `claude-codex` grow a new `CopilotProvider`, or does `codex-copilot` get a thin shim crate that implements `claude-codex`'s provider interface?
- **Dependency direction.** Does `claude-codex` take a git/path dep on `codex-copilot`, or do we vendor? Justify with license posture + release cadence.
- **Profile schema extension.** Propose the exact TOML (or whatever format claude-codex uses) for a `copilot` profile. Preserve the `-p copilot` UX.
- **Enum bridging.** Map `codex-copilot`'s simple `ResponseEvent` and/or the richer `provider_sse::ResponseEvent` onto whatever claude-codex streams internally. Show the conversion function signature.
- **Auth bootstrapping.** How does claude-codex construct the `CopilotHttpClient`? Where does the TTY guard fire? Where does the banner print? Where does OAuth discovery happen (`~/.config/github-copilot/`)?
- **Test plan.** Which codex-agent tests migrate verbatim, which need wiremock-in-claude-codex, which stay in codex-agent only. Commit to an exact final test count.
- **No-regression checklist.** Apply the 10 invariants from `codex-agent/AGENTS.md` to the integrated tree and confirm each still holds, or flag the exception with rationale.

---

## Phase 2 — Fork + scaffold

- `gh repo fork realsweetpaul/claude-codex --clone=false` → `netbrah/claude-codex` (private by default).
- Clone to `/home/user/workspace/claude-codex/`.
- Create branch `feat/codex-copilot-integration` off `main`.
- Apply the Phase 1 scaffold only (new files, no edits to existing files). Commit as `scaffold: add codex-copilot integration skeleton`.

## Phase 3 — Wire

- Implement the adapter from Phase 1.
- Port tests per the Phase 1 test plan.
- `cargo clippy` / `cargo fmt` (or the claude-codex equivalents) must stay clean.
- Commit per slice; do not mega-commit.

## Phase 4 — Validate

- Run the full claude-codex test suite + the ported Copilot tests.
- Run `codex-agent`'s own per-crate suites — 83 tests must still be green (proves nothing regressed upstream).
- Produce `docs/integration/03-validation.md` with test output, clippy output, and a per-invariant checkpoint.

## Phase 5 — Handoff

- `docs/integration/04-pr-body.md` — PR body ready to paste when the user takes it upstream.
- `docs/integration/05-handoff.md` — CHARLIE MIKE-style handoff for the next operator (the user, or the agent that takes it to `openai/codex`).

---

## Ground rules

- **Work in `/home/user/workspace/copilot-codex/`** for all integration docs. Write artifacts to `copilot-codex/docs/integration/` (this repo), not inside either child repo.
- **Never run `cargo test` at any workspace root** unless you have verified disk budget in that specific repo. In codex-agent it is forbidden (v8-sys → 22 GB). In claude-codex, check first.
- **Rust 1.95.0 at `~/.cargo/bin/cargo`.** Source `. "$HOME/.cargo/env"` before use.
- **GitHub via `bash` + `api_credentials=["github"]`.** Default `--private` on fork. Do not inspect credentials.
- **Commits on `netbrah/codex-agent` go to `main` directly** — no PR process.
- **Commits on `netbrah/claude-codex` go to `feat/codex-copilot-integration`** (branch off `dev`) until the user says otherwise.
- **Commits on `netbrah/copilot-codex` go to `main` directly** — no PR process. This is where all integration docs live.
- **When in doubt, ask the user.** Phase 0 §G is the holding pen for questions; surface them early.
- **No emojis in commits or docs.** Concise, factual prose.

## Definition of done (for this seed mission)

- `copilot-codex/docs/integration/01-recon.md` through `05-handoff.md` exist, cited, and reviewed.
- `netbrah/claude-codex` exists, has the integration branch pushed, and its test suite plus the ported Copilot tests are all green.
- `codex-agent` main still shows 83 tests green.
- Zero changes to `codex-copilot/src/*.rs` or its test files. If you need to edit the Copilot crate to make integration work, stop and escalate to the user — that is a signal the adapter boundary is in the wrong place.
