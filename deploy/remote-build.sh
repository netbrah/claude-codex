#!/usr/bin/env bash
set -euo pipefail

# ============================================================================
# Remote Linux Build Script
#
# Builds XLI natively on an x86_64 Linux remote via SSH.
# Handles: source rsync, git dep sync, V8 prebuilt cache, vendor sync.
#
# Usage:
#   ./deploy/remote-build.sh              # Build on SCS
#   REMOTE=myhost ./deploy/remote-build.sh  # Use different host
#
# Prerequisites on remote:
#   - rustup with 1.94.1 toolchain (synced from local if needed)
#   - gcc, cmake, nasm, libcap-devel, pkg-config
# ============================================================================

REMOTE="${REMOTE:-scs}"
REMOTE_DIR="/tmp/xli"
LOCAL_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CODEX_RS="$LOCAL_ROOT/codex-rs"
NPM="$LOCAL_ROOT/deploy/npm"
LINUX_TRIPLE="x86_64-unknown-linux-gnu"

die()  { echo "✗ $*" >&2; exit 1; }
info() { echo "▸ $*"; }
ok()   { echo "  ✓ $*"; }

echo ""
echo "  XLI Remote Build"
echo "  ─────────────────"
echo "  Remote : $REMOTE"
echo "  Dir    : $REMOTE_DIR"
echo ""

# ── Step 1: Rsync source to remote ──────────────────────────────────────────
info "Syncing source to $REMOTE:$REMOTE_DIR ..."
ssh "$REMOTE" "mkdir -p $REMOTE_DIR/codex-rs"
rsync -az --delete --exclude='target/' --exclude='.git/' --exclude='node_modules/' --exclude='*.tar.gz' "$CODEX_RS/" "$REMOTE:$REMOTE_DIR/codex-rs/"
ok "Source synced (including vendor/)"

# ── Step 2: Sync cargo git deps (private forks blocked by corp proxy) ───────
info "Syncing cargo git dependencies..."
for name in $(ls "$HOME/.cargo/git/db/" | grep -E 'crossterm-|nucleo-|ratatui-|rules_rust-|rust-sdks-|tokio-tungstenite-|tungstenite-rs-'); do
    rsync -az "$HOME/.cargo/git/db/$name" "$REMOTE:~/.cargo/git/db/" 2>/dev/null
    rsync -az "$HOME/.cargo/git/checkouts/$name" "$REMOTE:~/.cargo/git/checkouts/" 2>/dev/null
done
ok "Git deps synced"

# ── Step 3: Sync V8 prebuilt binary (blocked by corp proxy) ─────────────────
V8_CACHE="$HOME/.cargo/.rusty_v8"
if [ -d "$V8_CACHE" ]; then
    info "Syncing V8 prebuilt cache..."
    rsync -az "$V8_CACHE/" "$REMOTE:~/.cargo/.rusty_v8/"
    ok "V8 cache synced"
fi

# ── Step 4: Build on remote ─────────────────────────────────────────────────
info "Building on $REMOTE (release + fat LTO — may take 20-40 min on first run)..."
# Use SSH keepalive to prevent timeout during long builds.
# The build runs under nohup so it survives brief disconnects.
SSH_OPTS="-o ServerAliveInterval=60 -o ServerAliveCountMax=30"
# shellcheck disable=SC2086
ssh $SSH_OPTS "$REMOTE" bash <<REMOTE_SCRIPT
export PATH=\$HOME/.cargo/bin:\$PATH
export RUSTUP_TOOLCHAIN=1.94.1-x86_64-unknown-linux-gnu
export RUSTUP_NO_UPDATE_CHECK=1
cd $REMOTE_DIR/codex-rs

echo "  rustc: \$(rustc --version)"
echo "  building..."

cargo build --release -p codex-cli 2>&1 | tail -5

BIN="target/release/xli"
if [ -f "\$BIN" ]; then
    echo "  ✓ Build succeeded: \$(du -h "\$BIN" | cut -f1)"
    file "\$BIN"
else
    echo "  ✗ Build failed"
    exit 1
fi
REMOTE_SCRIPT
ok "Remote build complete"

# ── Step 5: Copy binary back ────────────────────────────────────────────────
info "Fetching binary..."
DEST="$NPM/vendor/$LINUX_TRIPLE"
mkdir -p "$DEST"
scp "$REMOTE:$REMOTE_DIR/codex-rs/target/release/xli" "$DEST/xli"
chmod +x "$DEST/xli"
ok "Binary at: $DEST/xli ($(du -h "$DEST/xli" | cut -f1))"

echo ""
echo "  Done. To test on Linux:"
echo "    ssh $REMOTE '$REMOTE_DIR/codex-rs/target/release/xli --version'"
echo ""
