#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
fixture_build_dir=$(mktemp -d "${TMPDIR:-/tmp}/mlx-guard-metal.XXXXXX")
trap 'rm -rf -- "$fixture_build_dir"' EXIT

clang -fobjc-arc -Wall -Wextra -Werror -Wpedantic \
    -framework Foundation -framework Metal \
    "$repo_root/fixtures/small_metal.m" -o "$fixture_build_dir/small_metal"

set +e
"$fixture_build_dir/small_metal" 5001 >"$fixture_build_dir/invalid.out" \
    2>"$fixture_build_dir/invalid.err"
invalid_status=$?
set -e
if [[ $invalid_status -ne 64 ]]; then
    echo "expected over-ceiling Metal fixture to exit 64, got $invalid_status" >&2
    exit 1
fi

"$fixture_build_dir/small_metal" 25 >"$fixture_build_dir/metal.out"
if ! rg -q '^WORKER_READY kind=metal$' "$fixture_build_dir/metal.out"; then
    echo "bounded Metal fixture did not acknowledge real work" >&2
    exit 1
fi
