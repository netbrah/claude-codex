# docs/integration/

Cross-repo integration work. This directory is the canonical engineering trail for the Copilot/XLI integration effort, but **not every file here serves the same role**. The numbered recon/design/validation docs are the primary source of truth; the PR body and handoff are operational appendices.

## Canonical reading order

1. `00-seed-prompt.md` — original mission brief and non-negotiable invariants.
2. `01-recon.md` — source-cited recon on `claude-codex`.
3. `02-design.md` — canonical design and invariant decisions.
4. `03-validation.md` — verification matrix, known gaps, and reviewer command set.
5. `04-oprod-3wire-router.md` — follow-on 3-wire router operations order and after-action record.
6. `06-op2-hardening-recon.md` — current hardening recon and fire/hold matrix.
7. `07-research-copilot-cli.md` — separate research track for the Copilot CLI/SDK JSON-RPC surface.

## File map

| File | Role | Status | Purpose |
|---|---|---|---|
| `00-seed-prompt.md` | Mission brief | Canonical | Original spec and invariants for the integration push. |
| `01-recon.md` | Phase 0 recon | Canonical | Ground-truth codebase recon with file:line citations. |
| `02-design.md` | Phase 1 design | Canonical | Decision record and invariant master. |
| `03-validation.md` | Verification | Canonical | Sign-off matrix, commands, and known gaps. |
| `04-oprod-3wire-router.md` | Follow-on OPROD | Canonical | Native Messages/Responses/Chat routing mission plan and AAR. |
| `04-pr-body.md` | Delivery artifact | Appendix | Paste-ready PR body; useful, but derivative of design/validation. |
| `05-handoff.md` | Delivery artifact | Appendix | Point-in-time operator handoff and loose-ends list. |
| `06-op2-hardening-recon.md` | Hardening recon | Canonical | Read-only fire/hold decisions for the summer-prose hardening pass. |
| `07-research-copilot-cli.md` | Research track | Canonical | Copilot CLI/SDK recon for the JSON-RPC integration surface. |

## Related sortie docs

- `../../sorties/README.md` — sortie control surface, active briefs, and archive map.
- `../../sorties/UPSTREAM-COMPATIBILITY-GUIDE.md` — constitutional rules for where XLI code belongs.
- `../../sorties/deferred/OP2-subagent-ux-spike.md` — detailed subagent UX spike that feeds `06-op2-hardening-recon.md` (currently parked).
- `../../codex-rs/copilot-host/SORTIE.md` — delivery report for the Copilot extension-host fragment.
- `../../codex-rs/vector-search/SORTIE-REPORT.md` — delivery report + backlog for vector search.

## Repository posture

The integration push referenced in `00-seed-prompt.md` originally lived in the
`netbrah/codex-agent` source repo and a `realsweetpaul/claude-codex` fork
target. **That work has landed.** It now lives here on `netbrah/xli`, with the
Copilot wire shipping in-tree under `codex-rs/copilot/` (Track 1, HTTP) and
`codex-rs/copilot-host/` (Track 2, CLI host). There is no separate downstream
fork target on this branch.

Active branch: `sortie/summer-prose`. Default branch: `dev`. Upstream pull
source: `openai/codex` (`upstream` remote).

## Rules

- Every doc in this directory cites file-and-line when making claims.
- Code lands in-tree (`codex-rs/copilot/`, `codex-rs/copilot-host/`, `codex-rs/core/`). No external integration repo on this branch.
- Track 1 ROE lives in `../../codex-rs/copilot/AGENTS.md` and `04-oprod-3wire-router.md`. Treat both as binding for any change to `codex-rs/copilot/`.
- When a new integration artifact is created, decide whether it is **canonical** or **appendix** and update this index immediately.
