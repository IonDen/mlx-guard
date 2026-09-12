#!/usr/bin/env bash
# Derive the evidence profile label for a host from `system_profiler -json SPHardwareDataType`
# on stdin: the chip words after "Apple", lowercased, with every run of characters that is not a
# letter or digit collapsed to one "-", then "-<memory>gb". A chip of "Apple M1 Max" with
# "32 GB" gives "m1-max-32gb"; "Apple M1 (Virtual)" with "7 GB" gives "m1-virtual-7gb". The JSON
# keys do not change with the user's language, unlike the text report. Only the chip and memory
# fields are read, so the label can never carry a serial number or UUID even though the document
# holds both. A document with no "Apple ..." chip (an Intel Mac), no memory size, a chip that
# leaves no letters or digits, or that is not the expected JSON is refused with exit 65.
set -euo pipefail

document=$(cat)
python3 - "$document" <<'PY'
import json
import re
import sys

try:
    document = json.loads(sys.argv[1])
    record = document["SPHardwareDataType"][0]
except (ValueError, KeyError, IndexError, TypeError):
    print("stdin is not a system_profiler -json SPHardwareDataType document", file=sys.stderr)
    sys.exit(65)

chip = str(record.get("chip_type", ""))
if not chip.startswith("Apple "):
    print(
        "no 'Apple ...' chip in the hardware record; only Apple Silicon hosts are calibrated",
        file=sys.stderr,
    )
    sys.exit(65)
memory = re.fullmatch(r"\s*([0-9]+)\s*GB\s*", str(record.get("physical_memory", "")))
if memory is None:
    print("no '<N> GB' memory size in the hardware record; the label needs it", file=sys.stderr)
    sys.exit(65)

words = re.sub(r"[^a-z0-9]+", "-", chip[len("Apple ") :].lower()).strip("-")
if not words:
    print("the chip name leaves no letters or digits for a label", file=sys.stderr)
    sys.exit(65)
print(f"{words}-{memory.group(1)}gb")
PY
