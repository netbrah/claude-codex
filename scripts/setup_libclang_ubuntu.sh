#!/bin/bash
# Install libclang development libraries on Ubuntu/Debian.
#
# Usage:
#   sudo ./scripts/setup_libclang_ubuntu.sh
#
# In GitHub Actions:
#   - name: Install libclang
#     run: sudo ./scripts/setup_libclang_ubuntu.sh

set -euo pipefail

echo "==> Installing libclang-dev and llvm-dev..."
apt-get update -qq
apt-get install -y --no-install-recommends libclang-dev llvm-dev

LIBCLANG_PATH="$(llvm-config --libdir 2>/dev/null || echo "")"
if [ -z "$LIBCLANG_PATH" ]; then
    # Fallback: search common paths.
    for candidate in /usr/lib/llvm-*/lib; do
        if [ -f "$candidate/libclang.so" ] || [ -f "$candidate/libclang.dylib" ]; then
            LIBCLANG_PATH="$candidate"
            break
        fi
    done
fi

if [ -z "$LIBCLANG_PATH" ]; then
    echo "ERROR: Could not locate libclang after install." >&2
    exit 1
fi

echo "==> libclang located at: $LIBCLANG_PATH"

# Export for GitHub Actions.
if [ -n "${GITHUB_ENV:-}" ]; then
    echo "LIBCLANG_PATH=$LIBCLANG_PATH" >> "$GITHUB_ENV"
    echo "==> Exported LIBCLANG_PATH to GITHUB_ENV"
fi

export LIBCLANG_PATH
echo "==> Done."
