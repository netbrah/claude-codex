#!/usr/bin/env bash
# Reports whether the release xli binary is older than any tracked source.
set -uo pipefail
BIN="$HOME/Projects/xli/codex-rs/target/release/xli"
SRC="$HOME/Projects/xli/codex-rs"

if [[ ! -x "$BIN" ]]; then
  echo "✗ xli binary missing: $BIN — run \`xli-build\`"
  exit 1
fi

bin_mtime=$(stat -f %m "$BIN")
newest_src=$(find "$SRC" -type f \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' \) \
  -not -path '*/target/*' -print0 \
  | xargs -0 stat -f '%m %N' \
  | sort -rn | head -1)

newest_mtime=${newest_src%% *}
newest_path=${newest_src#* }

if (( newest_mtime > bin_mtime )); then
  bin_dt=$(date -r "$bin_mtime" '+%Y-%m-%d %H:%M:%S')
  src_dt=$(date -r "$newest_mtime" '+%Y-%m-%d %H:%M:%S')
  echo "⚠ STALE: binary built $bin_dt"
  echo "         newer source: $newest_path ($src_dt)"
  echo "         run: xli-build"
  exit 2
fi

bin_dt=$(date -r "$bin_mtime" '+%Y-%m-%d %H:%M:%S')
echo "✓ fresh: $BIN ($bin_dt)"
