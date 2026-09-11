#!/usr/bin/env bash
# One-command calibration bundle for any Apple Silicon host. Runs each calibration measurement as
# its own chunk, writes every chunk's artifact the moment it finishes, and resumes an interrupted
# run by skipping chunks that already passed. The bundle's profile label (for example
# `m1-max-32gb`) is derived from the sanitized hardware record; the published files carry no
# machine identifier and no local path.
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 OUTPUT_DIRECTORY" >&2
    echo "  MLX_GUARD_CALIBRATION_ENDURANCE_SECONDS overrides the 1800 s endurance chunk (dry runs only)" >&2
    exit 64
fi

repo_root=$(cd "$(dirname "$0")/.." && pwd)
output_directory=$1
if [[ -e "$output_directory" ]]; then
    echo "output directory already exists: $output_directory" >&2
    exit 64
fi
absolute_output=$(python3 -c 'import os, sys; print(os.path.abspath(sys.argv[1]))' "$output_directory")
if [[ "$absolute_output" == "$repo_root" || "$absolute_output" == "$repo_root"/* ]]; then
    echo "output directory must be outside the repository (its staging directory would dirty" >&2
    echo "the worktree and block a resume); write it elsewhere and copy the bundle in" >&2
    exit 64
fi
endurance_seconds=${MLX_GUARD_CALIBRATION_ENDURANCE_SECONDS:-1800}
if ! [[ $endurance_seconds =~ ^[1-9][0-9]*$ ]] || (( endurance_seconds > 1800 )); then
    echo "MLX_GUARD_CALIBRATION_ENDURANCE_SECONDS must be an integer in 1..=1800, got: $endurance_seconds" >&2
    exit 64
fi

cd "$repo_root"
if [[ -n "$(git status --porcelain)" ]]; then
    echo "calibration requires a clean worktree so the bundle names one exact commit" >&2
    exit 65
fi
if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
    echo "calibration runs only on macOS on Apple Silicon (arm64)" >&2
    exit 69
fi

hardware=$(system_profiler SPHardwareDataType 2>/dev/null)
profile=$(scripts/host-profile-label.sh <<<"$hardware")
sanitized_hardware=$(grep -E '^ *(Model Name|Model Identifier|Chip|Total Number of Cores|Memory):' \
    <<<"$hardware" | sed -E 's/^ *//')

# The staging directory is deterministic so an interrupted run resumes; it is never a temp dir.
staging_directory="${output_directory}.staging"
log_directory="$staging_directory/logs"
build_directory="$staging_directory/build"
mkdir -p "$log_directory" "$build_directory"

# An interrupted chunk can leave fixture processes behind; end them so a resume does not measure
# under leftover load. Nothing else on the machine matches these names.
on_interrupt() {
    echo "interrupted; ending fixture processes" >&2
    pkill -f 'mlx-guard-fixture' 2>/dev/null || true
    pkill -f 'metal-calibration' 2>/dev/null || true
    exit 130
}
trap on_interrupt INT TERM

commit=$(git rev-parse HEAD)
provenance="$staging_directory/provenance.json"
if [[ -f "$provenance" ]]; then
    staged_commit=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["git_commit"])' "$provenance")
    if [[ "$staged_commit" != "$commit" ]]; then
        echo "staged chunks in $staging_directory belong to $staged_commit, not HEAD $commit" >&2
        echo "move that directory aside before calibrating a different commit" >&2
        exit 65
    fi
    staged_seconds=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["requested_endurance_seconds"])' "$provenance")
    if [[ "$staged_seconds" != "$endurance_seconds" ]]; then
        echo "staged chunks were run with a ${staged_seconds}s endurance chunk but this run wants ${endurance_seconds}s" >&2
        exit 65
    fi
    echo "[resume] reusing staged chunks in $staging_directory"
else
    python3 - "$provenance" "$commit" "$profile" "$endurance_seconds" "$sanitized_hardware" \
        "$(sw_vers)" "$(uname -r)" "$(uname -m)" "$(rustc --version)" <<'PY'
import json, sys, datetime
path, commit, profile, seconds, hardware, macos, kernel, arch, rustc = sys.argv[1:]
json.dump({
    "schema_version": 1,
    "git_commit": commit,
    "git_status_porcelain": "",
    "captured_at_utc": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "profile": profile,
    "requested_endurance_seconds": int(seconds),
    "hardware": hardware,
    "macos": macos,
    "kernel_release": kernel,
    "architecture": arch,
    "rustc": rustc,
    "build_profile": "debug",
}, open(path, "w"), indent=2)
PY
fi
echo "[profile] $profile"

metal_fixture="$build_directory/metal-calibration"
"$repo_root/scripts/build-metal-calibration-fixture.sh" "$metal_fixture"
cargo build -p mlx-guard-cli --bin mlx-guard
cargo build -p mlx-guard-test-support --bin mlx-guard-fixture
fixture="$repo_root/target/debug/mlx-guard-fixture"

strays() {
    sleep 1
    ps -Ao pid,ppid,etime,command \
        | grep -E 'mlx-guard-fixture|target/debug/mlx-guard (observe|run)|metal-calibration' \
        | grep -v grep || true
}

