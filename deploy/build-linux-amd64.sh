#!/bin/bash
# Build XLI for Linux x86_64 (run ON a Linux amd64 machine)
# 
# Usage:
#   ssh build-host 'bash -s' < deploy/build-linux-amd64.sh
#   # or: copy this repo to a Linux box and run it there
#
# Prerequisites on the Linux build host:
#   - Rust toolchain (rustup + x86_64-unknown-linux-musl target)
#   - musl-tools: apt install musl-tools
#   - Node 18+
set -euo pipefail

cd "$(dirname "$0")/.."
REPO_ROOT="$(pwd)"
TRIPLE="x86_64-unknown-linux-musl"
VENDOR_DIR="deploy/npm/vendor/${TRIPLE}/xli"

echo "=== Building XLI for ${TRIPLE} ==="

# Ensure target is installed
rustup target add "$TRIPLE" 2>/dev/null || true

cd codex-rs
cargo build --release --target "$TRIPLE" -p codex-exec

cd "$REPO_ROOT"
mkdir -p "$VENDOR_DIR"
cp "codex-rs/target/${TRIPLE}/release/codex-exec" "${VENDOR_DIR}/xli"
chmod +x "${VENDOR_DIR}/xli"

echo "=== Build complete ==="
file "${VENDOR_DIR}/xli"
ls -lh "${VENDOR_DIR}/xli"
echo ""
echo "Binary at: ${VENDOR_DIR}/xli"
echo "To install: cd deploy/npm && npm link"
