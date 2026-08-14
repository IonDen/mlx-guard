#!/usr/bin/env bash
set -euo pipefail

"$(dirname "$0")/test-fast.sh"
cargo audit --deny warnings

if cargo deny --version >/dev/null 2>&1; then
    cargo deny check
else
    echo "cargo-deny is not installed; CI remains authoritative for deny.toml" >&2
fi
