#!/usr/bin/env bash
# Derive the evidence profile label for a host from `system_profiler SPHardwareDataType` text on
# stdin: the chip words after "Apple", lowercased, with every run of characters that is not a
# letter or digit collapsed to one "-", then "-<memory>gb". "Chip: Apple M1 Max" with
# "Memory: 32 GB" gives "m1-max-32gb"; "Chip: Apple M1 (Virtual)" with "Memory: 7 GB" gives
# "m1-virtual-7gb". Only the chip and memory lines are read, so the label can never carry a
# serial number, UUID, or UDID even when the raw record is piped in. A record with no
# "Chip: Apple" line (an Intel Mac) or no memory line is refused with exit 65.
set -euo pipefail

record=$(cat)
chip=$(sed -nE 's/^[[:space:]]*Chip:[[:space:]]*Apple[[:space:]]+(.+)$/\1/p' <<<"$record" | head -n 1)
memory=$(sed -nE 's/^[[:space:]]*Memory:[[:space:]]*([0-9]+)[[:space:]]*GB[[:space:]]*$/\1/p' <<<"$record" | head -n 1)

if [[ -z "$chip" ]]; then
    echo "no 'Chip: Apple ...' line in the hardware record; only Apple Silicon hosts are calibrated" >&2
    exit 65
fi
if [[ -z "$memory" ]]; then
    echo "no 'Memory: <N> GB' line in the hardware record; the label needs the memory size" >&2
    exit 65
fi

label=$(tr '[:upper:]' '[:lower:]' <<<"$chip" | sed -E 's/[^a-z0-9]+/-/g; s/^-+//; s/-+$//')
echo "${label}-${memory}gb"
