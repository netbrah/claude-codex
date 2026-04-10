#!/usr/bin/env bash
set -euo pipefail

# ============================================================================
# XLI Build Script
#
#   ./build.sh                    Build macOS arm64 (local dev)
#   ./build.sh linux              Build Linux x86_64 (distribution)
#   ./build.sh all                Build both + npm tarball
#   ./build.sh pack               npm pack only (after builds)
#
#   VERSION=0.2.0 ./build.sh all  Set version
# ============================================================================

ROOT="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$ROOT/.." && pwd)"
CODEX="$REPO/codex-rs"
SKILLS="$ROOT/skills"
NPM="$ROOT/npm"
SAMPLES="$CODEX/skills/src/assets/samples"

# Single source of truth: codex-rs/Cargo.toml workspace version
VERSION="${VERSION:-$(grep '^version = ' codex-rs/Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')}"
MAC_TRIPLE="aarch64-apple-darwin"
LINUX_TRIPLE="x86_64-unknown-linux-gnu"

# ── helpers ──────────────────────────────────────────────────────────────────

die()  { echo "✗ $*" >&2; exit 1; }
info() { echo "▸ $*"; }
ok()   { echo "  ✓ $*"; }

inject_skills() {
    if [ -d "$SAMPLES" ] && [ ! -L "$SAMPLES" ]; then
        mv "$SAMPLES" "${SAMPLES}.bak"
    fi
    ln -sfn "$SKILLS" "$SAMPLES"
    ok "Skills injected ($(ls "$SKILLS" 2>/dev/null | wc -l | tr -d ' ') dirs)"
}

restore_skills() {
    rm -f "$SAMPLES" 2>/dev/null || true
    if [ -d "${SAMPLES}.bak" ]; then
        mv "${SAMPLES}.bak" "$SAMPLES"
    fi
}
trap restore_skills EXIT

build_mac() {
    info "Building macOS arm64..."
    cd "$CODEX"
    cargo build --release -p codex-cli 2>&1 | tail -3
    local bin="$CODEX/target/release/xli"
    [ -f "$bin" ] || die "macOS build failed"

    local dest="$NPM/vendor/$MAC_TRIPLE/xli"
    mkdir -p "$dest"
    cp "$bin" "$dest/xli"
    strip "$dest/xli" 2>/dev/null || true
    chmod +x "$dest/xli"
    ok "macOS arm64: $(du -h "$dest/xli" | cut -f1)"
}

build_linux() {
    info "Building Linux x86_64 (remote)..."
    "$ROOT/remote-build.sh" || die "Remote Linux build failed"
}

do_pack() {
    info "Packaging npm tarball (v$VERSION)..."
    cd "$NPM"

    # Stamp version
    python3 -c "
import json
with open('package.json') as f: p = json.load(f)
p['version'] = '$VERSION'
with open('package.json','w') as f: json.dump(p, f, indent=2); f.write('\n')
"
    # Clean old tarballs
    rm -f "$ROOT"/netapp-xli-*.tgz 2>/dev/null || true

    npm pack --pack-destination "$ROOT" 2>/dev/null
    local tgz
    tgz="$(ls "$ROOT"/netapp-xli-*.tgz 2>/dev/null | head -1)"
    [ -f "$tgz" ] || die "npm pack failed"

    ok "$(basename "$tgz") ($(du -h "$tgz" | cut -f1))"
    echo ""
    echo "  Upload:  scp $(basename "$tgz") devbox:~/"
    echo "  Install: npm i -g $tgz"
    echo "  Run:     xli"
}

# ── main ─────────────────────────────────────────────────────────────────────

echo ""
echo "  XLI Build — v$VERSION"
echo "  ────────────────────"
echo ""

# Ensure skills dir exists and has content
[ -d "$SKILLS" ] || die "No skills/ directory. Create it and add SKILL.md folders."
if [ -z "$(ls -A "$SKILLS" 2>/dev/null)" ]; then
    info "Warning: skills/ is empty — building with no custom skills"
fi

inject_skills

case "${1:-mac}" in
    mac|macos|local)
        build_mac
        ;;
    linux)
        build_linux
        ;;
    all)
        build_mac
        build_linux
        do_pack
        ;;
    pack)
        do_pack
        ;;
    *)
        echo "Usage: $0 {mac|linux|all|pack}"
        exit 1
        ;;
esac

echo ""
echo "  Done."
