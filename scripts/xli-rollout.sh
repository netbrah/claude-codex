#!/usr/bin/env bash
# xli-rollout — show model/provider for the most recent rollout(s)
set -uo pipefail
N="${1:-1}"
ROOT="${XLI_HOME:-$HOME/.xli}/sessions"
ls -t "$ROOT"/*/*/*/rollout-*.jsonl 2>/dev/null | head -"$N" | while read -r f; do
  python3 - "$f" <<'PY'
import json, sys, re
path = sys.argv[1]
print(path)
with open(path) as fh:
    first = fh.readline()
    meta = json.loads(first).get("payload", {})
    print(f'  provider:    {meta.get("model_provider", "?")}')
    print(f'  branch:      {meta.get("git", {}).get("branch", "?")}')
    print(f'  cwd:         {meta.get("cwd", "?")}')
    # Scan a few lines for the first model field
    model = None
    for line in fh:
        m = re.search(r'"model":"([^"]+)"', line)
        if m:
            model = m.group(1); break
    print(f'  model:       {model or "?"}')
print()
PY
done
