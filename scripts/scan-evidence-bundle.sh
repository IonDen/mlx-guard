#!/usr/bin/env bash
# Refuse to publish an evidence bundle that carries a machine identifier or a local path.
#
#   scan-evidence-bundle.sh [--exclude-dir NAME ...] DIRECTORY [LITERAL ...]
#
# Every file under DIRECTORY (dotfiles and binary journals included) is searched for the
# identifier labels system_profiler prints, for a UUID-shaped value, for the usual local-path
# prefixes, for the invoking user's home directory, and for each extra LITERAL the caller knows
# about (the serial number and UUIDs read from the hardware record, the host name, the checkout
# path). Literals are matched as fixed strings, never as regular expressions, so a bracket in a
# home directory cannot turn the scan into a syntax error that reads as "clean". Exit 0 means no
# file matched; exit 70 lists the offending files (or names the error that stopped the scan).
set -euo pipefail

excludes=()
while [[ $# -ge 2 && "$1" == "--exclude-dir" ]]; do
    excludes+=("--exclude-dir=$2")
    shift 2
done
if [[ $# -lt 1 ]]; then
    echo "usage: $0 [--exclude-dir NAME ...] DIRECTORY [LITERAL ...]" >&2
    exit 64
fi
directory=$1
shift
if [[ ! -d "$directory" ]]; then
    echo "not a directory: $directory" >&2
    exit 70
fi

literals=('Serial Number' 'Hardware UUID' 'Provisioning UDID' 'Activation Lock'
    '/Users/' '/home/' '/var/folders/' "${HOME:-}" "$@")
fixed=()
for literal in "${literals[@]}"; do
    # An empty literal would match every file; it comes from a field the host did not report.
    [[ -n "$literal" ]] && fixed+=(-e "$literal")
done

# grep -r visits dotfiles and reads journals as text (-a); exit 0 is a match, 1 is none, and
# anything else, or any complaint on stderr, is a failure of the scan itself, which must not pass
# as clean (a shell error inside the substitution also exits 1, so the exit code alone is not
# enough to tell "nothing matched" from "the scan did not run").
scan_errors=$(mktemp "${TMPDIR:-/tmp}/mlx-guard-scan.XXXXXX")
trap 'rm -f "$scan_errors"' EXIT
status=0
hits=$(grep -rlaF ${excludes[@]+"${excludes[@]}"} "${fixed[@]}" -- "$directory" 2>"$scan_errors") || status=$?
if [[ $status -gt 1 || -s "$scan_errors" ]]; then
    echo "identifier scan failed (grep exit $status): $(cat "$scan_errors"); not publishing" >&2
    exit 70
fi
uuid_status=0
uuid_hits=$(grep -rlaE ${excludes[@]+"${excludes[@]}"} \
    -e '[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}' \
    -- "$directory" 2>"$scan_errors") || uuid_status=$?
if [[ $uuid_status -gt 1 || -s "$scan_errors" ]]; then
    echo "identifier scan failed (grep exit $uuid_status): $(cat "$scan_errors"); not publishing" >&2
    exit 70
fi

if [[ -n "$hits" || -n "$uuid_hits" ]]; then
    echo "files carrying a machine identifier or local path; not publishing:" >&2
    printf '%s\n' "$hits" "$uuid_hits" | sed '/^$/d' | sort -u
    # Name what matched so a false positive (a host name that is also an ordinary word) can be
    # told from a leak; this goes to the operator's terminal, never into the bundle.
    for literal in "${literals[@]}"; do
        [[ -n "$literal" ]] || continue
        if grep -rqaF ${excludes[@]+"${excludes[@]}"} -e "$literal" -- "$directory"; then
            echo "  matched literal: $literal" >&2
        fi
    done
    [[ -n "$uuid_hits" ]] && echo "  matched: a UUID-shaped value" >&2
    exit 70
fi
