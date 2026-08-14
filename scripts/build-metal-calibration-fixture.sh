#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 OUTPUT" >&2
    exit 64
fi

repo_root=$(cd "$(dirname "$0")/.." && pwd)
output=$1
clang -fobjc-arc -Wall -Wextra -Werror -Wpedantic \
    -framework Foundation -framework Metal \
    "$repo_root/fixtures/calibrate_metal.m" -o "$output"
