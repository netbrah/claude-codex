#!/usr/bin/env bash
# gen-cargo-workspace.sh — Union upstream Cargo.toml + xli overlay (S-WI-02)
#
# Usage:
#   bash scripts/gen-cargo-workspace.sh
#   bash scripts/gen-cargo-workspace.sh --check
#
# Env: UPSTREAM_REF=upstream/main

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CODEX_RS="$REPO_ROOT/codex-rs"
OVERLAY="$CODEX_RS/xli-workspace.overlay.toml"
OUT="$CODEX_RS/Cargo.toml"
UPSTREAM_REF="${UPSTREAM_REF:-upstream/main}"
CHECK=0

[[ "${1:-}" == "--check" ]] && CHECK=1

if [[ ! -f "$OVERLAY" ]]; then
  echo "ERROR: missing overlay $OVERLAY" >&2
  exit 2
fi

python3 - "$UPSTREAM_REF" "$OVERLAY" "$OUT" "$CHECK" <<'PY'
import re
import subprocess
import sys
import tomllib
from pathlib import Path

upstream_ref, overlay_path, out_path, check_mode = sys.argv[1:5]
check_mode = check_mode == "1"
repo = Path(out_path).parent.parent

upstream_toml = subprocess.check_output(
    ["git", "-C", str(repo), "show", f"{upstream_ref}:codex-rs/Cargo.toml"],
    text=True,
)
overlay = tomllib.loads(Path(overlay_path).read_text())

# Parse upstream members
m = re.search(r"members\s*=\s*\[(.*?)\]", upstream_toml, re.S)
if not m:
    sys.exit("could not parse upstream members")
upstream_members = re.findall(r'"([^"]+)"', m.group(1))

overlay_members = [e["path"] for e in overlay.get("members", [])]
all_members = sorted(set(upstream_members) | set(overlay_members))

# Split upstream at members array
pre, rest = upstream_toml.split("members = [", 1)
post_members = rest.split("]", 1)[1]

banner = (
    "# GENERATED — do not edit by hand.\n"
    "# Edit codex-rs/xli-workspace.overlay.toml and run: bash scripts/gen-cargo-workspace.sh\n"
    "# Upstream base: " + upstream_ref + "\n\n"
)

members_block = "members = [\n" + "".join(f'    "{p}",\n' for p in all_members) + "]\n"

# Merge workspace.dependencies: start from upstream table, add overlay-only keys
dep_match = re.search(
    r"(\[workspace\.dependencies\]\n)(.*?)(?=\n\[|\Z)",
    post_members,
    re.S,
)
if not dep_match:
    sys.exit("could not parse [workspace.dependencies]")
dep_header, dep_body = dep_match.group(1), dep_match.group(2)

upstream_keys = set(re.findall(r"^(\S+)\s*=", dep_body, re.M))
overlay_deps = overlay.get("workspace.dependencies", {})
lines = [dep_header.rstrip("\n"), dep_body.rstrip("\n")]
for key, val in overlay_deps.items():
    if key in upstream_keys:
        print(f"WARN: overlay dep {key} collides with upstream — upstream wins", file=sys.stderr)
        continue
    if isinstance(val, str):
        lines.append(f'{key} = "{val}"')
    elif isinstance(val, dict):
        # minimal inline table render
        inner = ", ".join(
            f'{k} = "{v}"' if isinstance(v, str) else f"{k} = {v}"
            for k, v in val.items()
        )
        if "features" in val:
            feats = ", ".join(f'"{f}"' for f in val["features"])
            ver = val.get("version", "")
            lines.append(f'{key} = {{ version = "{ver}", features = [{feats}] }}')
        elif "path" in val:
            lines.append(f'{key} = {{ path = "{val["path"]}" }}')
        else:
            lines.append(f"{key} = {{ {inner} }}")
    else:
        lines.append(f"{key} = {val}")
merged_deps = "\n".join(lines) + "\n"

new_post = post_members[: dep_match.start()] + merged_deps + post_members[dep_match.end() :]
generated = banner + pre.rstrip("\n") + "\n" + members_block + new_post.lstrip("\n")

if check_mode:
    current = Path(out_path).read_text()
    if current != generated:
        print("DRIFT: codex-rs/Cargo.toml differs from generator output", file=sys.stderr)
        sys.exit(1)
    print("OK: Cargo.toml matches overlay generator")
else:
    Path(out_path).write_text(generated)
    print(f"Wrote {out_path} ({len(all_members)} members, +{len(overlay_members)} overlay)")
PY
