#!/usr/bin/env bash
# Reference-host soak gate: runs each long endurance test as its own chunk, writing every chunk's
# JSON the moment it finishes, and resumes by skipping chunks whose JSON already exists.
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 OUTPUT_DIRECTORY" >&2
    echo "  MLX_GUARD_SOAK_REFERENCE_SECONDS overrides the 1800 s per-chunk duration (dry runs only)" >&2
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
    echo "the worktree and block a resume); write it elsewhere and copy the JSON files in" >&2
    exit 64
fi
seconds=${MLX_GUARD_SOAK_REFERENCE_SECONDS:-1800}
if ! [[ $seconds =~ ^[1-9][0-9]*$ ]] || (( seconds > 1800 )); then
    echo "MLX_GUARD_SOAK_REFERENCE_SECONDS must be an integer in 1..=1800, got: $seconds" >&2
    exit 64
fi

cd "$repo_root"
if [[ -n "$(git status --porcelain)" ]]; then
    echo "reference soak requires a clean worktree" >&2
    exit 65
fi

hardware=$(system_profiler SPHardwareDataType)
if ! grep -Eq '^ *Chip: Apple M1 Max$' <<<"$hardware" \
    || ! grep -Eq '^ *Memory: 32 GB$' <<<"$hardware"; then
    echo "reference soak requires an Apple M1 Max with 32 GB memory" >&2
    exit 69
fi

# The staging directory is deterministic so an interrupted run resumes; it is never a temp dir.
staging_directory="${output_directory}.staging"
log_directory="$staging_directory/logs"
mkdir -p "$log_directory"

# An interrupted chunk leaves its churn shell running in its own process group, where Ctrl-C on
# cargo never reaches it; end it here so a resume does not measure under leftover load.
on_interrupt() {
    echo "interrupted; ending churn processes" >&2
    pkill -f 'mlx-guard-fixture' 2>/dev/null || true
    pkill -f 'setsid-stall' 2>/dev/null || true
    pkill -f 'lt [0-9]+; do' 2>/dev/null || true
    exit 130
}
trap on_interrupt INT TERM
commit=$(git rev-parse HEAD)
provenance="$staging_directory/provenance.json"
if [[ -f "$provenance" ]]; then
    staged_commit=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["git_commit"])' "$provenance")
    if [[ "$staged_commit" != "$commit" ]]; then
        echo "staged chunks in $staging_directory belong to $staged_commit, not HEAD $commit" >&2
        echo "move that directory aside before soaking a different commit" >&2
        exit 65
    fi
    echo "[resume] reusing staged chunks in $staging_directory"
else
    sanitized_hardware=$(grep -E '^ *(Model Name|Model Identifier|Chip|Total Number of Cores|Memory):' \
        <<<"$hardware" | sed -E 's/^ *//')
    python3 - "$provenance" "$commit" "$seconds" "$sanitized_hardware" "$(sw_vers)" "$(uname -r)" \
        "$(uname -m)" "$(rustc --version)" <<'PY'
import json, sys, datetime
path, commit, seconds, hardware, macos, kernel, arch, rustc = sys.argv[1:]
json.dump({
    "schema_version": 1,
    "git_commit": commit,
    "git_status_porcelain": "",
    "captured_at_utc": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "requested_chunk_seconds": int(seconds),
    "hardware": hardware,
    "macos": macos,
    "kernel_release": kernel,
    "architecture": arch,
    "rustc": rustc,
    "build_profile": "debug",
}, open(path, "w"), indent=2)
PY
fi

cargo build -p mlx-guard-cli --bin mlx-guard
cargo build -p mlx-guard-test-support --bin mlx-guard-fixture

strays() {
    sleep 1
    ps -Ao pid,ppid,etime,command \
        | grep -E 'mlx-guard-fixture|target/debug/mlx-guard observe|setsid-stall' \
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

# run_chunk NAME OUTPUT_ENV SECONDS_ENV TEST_FILE TEST_NAME [EXTRA_ENV=VALUE ...]
# A chunk counts as done only when its success sentinel exists: every chunk test writes its JSON
# before judging its ceilings, so the artifact alone would let a failed chunk into the bundle.
run_chunk() {
    local name=$1 output_env=$2 seconds_env=$3 test_file=$4 test_name=$5
    shift 5
    local artifact="$staging_directory/$name.json"
    local sentinel="$log_directory/$name.ok"
    if [[ -f "$sentinel" && -f "$artifact" ]]; then
        local staged_seconds
        staged_seconds=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["requested_duration_seconds"])' "$artifact")
        if [[ "$staged_seconds" != "$seconds" ]]; then
            echo "chunk $name was staged for ${staged_seconds}s but this run wants ${seconds}s" >&2
            exit 65
        fi
        echo "[skip] $name (passed earlier at ${seconds}s)"
        return
    fi
    refuse_strays "before chunk $name"
    echo "[run] $name (${seconds}s) -> $artifact"
    rm -f "$artifact" "$sentinel"
    env "$output_env=$artifact" "$seconds_env=$seconds" "$@" \
        cargo test -p mlx-guard-test-support --test "$test_file" "$test_name" \
        -- --ignored --exact --nocapture 2>&1 | tee "$log_directory/$name.log"
    if [[ ! -f "$artifact" ]]; then
        echo "chunk $name finished without writing $artifact" >&2
        exit 70
    fi
    refuse_strays "after chunk $name"
    : >"$sentinel"
}

run_chunk escaping-churn MLX_GUARD_SOAK_OUTPUT MLX_GUARD_SOAK_SECONDS \
    soak escaping_churn_supervisor_footprint_stays_bounded
run_chunk real-binary MLX_GUARD_SOAK_BINARY_OUTPUT MLX_GUARD_SOAK_SECONDS \
    soak real_binary_supervising_escaping_churn_stays_bounded \
    "MLX_GUARD_SOAK_BINARY_REPORT=$log_directory/real-binary-report.json"
run_chunk endurance MLX_GUARD_ENDURANCE_OUTPUT MLX_GUARD_ENDURANCE_SECONDS \
    footprint_sampling thirty_minute_sampler_stays_inside_cpu_rss_and_history_bounds
run_chunk pid-churn MLX_GUARD_CHURN_OUTPUT MLX_GUARD_CHURN_SECONDS \
    footprint_sampling pid_churn_sampler_rss_stays_bounded_despite_many_distinct_children

# Only the JSON files are published for committing, and they must carry no unique machine
# identifier and no local path. The cargo transcripts and the copied schema report stay in the
# sibling logs directory: they name local paths by nature and are never committed.
if grep -El \
    -e 'Serial Number' -e 'Hardware UUID' -e 'Provisioning UDID' -e 'Activation Lock' \
    -e '[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}' \
    -e '/Users/' -e '/home/' -e '/var/folders/' -e "$HOME" \
    "$staging_directory"/*.json; then
    echo "staged artifacts contain a unique identifier or local path; not publishing" >&2
    exit 70
fi

mkdir -p "$(dirname "$output_directory")" "$output_directory"
mv "$staging_directory"/*.json "$output_directory"/
mv "$log_directory" "${output_directory}.logs"
rmdir "$staging_directory"
echo "soak bundle written to $output_directory (transcripts in ${output_directory}.logs)"
