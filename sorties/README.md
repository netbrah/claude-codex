# sorties/

Sortie control surface for XLI (`netbrah/xli`).

## Canonical Board

The **single source of truth** for all active and planned work is:

    ~/Projects/cli-ops/sortie-board/xli-v2/SORTIE-BOARD-V3.md

This repo carries operational references and deferred briefs. The board
lives in `cli-ops` because it spans both repos and is the operator's
planning surface.

## Layout

| Path | Contents |
|------|---------|
| `UPSTREAM-COMPATIBILITY-GUIDE.md` | Required pre-read: file ownership, conflict zones, merge discipline |
| `deferred/` | Parked but viable briefs. Queue, not graveyard. See `deferred/README.md` |
| `archive/completed/` | Landed sortie cards kept for bisect context |
| `archive/reviews/` | Deep review artifacts that generated sortie work |
| `archive/stale/` | Briefs whose premises were pre-empted; need rewrite before re-fire |
| `archive/stale/pre-extraction-era/` | Pre-Sortie-1 boards and briefs (summer-prose era, Wave 1 era) |

## Rules

1. New sortie briefs go in `sorties/` at top level until dispatched.
2. Completed work moves to `archive/completed/`.
3. Stale work moves to `archive/stale/`.
4. Deferred work moves to `deferred/` with a reason.
5. The board in `cli-ops` is the only place active status is tracked.