refuse_strays() {
    local when=$1 leftovers
    leftovers=$(strays)
    if [[ -n "$leftovers" ]]; then
        echo "processes still alive $when:" >&2
        echo "$leftovers" >&2
        exit 70
    fi
}

# chunk_done NAME ARTIFACT — a chunk counts as done only when its success sentinel exists: the
# chunk tests write their JSON before judging their bounds, so the artifact alone would let a
# failed chunk into the bundle.
chunk_done() {
    local name=$1 artifact=$2
    if [[ -f "$log_directory/$name.ok" && -e "$artifact" ]]; then
        echo "[skip] $name (passed earlier)"
        return 0
    fi
    return 1
}

# run_chunk NAME ARTIFACT COMMAND... — runs COMMAND (an `env VAR=... cargo test ...` line) with
# its transcript tee'd to the logs, then requires the artifact and a clean process table.
run_chunk() {
    local name=$1 artifact=$2
    shift 2
    if chunk_done "$name" "$artifact"; then
        return
    fi
    refuse_strays "before chunk $name"
    echo "[run] $name -> $artifact"
    rm -rf "$artifact" "$log_directory/$name.ok"
    "$@" 2>&1 | tee "$log_directory/$name.log"
    if [[ ! -e "$artifact" ]]; then
        echo "chunk $name finished without writing $artifact" >&2
        exit 70
    fi
    refuse_strays "after chunk $name"
    : >"$log_directory/$name.ok"
}

run_chunk footprint "$staging_directory/footprint.json" \
    env MLX_GUARD_METAL_FIXTURE="$metal_fixture" \
        MLX_GUARD_CALIBRATION_OUTPUT="$staging_directory/footprint.json" \
        cargo test -p mlx-guard-test-support --test reference_calibration \
        reference_host_footprint_measurements_write_raw_and_derived_json \
        -- --ignored --exact --nocapture

run_chunk runtime "$staging_directory/runtime.json" \
    env MLX_GUARD_FIXTURE="$fixture" \
        MLX_GUARD_RUNTIME_CALIBRATION_OUTPUT="$staging_directory/runtime.json" \
        cargo test -p mlx-guard-cli --test reference_runtime_calibration \
        reference_host_runtime_measurements_write_raw_and_derived_json \
        -- --ignored --exact --nocapture

run_chunk intervention "$staging_directory/intervention.json" \
    env MLX_GUARD_INTERVENTION_OUTPUT="$staging_directory/intervention.json" \
        cargo test -p mlx-guard-test-support --test intervention_process \
        threshold_decision_to_first_signal_p95_stays_within_ten_milliseconds \
        -- --exact --nocapture

run_chunk scenarios "$staging_directory/scenarios/scenarios.json" \
    env MLX_GUARD_FIXTURE="$fixture" \
        MLX_GUARD_SCENARIO_OUTPUT_DIRECTORY="$staging_directory/scenarios" \
        cargo test -p mlx-guard-cli --test reference_runtime_calibration \
        reference_host_scenarios_write_reports_and_false_intervention_count \
        -- --ignored --exact --nocapture

run_chunk endurance "$staging_directory/endurance.json" \
    env MLX_GUARD_ENDURANCE_SECONDS="$endurance_seconds" \
        MLX_GUARD_ENDURANCE_OUTPUT="$staging_directory/endurance.json" \
        cargo test -p mlx-guard-test-support --test footprint_sampling \
        thirty_minute_sampler_stays_inside_cpu_rss_and_history_bounds \
        -- --ignored --exact --nocapture

run_chunk escalation-envelope "$staging_directory/escalation-envelope.json" \
    env MLX_GUARD_ENVELOPE_OUTPUT="$staging_directory/escalation-envelope.json" \
        MLX_GUARD_ENVELOPE_PROFILE="$profile" \
        MLX_GUARD_FIXTURE="$fixture" \
        cargo test -p mlx-guard-cli --test escalation_envelope \
        capture_escalation_envelope \
        -- --ignored --exact --nocapture

# Everything published must carry no unique machine identifier and no local path: the JSON
# files, the scenario reports, and their journals alike. The cargo transcripts stay in the
# sibling logs directory; they name local paths by nature and are never committed.
if find "$staging_directory" -path "$log_directory" -prune -o -path "$build_directory" -prune \
        -o -type f -print0 \
    | xargs -0 grep -alE \
        -e 'Serial Number' -e 'Hardware UUID' -e 'Provisioning UDID' -e 'Activation Lock' \
        -e '[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}' \
        -e '/Users/' -e '/home/' -e '/var/folders/' -e "$HOME"; then
    echo "staged artifacts contain a unique identifier or local path; not publishing" >&2
    exit 70
fi

rm -rf "$build_directory"
mkdir -p "$(dirname "$output_directory")" "$output_directory"
mv "$staging_directory"/*.json "$staging_directory/scenarios" "$output_directory"/
mv "$log_directory" "${output_directory}.logs"
rmdir "$staging_directory"
echo "calibration bundle for $profile written to $output_directory (transcripts in ${output_directory}.logs)"
