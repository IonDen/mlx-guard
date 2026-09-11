#!/usr/bin/env bash
# Reference-host calibration: the same resumable bundle `scripts/calibrate-host.sh` produces on
# any Apple Silicon host, refused on anything but the M1 Max 32 GB reference machine so the
# bundle committed under evidence/<version>/m1-max-32gb/ always comes from that host.
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 OUTPUT_DIRECTORY" >&2
    exit 64
fi

repo_root=$(cd "$(dirname "$0")/.." && pwd -P)
profile=$(system_profiler -json SPHardwareDataType 2>/dev/null | "$repo_root/scripts/host-profile-label.sh" || true)
if [[ "$profile" != "m1-max-32gb" ]]; then
    echo "reference calibration requires an Apple M1 Max with 32 GB memory (this host: ${profile:-unknown})" >&2
    exit 69
fi

exec "$repo_root/scripts/calibrate-host.sh" "$1"
