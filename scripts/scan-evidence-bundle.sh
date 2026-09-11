#!/usr/bin/env bash
# Refuse to publish an evidence bundle that carries a machine identifier or a local path.
#
#   scan-evidence-bundle.sh DIRECTORY [LITERAL ...]
#
# Every file under DIRECTORY (dotfiles and binary journals included) is searched for the
# identifier labels system_profiler prints, for a UUID-shaped value, for the usual local-path
# prefixes, for the invoking user's home directory, and for each extra LITERAL the caller knows
# about (the serial number and UUIDs read from the hardware record, the host name, the checkout
# path). Literals are matched as fixed strings, never as regular expressions, so a bracket in a
# home directory cannot turn the scan into a syntax error that reads as "clean". Exit 0 means no
# file matched; exit 70 lists the offending files (or names the error that stopped the scan).
set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "usage: $0 DIRECTORY [LITERAL ...]" >&2
    exit 64
fi
directory=$1
shift
if [[ ! -d "$directory" ]]; then
    echo "not a directory: $directory" >&2
    exit 70
fi

fixed=(
    -e 'Serial Number' -e 'Hardware UUID' -e 'Provisioning UDID' -e 'Activation Lock'
    -e '/Users/' -e '/home/' -e '/var/folders/' -e "$HOME"
)
for literal in "$@"; do
    # An empty literal would match every file; it comes from a field the host did not report.
    [[ -n "$literal" ]] && fixed+=(-e "$literal")
done

# grep -r visits dotfiles and reads journals as text (-a); exit 0 is a match, 1 is none, and
# anything else is a failure of the scan itself, which must not pass as clean.
status=0
hits=$(grep -rlaF "${fixed[@]}" -- "$directory") || status=$?
if [[ $status -gt 1 ]]; then
    echo "identifier scan failed (grep exit $status); not publishing" >&2
    exit 70
fi
uuid_status=0
uuid_hits=$(grep -rlaE \
    -e '[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}' \
    -- "$directory") || uuid_status=$?
if [[ $uuid_status -gt 1 ]]; then
    echo "identifier scan failed (grep exit $uuid_status); not publishing" >&2
    exit 70
fi

if [[ -n "$hits" || -n "$uuid_hits" ]]; then
    echo "files carrying a machine identifier or local path; not publishing:" >&2
    printf '%s\n' "$hits" "$uuid_hits" | sed '/^$/d' | sort -u
    exit 70
fi
