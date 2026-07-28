#!/usr/bin/env bash
# xli-smoke — wall-clock smoke test of every profile in ~/.xli/config.toml
#
# Usage:
#   scripts/xli-smoke.sh                    # all profiles
#   scripts/xli-smoke.sh gemini gpt copilot # selected profiles
#
# What it does:
#   1. Verifies binary freshness via xli-stamp.sh
#   2. For each profile, runs:
#        xli -p <profile> exec --skip-git-repo-check 'Reply with: SMOKE_OK_<profile>'
#      with a 60s timeout.
#   3. Reports PASS if SMOKE_OK_<profile> appears in output, FAIL otherwise.
#   4. Prints a compact summary.
set -uo pipefail

XLI_BIN="$HOME/Projects/xli/codex-rs/target/release/xli"
STAMP="$HOME/Projects/xli/scripts/xli-stamp.sh"
TIMEOUT=60

if [[ ! -x "$XLI_BIN" ]]; then
  echo "✗ xli binary missing: $XLI_BIN — run xli-build" >&2
  exit 1
fi

# Soft freshness check (warns but doesn't block — operator may want to test old binary).
"$STAMP" || true
echo

if (( $# > 0 )); then
  PROFILES=("$@")
else
  # Auto-discover profiles from config.toml.
  CONFIG="${XLI_HOME:-$HOME/.xli}/config.toml"
  mapfile -t PROFILES < <(grep -E '^\[profiles\.' "$CONFIG" | sed -E 's/^\[profiles\.([^]]+)\]/\1/')
fi

if (( ${#PROFILES[@]} == 0 )); then
  echo "✗ no profiles found" >&2
  exit 1
fi

PASS=()
FAIL=()
LOGDIR="$(mktemp -d)"
echo "Logs: $LOGDIR"
echo "Profiles: ${PROFILES[*]}"
echo

for prof in "${PROFILES[@]}"; do
  printf "▸ %-22s ... " "$prof"
  log="$LOGDIR/$prof.log"
  marker="SMOKE_OK_${prof//-/_}"
  start=$(date +%s)
  # `gtimeout` if available (coreutils on mac), else perl one-liner.
  if command -v gtimeout >/dev/null 2>&1; then
    TO=(gtimeout "$TIMEOUT")
  elif command -v timeout >/dev/null 2>&1; then
    TO=(timeout "$TIMEOUT")
  else
    TO=(perl -e "alarm $TIMEOUT; exec @ARGV" --)
  fi

  "${TO[@]}" "$XLI_BIN" -p "$prof" exec --skip-git-repo-check \
    "Reply with exactly this string and nothing else: $marker" \
    > "$log" 2>&1
  rc=$?
  elapsed=$(( $(date +%s) - start ))

  if grep -q "$marker" "$log"; then
    echo "PASS (${elapsed}s)"
    PASS+=("$prof")
  else
    echo "FAIL (rc=$rc, ${elapsed}s) — $log"
    FAIL+=("$prof")
  fi
done

echo
echo "──────────── summary ────────────"
echo "PASS: ${#PASS[@]} (${PASS[*]:-})"
echo "FAIL: ${#FAIL[@]} (${FAIL[*]:-})"
(( ${#FAIL[@]} == 0 ))
