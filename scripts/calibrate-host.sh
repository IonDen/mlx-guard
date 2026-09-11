#!/usr/bin/env bash
# One-command calibration bundle for any Apple Silicon host. Runs each calibration measurement as
# its own chunk, writes every chunk's artifact the moment it finishes, and resumes an interrupted
# run by skipping chunks that already passed. The bundle's profile label (for example
# `m1-max-32gb`) is derived from the hardware record; the published files carry no machine
# identifier and no local path (scripts/scan-evidence-bundle.sh is the gate).
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 OUTPUT_DIRECTORY" >&2
    echo "  MLX_GUARD_CALIBRATION_ENDURANCE_SECONDS overrides the 1800 s endurance chunk (dry runs only)" >&2
    echo "  MLX_GUARD_CALIBRATION_CHECK_ARGUMENTS_ONLY=1 validates the arguments and exits without building" >&2
    exit 64
fi

repo_root=$(cd "$(dirname "$0")/.." && pwd -P)
output_directory=$1
if [[ -e "$output_directory" ]]; then
    echo "output directory already exists: $output_directory" >&2
    exit 64
fi
if [[ -e "${output_directory}.logs" ]]; then
    echo "transcript directory already exists: ${output_directory}.logs (move the earlier run's logs aside)" >&2
    exit 64
fi
absolute_output=$(python3 -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "$output_directory")
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
if [[ "${MLX_GUARD_CALIBRATION_CHECK_ARGUMENTS_ONLY:-0}" == "1" ]]; then
    echo "arguments accepted: output $absolute_output, endurance ${endurance_seconds}s"
    exit 0
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

if ! hardware_json=$(system_profiler -json SPHardwareDataType 2>&1); then
    echo "system_profiler failed: $hardware_json" >&2
    exit 69
fi
profile=$(scripts/host-profile-label.sh <<<"$hardware_json")
# The five allowlisted hardware facts, rendered from the language-independent JSON keys, and the
# identifiers the host reports, which the publish scan must never find in the bundle.
hardware_facts() {
    python3 -c '
import json, sys
record = json.load(sys.stdin)["SPHardwareDataType"][0]
labels = [("Model Name", "machine_name"), ("Model Identifier", "machine_model"), ("Chip", "chip_type"),
          ("Cores", "number_processors"), ("Memory", "physical_memory")]
if sys.argv[1] == "sanitized":
    print("\n".join(f"{label}: {record[key]}" for label, key in labels if key in record))
else:
    for key in ("serial_number", "platform_UUID", "provisioning_UDID"):
        print(record.get(key, ""))
' "$1" <<<"$hardware_json"
}
sanitized_hardware=$(hardware_facts sanitized)
host_identifiers=()
while IFS= read -r line; do host_identifiers+=("$line"); done < <(hardware_facts identifiers)
host_identifiers+=("$(scutil --get LocalHostName 2>/dev/null || true)" "$repo_root" "$absolute_output")

# The staging directory is deterministic so an interrupted run resumes; it is never a temp dir.
staging_directory="${output_directory}.staging"
log_directory="$staging_directory/logs"
build_directory="$staging_directory/build"
mkdir -p "$log_directory" "$build_directory"
metal_fixture="$build_directory/metal-calibration"
fixture="$repo_root/target/debug/mlx-guard-fixture"
guard="$repo_root/target/debug/mlx-guard"

# An interrupted chunk can leave fixture processes behind; end them so a resume does not measure
# under leftover load. The patterns are anchored to the binaries this run builds, so an editor
# with a similarly named file open is never matched.
on_interrupt() {
    echo "interrupted; ending fixture processes" >&2
    pkill -f "^$fixture" 2>/dev/null || true
    pkill -f "^$metal_fixture" 2>/dev/null || true
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
if (( endurance_seconds < 1800 )); then
    echo "[dry run] endurance chunk shortened to ${endurance_seconds}s; this bundle is not publishable"
fi

"$repo_root/scripts/build-metal-calibration-fixture.sh" "$metal_fixture"
cargo build -p mlx-guard-cli --bin mlx-guard
cargo build -p mlx-guard-test-support --bin mlx-guard-fixture

strays() {
    ps -Ao pid,ppid,etime,command \
        | grep -E "^ *[0-9]+ +[0-9]+ +[^ ]+ +($fixture|$metal_fixture|$guard (observe|run))( |$)" \
        || true
}

# refuse_strays WHEN — a fixture may take a moment to be reaped after its test returns, so poll
# up to the fixtures' own 10 s wall ceiling before treating a survivor as a failure.
refuse_strays() {
    local when=$1 leftovers attempt
    for attempt in 1 2 3 4 5 6 7 8 9 10; do
        leftovers=$(strays)
        [[ -z "$leftovers" ]] && return 0
        sleep 1
    done
    echo "processes still alive $when:" >&2
    echo "$leftovers" >&2
    exit 70
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
# its transcript tee'd to the logs, then requires the artifact and a clean process table. The
# artifact is what a retry removes first: a file for most chunks, the whole output directory for
# the scenarios chunk, whose test refuses to run into an existing directory.
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
    : >"$log_directory/$name.ok"
    refuse_strays "after chunk $name"
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

run_chunk scenarios "$staging_directory/scenarios" \
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

# Everything published must carry no machine identifier and no local path: the JSON files, the
# scenario reports, and their journals alike. The cargo transcripts stay in the sibling logs
# directory; they name local paths by nature and are never committed.
rm -rf "$build_directory"
mv "$log_directory" "${output_directory}.logs"
"$repo_root/scripts/scan-evidence-bundle.sh" "$staging_directory" "${host_identifiers[@]}"

mkdir -p "$(dirname "$output_directory")"
mv "$staging_directory" "$output_directory"
if (( endurance_seconds < 1800 )); then
    echo "[dry run] bundle for $profile written to $output_directory with a ${endurance_seconds}s endurance chunk; not publishable"
else
    echo "calibration bundle for $profile written to $output_directory (transcripts in ${output_directory}.logs)"
fi
