#!/bin/bash
# Run the clang-graph feature-gated tests.
#
# Prerequisites:
#   - libclang-dev installed (see setup_libclang_ubuntu.sh)
#   - LIBCLANG_PATH set (or llvm-config available)
#
# Usage:
#   ./scripts/test_libclang.sh

set -euo pipefail

export LIBCLANG_PATH="${LIBCLANG_PATH:-$(llvm-config --libdir 2>/dev/null || echo "")}"
if [ -z "$LIBCLANG_PATH" ]; then
    echo "ERROR: LIBCLANG_PATH not set and llvm-config not found." >&2
    echo "Run: sudo ./scripts/setup_libclang_ubuntu.sh" >&2
    exit 1
fi

echo "==> LIBCLANG_PATH=$LIBCLANG_PATH"

# Enable the integration test gate.
export CODEX_TEST_LIBCLANG=1

echo "==> Running clang-graph unit + integration tests..."
cargo test --features clang-graph -p codex-core -- clang_graph

echo "==> All clang-graph tests passed."
