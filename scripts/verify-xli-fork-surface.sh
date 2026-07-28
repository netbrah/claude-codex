#!/usr/bin/env bash
# verify-xli-fork-surface.sh — Ratchet gate for the XLI wire-island campaign (S-WI-05).
#
# Counts in-place upstream patches (git diff upstream/main...HEAD --diff-filter=M)
# and fails when non-island modified files exceed the threshold. Mirrors Apex
# scripts/verify-fork-surface.sh with an island allowlist subtracted.
#
# Usage (from xli repo root):
#   bash scripts/verify-xli-fork-surface.sh 258 11
#   bash scripts/verify-xli-fork-surface.sh 80 15    # v5 exit targets
#
# Thresholds (xli-v4 @ upstream/main...xli-v4, 2026-06-25):
#   non_island_modified=258   wire_adjacent_modified=11
#   modified_total=267        added_fork_only=467     deleted=8
#
# See: cli-ops/sortie-board/xli-v5/recon/B6-seam-contract-rust.md
#
# Exit codes:
#   0 — within thresholds
#   1 — ratchet or deletion policy violation
#   2 — usage / missing upstream remote

set -euo pipefail

NON_ISLAND_THRESHOLD="${1:-}"
WIRE_ADJACENT_THRESHOLD="${2:-}"

if [[ -z "$NON_ISLAND_THRESHOLD" ]]; then
  cat <<EOF >&2
Usage: $0 <non_island_modified_threshold> [wire_adjacent_modified_threshold]

Measures fork surface with three-dot diff: upstream/main...HEAD
  non_island_modified  — modified upstream files outside island allowlist
  wire_adjacent_modified — non-island modified under codex-api / provider-* / model-provider*

Phase 1 baseline (xli-v4): 258 11
v5 exit targets:            80 15

Allowlist: scripts/xli-island-allowlist.txt
EOF
  exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ALLOWLIST="$SCRIPT_DIR/xli-island-allowlist.txt"
GRANDFATHER_FILE="$SCRIPT_DIR/grandfathered-deletions.txt"
UPSTREAM_REF="${UPSTREAM_REF:-upstream/main}"
COMPARE_REF="${COMPARE_REF:-HEAD}"
DIFF_RANGE="${UPSTREAM_REF}...${COMPARE_REF}"

if ! git -C "$REPO_ROOT" remote get-url upstream >/dev/null 2>&1; then
  echo "ERROR: no 'upstream' remote. Add openai/codex:" >&2
  echo "  git remote add upstream https://github.com/openai/codex.git" >&2
  exit 2
fi

if [[ ! -f "$ALLOWLIST" ]]; then
  echo "ERROR: missing allowlist: $ALLOWLIST" >&2
  exit 2
fi

git -C "$REPO_ROOT" fetch upstream --quiet 2>/dev/null || {
  echo "WARN: could not fetch upstream; using cached refs." >&2
}

