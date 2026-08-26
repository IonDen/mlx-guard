#!/usr/bin/env bash
set -euo pipefail

# Capture the escalation-envelope evidence artifact. This records numbers; it never asserts a
# latency target, so it is not a gate. The profile label is what identifies the environment a
# capture came from, which is why an unlabeled run is refused rather than silently published.

if [[ $# -ne 2 ]]; then
    echo "usage: $0 PROFILE OUTPUT_DIRECTORY" >&2
    exit 64
fi

repo_root=$(cd "$(dirname "$0")/.." && pwd)
profile=$1
output_directory=$2
if [[ -z "$profile" ]]; then
    echo "profile label must not be empty" >&2
    exit 64
fi
if [[ -e "$output_directory" ]]; then
    echo "output directory already exists: $output_directory" >&2
    exit 64
fi

staging_directory=$(mktemp -d "${TMPDIR:-/tmp}/mlx-guard-envelope.XXXXXX")
cleanup() {
    status=$?
    if [[ $status -ne 0 ]]; then
        echo "partial capture retained at $staging_directory" >&2
    fi
}
trap cleanup EXIT

cd "$repo_root"
cargo build -p mlx-guard-test-support --bin mlx-guard-fixture
fixture="$repo_root/target/debug/mlx-guard-fixture"

MLX_GUARD_ENVELOPE_OUTPUT="$staging_directory/escalation-envelope.json" \
MLX_GUARD_ENVELOPE_PROFILE="$profile" \
MLX_GUARD_FIXTURE="$fixture" \
    cargo test -p mlx-guard-cli --test escalation_envelope \
    capture_escalation_envelope -- --exact --ignored --nocapture

mkdir -p "$(dirname "$output_directory")"
mv "$staging_directory" "$output_directory"
echo "captured $output_directory/escalation-envelope.json"
