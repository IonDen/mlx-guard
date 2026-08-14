#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 OUTPUT_DIRECTORY" >&2
    exit 64
fi

repo_root=$(cd "$(dirname "$0")/.." && pwd)
output_directory=$1
if [[ -e "$output_directory" ]]; then
    echo "output directory already exists: $output_directory" >&2
    exit 64
fi

cd "$repo_root"
if [[ -n "$(git status --porcelain)" ]]; then
    echo "reference calibration requires a clean worktree" >&2
    exit 65
fi

hardware=$(system_profiler SPHardwareDataType)
if ! grep -Eq '^ *Chip: Apple M1 Max$' <<<"$hardware" \
    || ! grep -Eq '^ *Memory: 32 GB$' <<<"$hardware"; then
    echo "reference calibration requires an Apple M1 Max with 32 GB memory" >&2
    exit 69
fi

staging_directory=$(mktemp -d "${TMPDIR:-/tmp}/mlx-guard-calibration.XXXXXX")
fixture_build_directory=$(mktemp -d "${TMPDIR:-/tmp}/mlx-guard-metal-calibration.XXXXXX")
cleanup() {
    status=$?
    rm -rf -- "$fixture_build_directory"
    if [[ $status -ne 0 ]]; then
        echo "partial calibration retained at $staging_directory" >&2
    fi
}
trap cleanup EXIT

metal_fixture="$fixture_build_directory/metal-calibration"
"$repo_root/scripts/build-metal-calibration-fixture.sh" "$metal_fixture"
cargo build -p mlx-guard-test-support --bin mlx-guard-fixture
fixture="$repo_root/target/debug/mlx-guard-fixture"

MLX_GUARD_METAL_FIXTURE="$metal_fixture" \
MLX_GUARD_CALIBRATION_OUTPUT="$staging_directory/footprint.json" \
    cargo test -p mlx-guard-test-support --test reference_calibration \
    reference_host_footprint_measurements_write_raw_and_derived_json \
    -- --ignored --exact --nocapture

MLX_GUARD_FIXTURE="$fixture" \
MLX_GUARD_RUNTIME_CALIBRATION_OUTPUT="$staging_directory/runtime.json" \
    cargo test -p mlx-guard-cli --test reference_runtime_calibration \
    reference_host_runtime_measurements_write_raw_and_derived_json \
    -- --ignored --exact --nocapture

MLX_GUARD_INTERVENTION_OUTPUT="$staging_directory/intervention.json" \
    cargo test -p mlx-guard-test-support --test intervention_process \
    threshold_decision_to_first_signal_p95_stays_within_ten_milliseconds \
    -- --exact --nocapture

MLX_GUARD_FIXTURE="$fixture" \
MLX_GUARD_SCENARIO_OUTPUT_DIRECTORY="$staging_directory/scenarios" \
    cargo test -p mlx-guard-cli --test reference_runtime_calibration \
    reference_host_scenarios_write_reports_and_false_intervention_count \
    -- --ignored --exact --nocapture

MLX_GUARD_ENDURANCE_SECONDS=1800 \
MLX_GUARD_ENDURANCE_OUTPUT="$staging_directory/endurance.json" \
    cargo test -p mlx-guard-test-support --test footprint_sampling \
    thirty_minute_sampler_stays_inside_cpu_rss_and_history_bounds \
    -- --ignored --exact --nocapture

mkdir -p "$(dirname "$output_directory")"
mv "$staging_directory" "$output_directory"
