#!/bin/sh
# ============================================================================
# Upload XLI release artifacts to NetApp Artifactory
#
# Usage:
#   ./deploy/upload.sh                  # uploads from deploy/
#   VERSION=0.2.0 ./deploy/upload.sh    # with explicit version
#
# Prerequisites:
#   - ARTIFACTORY_TOKEN env var (API key or identity token)
#   - Built artifacts: deploy/npm/vendor/<target>/xli{/xli}
#
# Uploads:
#   generic-local/xli/<version>/xli-<target>.tar.gz     (per-platform binary)
#   generic-local/xli/<version>/netapp-xli-<ver>.tgz    (npm tarball)
#   generic-local/xli/LATEST                            (version pointer)
#   generic-local/xli/<version>/install.sh              (installer script)
# ============================================================================

set -eu

ROOT="$(cd "$(dirname "$0")" && pwd)"
VERSION="${VERSION:-0.1.0}"
ARTIFACTORY_BASE="${ARTIFACTORY_URL:-https://artifactory.corp.netapp.com/artifactory/generic-local/xli}"

die()  { echo "✗ $*" >&2; exit 1; }
info() { echo "▸ $*"; }
ok()   { echo "  ✓ $*"; }

[ -n "${ARTIFACTORY_TOKEN:-}" ] || die "ARTIFACTORY_TOKEN must be set"

upload() {
  local_path="$1"
  remote_path="$2"
  info "Uploading $(basename "$local_path") ..."
  curl -fsSL -T "$local_path" \
    -H "X-JFrog-Art-Api: ${ARTIFACTORY_TOKEN}" \
    "${ARTIFACTORY_BASE}/${remote_path}"
  ok "$(basename "$local_path")"
}

echo ""
echo "  XLI Upload — v$VERSION"
echo "  ─────────────────────"
echo ""

tmp_dir="$(mktemp -d)"
cleanup() { rm -rf "$tmp_dir"; }
trap cleanup EXIT INT TERM

# ── Package per-platform tarballs ────────────────────────────────────────────

for target_dir in "$ROOT/npm/vendor"/*/; do
  target="$(basename "$target_dir")"
  # Find the binary — either at <target>/xli/xli or <target>/xli
  if [ -f "$target_dir/xli/xli" ]; then
    bin_path="$target_dir/xli/xli"
  elif [ -f "$target_dir/xli" ]; then
    bin_path="$target_dir/xli"
  else
    info "Skipping $target (no binary found)"
    continue
  fi

  tarball="xli-${target}-${VERSION}.tar.gz"
  # Create tarball with just the binary named "xli"
  cp "$bin_path" "$tmp_dir/xli"
  chmod +x "$tmp_dir/xli"
  tar -czf "$tmp_dir/$tarball" -C "$tmp_dir" xli
  rm "$tmp_dir/xli"

  upload "$tmp_dir/$tarball" "${VERSION}/${tarball}"
done

# ── Upload npm tarball if it exists ──────────────────────────────────────────

npm_tarball="$(ls "$ROOT"/netapp-xli-*.tgz 2>/dev/null | head -1)"
if [ -n "$npm_tarball" ]; then
  upload "$npm_tarball" "${VERSION}/$(basename "$npm_tarball")"
fi

# ── Upload install script ───────────────────────────────────────────────────

if [ -f "$ROOT/install.sh" ]; then
  upload "$ROOT/install.sh" "${VERSION}/install.sh"
  upload "$ROOT/install.sh" "install.sh"
fi

# ── Update LATEST pointer ───────────────────────────────────────────────────

printf '%s' "$VERSION" > "$tmp_dir/LATEST"
upload "$tmp_dir/LATEST" "LATEST"

echo ""
echo "  Done. Users can install with:"
echo "    curl -fsSL ${ARTIFACTORY_BASE}/install.sh | sh"
echo ""
