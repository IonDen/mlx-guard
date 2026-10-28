"""Tests for the hardware facts the one-command host calibration records in its provenance."""

from __future__ import annotations

import json
import os
import shlex
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

# Resolved from this file, not the CWD: the wheel proof runs the suite from a temp directory.
SCRIPT = Path(__file__).resolve().parents[2] / "scripts/hardware-facts.sh"

CORES_LINE = "Total Number of Cores: 10 (8 Performance and 2 Efficiency)"


def document(number_processors: str) -> str:
    """Build the `system_profiler -json SPHardwareDataType` document of the reference host."""
    return json.dumps(
        {
            "SPHardwareDataType": [
                {
                    "_name": "hardware_overview",
                    "machine_name": "MacBook Pro",
                    "machine_model": "MacBookPro18,2",
                    "model_number": "SECRET-SKU",
                    "chip_type": "Apple M1 Max",
                    "number_processors": number_processors,
                    "physical_memory": "32 GB",
                    "serial_number": "SECRET-SERIAL",
                    "platform_UUID": "SECRET-UUID",
                    "provisioning_UDID": "SECRET-UDID",
                }
            ]
        }
    )


def text_report(cores_line: str | None = CORES_LINE) -> str:
    """Build the English `system_profiler SPHardwareDataType` text report of the same host."""
    lines = [
        "Hardware:",
        "",
        "    Hardware Overview:",
        "",
        "      Model Name: MacBook Pro",
        "      Model Identifier: MacBookPro18,2",
        "      Model Number: SECRET-SKU",
        "      Chip: Apple M1 Max",
    ]
    if cores_line is not None:
        lines.append(f"      {cores_line}")
    lines += [
        "      Memory: 32 GB",
        "      Serial Number (system): SECRET-SERIAL",
        "      Hardware UUID: SECRET-UUID",
        "      Provisioning UDID: SECRET-UDID",
        "",
    ]
    return "\n".join(lines)


def facts(
    mode: str, json_document: str, report: str, env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    """Run the renderer with only the mode in argv; the JSON, a NUL, then the text on stdin."""
    return subprocess.run(
        [str(SCRIPT), mode],
        input=f"{json_document}\0{report}",
        capture_output=True,
        text=True,
        check=False,
        timeout=30,
        env=env,
    )


def expected_facts(cores: str) -> str:
    """The five allowlisted facts, in the order the provenance record lists them."""
    return (
        "Model Name: MacBook Pro\n"
        "Model Identifier: MacBookPro18,2\n"
        "Chip: Apple M1 Max\n"
        f"Total Number of Cores: {cores}\n"
        "Memory: 32 GB\n"
    )


class HardwareFactsTests(unittest.TestCase):
    """Each test names the one-line defect that would turn it red."""

    def test_macos_26_core_encoding_renders_the_text_report_value(self) -> None:
        # Red on any decoding of the JSON code: no field order of `proc 10:8:2:0` yields
        # 7 and 3, so this value can only come from the text report's line.
        report = text_report("Total Number of Cores: 10 (7 Performance and 3 Efficiency)")
        completed = facts("sanitized", document("proc 10:8:2:0"), report)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(completed.stdout, expected_facts("10 (7 Performance and 3 Efficiency)"))

    def test_macos_27_core_encoding_renders_eight_performance_and_two_efficiency(self) -> None:
        # Red on the field-order parse `counts[1] Performance and counts[2] Efficiency`: macOS 27
        # reports `proc 10:0:8:2`, which that parse renders as "0 Performance and 8 Efficiency".
        completed = facts("sanitized", document("proc 10:0:8:2"), text_report())
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(completed.stdout, expected_facts("10 (8 Performance and 2 Efficiency)"))

    def test_missing_text_cores_line_keeps_the_raw_value_uninterpreted(self) -> None:
        # Red if a fallback guesses which field of the four-part encoding means what: without
        # the text report's line the value is recorded exactly as the JSON gives it.
        completed = facts("sanitized", document("proc 10:0:8:2"), text_report(cores_line=None))
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(completed.stdout, expected_facts("proc 10:0:8:2"))

    def test_sanitized_facts_carry_no_identifier_from_the_text_report(self) -> None:
        # Red if more of the text report than the cores line is copied into the provenance:
        # the report holds the serial number and hardware UUID.
        completed = facts("sanitized", document("proc 10:0:8:2"), text_report())
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertNotIn("SECRET", completed.stdout)

    def test_identifiers_mode_lists_the_three_host_identifiers(self) -> None:
        # Red if any identifier drops out of the list the publish scan searches the bundle for.
        completed = facts("identifiers", document("proc 10:0:8:2"), text_report())
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(completed.stdout, "SECRET-SERIAL\nSECRET-UUID\nSECRET-UDID\n")

    def test_text_report_in_argv_is_refused(self) -> None:
        # Red if the renderer accepts the text report as an argument again: argv is visible to
        # every local user through `ps`, and the report holds the serial number.
        completed = subprocess.run(
            [str(SCRIPT), "sanitized", text_report()],
            input=document("proc 10:0:8:2"),
            capture_output=True,
            text=True,
            check=False,
            timeout=30,
        )
        self.assertEqual(completed.returncode, 64)
        self.assertEqual(completed.stdout, "")

    def test_input_without_the_separator_is_refused(self) -> None:
        # Red if the renderer guesses where the JSON ends instead of requiring the NUL.
        completed = subprocess.run(
            [str(SCRIPT), "sanitized"],
            input=document("proc 10:0:8:2"),
            capture_output=True,
            text=True,
            check=False,
            timeout=30,
        )
        self.assertEqual(completed.returncode, 65)
        self.assertEqual(completed.stdout, "")
        self.assertIn("NUL", completed.stderr)

    def test_no_host_identifier_reaches_the_interpreter_argv(self) -> None:
        # Red if the script hands the JSON document or the text report to `python3` as an
        # argument: a `python3` first on PATH records its argv, then runs the real interpreter.
        with tempfile.TemporaryDirectory() as scratch:
            log = Path(scratch, "argv.log")
            shim = Path(scratch, "python3")
            shim.write_text(
                "#!/bin/bash\n"
                f"printf '%s\\n' \"$@\" >> {shlex.quote(str(log))}\n"
                f'exec {shlex.quote(sys.executable)} "$@"\n',
                encoding="utf-8",
            )
            shim.chmod(0o700)
            env = dict(os.environ, PATH=f"{scratch}{os.pathsep}{os.environ.get('PATH', '')}")
            for mode in ("sanitized", "identifiers"):
                completed = facts(mode, document("proc 10:0:8:2"), text_report(), env=env)
                self.assertEqual(completed.returncode, 0, completed.stderr)
            recorded = log.read_text(encoding="utf-8")
        self.assertIn("identifiers", recorded)
        self.assertNotIn("SECRET", recorded)


if __name__ == "__main__":
    unittest.main()
