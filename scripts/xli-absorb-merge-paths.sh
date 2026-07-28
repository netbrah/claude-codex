#!/usr/bin/env bash
# xli-absorb-merge-paths.sh — Path-level upstream absorb (S-WI-01)
#
# Applies per-path OURS/THEIRS policies from xli-absorb-path-policies.toml
# for paths that differ between THEIRS_REF and OURS_REF.
#
# Usage:
#   bash scripts/xli-absorb-merge-paths.sh --dry-run
#   bash scripts/xli-absorb-merge-paths.sh --apply
#   bash scripts/xli-absorb-merge-paths.sh --dry-run --paths codex-rs/codex-api/src/lib.rs
#
# Env:
#   THEIRS_REF=upstream/main   OURS_REF=HEAD
#
# merge-careful paths are listed but not auto-applied (S-WI-04 manual fence).
#
# After --apply on Cargo bucket:
#   cd codex-rs && bash ../scripts/gen-cargo-workspace.sh && cargo generate-lockfile

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
POLICIES="$SCRIPT_DIR/xli-absorb-path-policies.toml"
THEIRS_REF="${THEIRS_REF:-upstream/main}"
OURS_REF="${OURS_REF:-HEAD}"
DRY_RUN=1
FILTER_PATHS=()

usage() {
  cat <<EOF
Usage: $0 --dry-run | --apply [--paths <path> ...]

Classify and optionally checkout paths from THEIRS_REF per policy file.
EOF
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=1; shift ;;
    --apply) DRY_RUN=0; shift ;;
    --paths)
      shift
      while [[ $# -gt 0 && "$1" != --* ]]; do
        FILTER_PATHS+=("$1")
        shift
      done
      ;;
    -h|--help) usage ;;
    *) echo "Unknown arg: $1" >&2; usage ;;
  esac
done

if [[ ! -f "$POLICIES" ]]; then
  echo "ERROR: missing $POLICIES" >&2
  exit 2
fi

# Parse policies.toml (minimal — no external toml dep)
declare -a POLICY_PATTERNS=()
declare -a POLICY_VALUES=()
_pending_pattern=""
while IFS= read -r line; do
  line="${line%%#*}"
  line="${line#"${line%%[![:space:]]*}"}}"
  line="${line%"${line##*[![:space:]]}"}"
  [[ -z "$line" ]] && continue
  if [[ "$line" =~ ^pattern\ =\ \"(.*)\"$ ]]; then
    _pending_pattern="${BASH_REMATCH[1]}"
  elif [[ "$line" =~ ^policy\ =\ \"(.*)\"$ && -n "$_pending_pattern" ]]; then
    POLICY_PATTERNS+=("$_pending_pattern")
    POLICY_VALUES+=("${BASH_REMATCH[1]}")
    _pending_pattern=""
  fi
done < "$POLICIES"

if [[ "${#POLICY_PATTERNS[@]}" -ne "${#POLICY_VALUES[@]}" ]]; then
  echo "ERROR: policy parse mismatch" >&2
  exit 2
fi

match_policy() {
  local path="$1"
  local i pat pol
  for i in "${!POLICY_PATTERNS[@]}"; do
    pat="${POLICY_PATTERNS[$i]}"
    pol="${POLICY_VALUES[$i]}"
    if [[ "$pat" == "**/*.snap" && "$path" == *.snap ]]; then
      echo "$pol"; return 0
    fi
    if [[ "$pat" == "**/snapshots/" && "$path" == *"/snapshots/"* ]]; then
      echo "$pol"; return 0
    fi
    if [[ "$path" == "$pat" || "$path" == "$pat"* ]]; then
      echo "$pol"; return 0
    fi
  done
  echo "theirs"
}

MB=$(git -C "$REPO_ROOT" merge-base "$THEIRS_REF" "$OURS_REF")
echo "merge-base: $MB"
echo "theirs: $THEIRS_REF  ours: $OURS_REF  mode: $([ "$DRY_RUN" -eq 1 ] && echo dry-run || echo apply)"
echo ""

THEIRS_LIST=()
OURS_LIST=()
SKIP_LIST=()
MERGE_CAREFUL_LIST=()
UNION_LIST=()

collect_paths() {
  if [[ ${#FILTER_PATHS[@]} -gt 0 ]]; then
    printf '%s\n' "${FILTER_PATHS[@]}"
    return
  fi
  comm -12 \
    <(git -C "$REPO_ROOT" diff --name-only "$MB" "$THEIRS_REF" | sort) \
    <(git -C "$REPO_ROOT" diff --name-only "$MB" "$OURS_REF" | sort)
}

while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  pol=$(match_policy "$path")
  case "$pol" in
    ours) OURS_LIST+=("$path") ;;
    theirs) THEIRS_LIST+=("$path") ;;
    skip) SKIP_LIST+=("$path") ;;
    merge-careful) MERGE_CAREFUL_LIST+=("$path") ;;
    union) UNION_LIST+=("$path") ;;
    *) THEIRS_LIST+=("$path") ;;
  esac
done < <(collect_paths)

echo "=== Policy summary (both-touched paths) ==="
echo "  ours: ${#OURS_LIST[@]}"
echo "  theirs: ${#THEIRS_LIST[@]}"
echo "  merge-careful: ${#MERGE_CAREFUL_LIST[@]}"
echo "  union: ${#UNION_LIST[@]}"
echo "  skip: ${#SKIP_LIST[@]}"
echo ""

if [[ ${#MERGE_CAREFUL_LIST[@]} -gt 0 ]]; then
  echo "=== merge-careful (manual / S-WI-04 fence — not auto-applied) ==="
  printf '  %s\n' "${MERGE_CAREFUL_LIST[@]}"
  echo ""
fi

if [[ ${#UNION_LIST[@]} -gt 0 ]]; then
  echo "=== union (run gen-cargo-workspace.sh — not git checkout) ==="
  printf '  %s\n' "${UNION_LIST[@]}"
  echo ""
fi

if [[ ${#OURS_LIST[@]} -gt 0 ]]; then
  echo "=== ours (keep current tree) ==="
  printf '  %s\n' "${OURS_LIST[@]}" | head -30
  [[ ${#OURS_LIST[@]} -gt 30 ]] && echo "  ... (+$((${#OURS_LIST[@]} - 30)) more)"
  echo ""
fi

if [[ ${#THEIRS_LIST[@]} -gt 0 ]]; then
  echo "=== theirs ($([ "$DRY_RUN" -eq 1 ] && echo would checkout || echo checking out)) ==="
  if [[ "$DRY_RUN" -eq 1 ]]; then
    printf '  %s\n' "${THEIRS_LIST[@]}" | head -40
    [[ ${#THEIRS_LIST[@]} -gt 40 ]] && echo "  ... (+$((${#THEIRS_LIST[@]} - 40)) more)"
  else
    for path in "${THEIRS_LIST[@]}"; do
      echo "  theirs: $path"
      git -C "$REPO_ROOT" checkout "$THEIRS_REF" -- "$path"
    done
  fi
  echo ""
fi

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "DRY-RUN complete. Use --apply to checkout ${#THEIRS_LIST[@]} theirs paths."
else
  echo "APPLY complete: ${#THEIRS_LIST[@]} paths from $THEIRS_REF"
  echo "Next: bash scripts/gen-cargo-workspace.sh && cd codex-rs && cargo check -p codex-protocol -p codex-api -p codex-core"
fi
