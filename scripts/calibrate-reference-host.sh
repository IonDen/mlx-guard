#!/usr/bin/env bash
# Reference-host calibration: the same resumable bundle `scripts/calibrate-host.sh` produces on
# any Apple Silicon host, refused on anything but the M1 Max 32 GB reference machine so the
# bundle committed under evidence/<version>/m1-max-32gb/ always comes from that host.
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 OUTPUT_DIRECTORY" >&2
    exit 64
fi

repo_root=$(cd "$(dirname "$0")/.." && pwd)
hardware=$(system_profiler SPHardwareDataType 2>/dev/null)
if ! grep -Eq '^ *Chip: Apple M1 Max$' <<<"$hardware" \
    || ! grep -Eq '^ *Memory: 32 GB$' <<<"$hardware"; then
    echo "reference calibration requires an Apple M1 Max with 32 GB memory" >&2
    exit 69
fi

exec "$repo_root/scripts/calibrate-host.sh" "$1"
