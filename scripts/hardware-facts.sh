#!/usr/bin/env bash
# Print the hardware facts the calibration provenance records: `sanitized` prints the five
# allowlisted facts, `identifiers` the host identifiers the publish scan must never find.
# Reads on stdin the `system_profiler -json SPHardwareDataType` document, one NUL byte, then the
# English `system_profiler SPHardwareDataType` text report. Only the mode is an argument, so no
# host identifier appears in any process's argv.
set -euo pipefail

if [[ $# -ne 1 || ( "$1" != "sanitized" && "$1" != "identifiers" ) ]]; then
    echo "usage: $0 sanitized|identifiers < system_profiler.json NUL text_report" >&2
    exit 64
fi
read -r -d '' program <<'PY' || true
import json, re, sys
mode = sys.argv[1]
document, separator, text = sys.stdin.buffer.read().decode("utf-8").partition("\0")
if not separator:
    print("hardware-facts.sh: stdin must hold the JSON document, a NUL byte, then the text report",
          file=sys.stderr)
    sys.exit(65)
record = json.loads(document)["SPHardwareDataType"][0]
labels = [("Model Name", "machine_name"), ("Model Identifier", "machine_model"), ("Chip", "chip_type"),
          ("Total Number of Cores", "number_processors"), ("Memory", "physical_memory")]
# number_processors is a colon-separated "proc ..." code whose field order changed between macOS
# releases ("proc 10:8:2:0" on 26, "proc 10:0:8:2" on 27 for the same 8P+2E chip), so the order
# cannot be decoded. The core count comes from the text report's line instead; without that line
# the JSON value is recorded as given, uninterpreted.
cores = re.search(r"^[ \t]*Total Number of Cores:[ \t]*(\S.*?)[ \t]*$", text, re.MULTILINE)
def render(key, value):
    if key == "number_processors" and cores is not None:
        return cores.group(1)
    return str(value)
if mode == "sanitized":
    print("\n".join(f"{label}: {render(key, record[key])}" for label, key in labels if key in record))
else:
    for key in ("serial_number", "platform_UUID", "provisioning_UDID"):
        print(record.get(key, ""))
PY
python3 -c "$program" "$1"