# --- allowlist helpers ---
is_island_path() {
  local path="$1"
  local line
  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line%%#*}"
    line="${line#"${line%%[![:space:]]*}"}"
    line="${line%"${line##*[![:space:]]}"}"
    [[ -z "$line" ]] && continue
    if [[ "$line" == */ ]]; then
      [[ "$path" == "$line"* ]] && return 0
    elif [[ "$path" == "$line" ]]; then
      return 0
    fi
  done < "$ALLOWLIST"
  return 1
}

is_wire_zone_path() {
  local path="$1"
  [[ "$path" == codex-rs/codex-api/* ]] && return 0
  [[ "$path" == codex-rs/provider-* ]] && return 0
  [[ "$path" == codex-rs/model-provider* ]] && return 0
  return 1
}

MODIFIED_FILES=()
while IFS= read -r f; do
  [[ -n "$f" ]] && MODIFIED_FILES+=("$f")
done < <(git -C "$REPO_ROOT" diff "$DIFF_RANGE" --diff-filter=M --name-only)

MODIFIED_TOTAL=${#MODIFIED_FILES[@]}
NON_ISLAND_FILES=()
WIRE_ADJ_FILES=()

for f in "${MODIFIED_FILES[@]}"; do
  if is_island_path "$f"; then
    continue
  fi
  NON_ISLAND_FILES+=("$f")
  if is_wire_zone_path "$f"; then
    WIRE_ADJ_FILES+=("$f")
  fi
done

NON_ISLAND_COUNT=${#NON_ISLAND_FILES[@]}
WIRE_ADJ_COUNT=${#WIRE_ADJ_FILES[@]}

ADDED_COUNT=$(git -C "$REPO_ROOT" diff "$DIFF_RANGE" --diff-filter=A --name-only \
  | grep -c . || true)
DELETED_COUNT=$(git -C "$REPO_ROOT" diff "$DIFF_RANGE" --diff-filter=D --name-only \
  | grep -c . || true)

LINE_CHURN=$(git -C "$REPO_ROOT" diff "$DIFF_RANGE" --diff-filter=M --numstat \
  | awk '{a+=$1; d+=$2} END {print "+"a" / -"d}')

GRANDFATHERED_COUNT=0
NEW_DELETED_COUNT=0
NEW_DELETED_LIST=""
if [[ -f "$GRANDFATHER_FILE" ]]; then
  GRANDFATHERED_COUNT=$(grep -cve '^\s*$' "$GRANDFATHER_FILE" || echo 0)
  NEW_DELETED_LIST=$(comm -23 \
    <(git -C "$REPO_ROOT" diff "$DIFF_RANGE" --diff-filter=D --name-only | sort) \
    <(grep -v '^\s*$' "$GRANDFATHER_FILE" | sort) || true)
  if [[ -n "$NEW_DELETED_LIST" ]]; then
    NEW_DELETED_COUNT=$(printf "%s\n" "$NEW_DELETED_LIST" | grep -c . || true)
  fi
fi

cat <<EOF
=== XLI Wire Island — Fork Surface Report ===
diff range:              $DIFF_RANGE
modified upstream (M):   $MODIFIED_TOTAL
non-island modified:     $NON_ISLAND_COUNT   (threshold: $NON_ISLAND_THRESHOLD)
wire-adjacent modified:  $WIRE_ADJ_COUNT${WIRE_ADJACENT_THRESHOLD:+   (threshold: $WIRE_ADJACENT_THRESHOLD)}
added fork-only (A):     $ADDED_COUNT     (unbounded — expected in island crates)
deleted upstream (D):    $DELETED_COUNT   (grandfathered: $GRANDFATHERED_COUNT, new: $NEW_DELETED_COUNT)
line churn on modified:  $LINE_CHURN

Island allowlist: $ALLOWLIST
EOF

if [[ "$NEW_DELETED_COUNT" -gt 0 ]]; then
  echo "FAIL: $NEW_DELETED_COUNT NEW upstream deletions (not grandfathered)." >&2
  printf "%s\n" "$NEW_DELETED_LIST" >&2
  exit 1
fi

if [[ "$NON_ISLAND_COUNT" -gt "$NON_ISLAND_THRESHOLD" ]]; then
  echo "FAIL: non-island modified $NON_ISLAND_COUNT > $NON_ISLAND_THRESHOLD" >&2
  echo "" >&2
  echo "Top non-island paths (wire-adjacent first):" >&2
  if [[ ${#WIRE_ADJ_FILES[@]} -gt 0 ]]; then
    echo "--- wire-adjacent (S-WI-04 targets) ---" >&2
    printf "  %s\n" "${WIRE_ADJ_FILES[@]}" >&2
  fi
  echo "--- top churn (non-island) ---" >&2
  git -C "$REPO_ROOT" diff "$DIFF_RANGE" --diff-filter=M --numstat \
    | while read -r add del path; do
        is_island_path "$path" && continue
        printf "%6s %s\n" "$((add + del))" "$path"
      done \
    | sort -rn | head -20 >&2
  echo "" >&2
  echo "Move cross-wire code into island paths or register in allowlist with doctrine review." >&2
  echo "See: sortie-board/xli-v5/recon/B6-seam-contract-rust.md" >&2
  exit 1
fi

if [[ -n "$WIRE_ADJACENT_THRESHOLD" && "$WIRE_ADJ_COUNT" -gt "$WIRE_ADJACENT_THRESHOLD" ]]; then
  echo "FAIL: wire-adjacent modified $WIRE_ADJ_COUNT > $WIRE_ADJACENT_THRESHOLD" >&2
  printf "  %s\n" "${WIRE_ADJ_FILES[@]}" >&2
  exit 1
fi

echo "PASS: non-island $NON_ISLAND_COUNT ≤ $NON_ISLAND_THRESHOLD${WIRE_ADJACENT_THRESHOLD:+, wire-adjacent $WIRE_ADJ_COUNT ≤ $WIRE_ADJACENT_THRESHOLD}."

if [[ "$NON_ISLAND_COUNT" -eq "$NON_ISLAND_THRESHOLD" ]]; then
  echo "NOTE: non-island count at threshold — next harness patch fails CI." >&2
fi

exit 0
