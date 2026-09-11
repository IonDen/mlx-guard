"""Tests for the evidence profile-label derivation used by the one-command host calibration."""

from __future__ import annotations

import json
import subprocess
import unittest
from pathlib import Path

# Resolved from this file, not the CWD: the wheel proof runs the suite from a temp directory.
SCRIPT = Path(__file__).resolve().parents[2] / "scripts/host-profile-label.sh"


def record(**fields: str) -> str:
    """Build the `system_profiler -json SPHardwareDataType` document for one hardware record."""
    return json.dumps({"SPHardwareDataType": [{"_name": "hardware_overview", **fields}]})


REFERENCE_HOST = record(
    machine_name="MacBook Pro",
    machine_model="MacBookPro18,2",
    model_number="SECRET-SKU",
    chip_type="Apple M1 Max",
    number_processors="proc 10:8:2:0",
    physical_memory="32 GB",
    serial_number="SECRET-SERIAL",
    platform_UUID="SECRET-UUID",
    provisioning_UDID="SECRET-UDID",
)

SHARED_VM = record(
    machine_name="Apple Virtual Machine 1",
    machine_model="VirtualMac2,1",
    chip_type="Apple M1 (Virtual)",
    number_processors="3",
    physical_memory="7 GB",
    serial_number="SECRET-SERIAL",
    platform_UUID="SECRET-UUID",
)

INTEL_HOST = record(
    machine_name="MacBook Pro",
    cpu_type="8-Core Intel Core i9",
    physical_memory="32 GB",
)


def derive(document: str) -> subprocess.CompletedProcess[str]:
    """Feed one hardware document to the script and return the completed process."""
    return subprocess.run(
        [str(SCRIPT)],
        input=document,
        capture_output=True,
        text=True,
        check=False,
    )


class HostProfileLabelTests(unittest.TestCase):
    """Each test names the one-line defect that would turn it red."""

    def test_reference_host_record_maps_to_m1_max_32gb(self) -> None:
        # Red if the chip words are not lowercased, the "Apple" prefix is kept, or the memory
        # suffix is dropped: the label must match the committed bundle directory name.
        completed = derive(REFERENCE_HOST)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(completed.stdout, "m1-max-32gb\n")

    def test_virtual_machine_chip_keeps_its_qualifier(self) -> None:
        # Red if parentheses are stripped without becoming a separator, which would fold a
        # virtual machine into the bare-chip row of the matrix.
        completed = derive(SHARED_VM)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(completed.stdout, "m1-virtual-7gb\n")

    def test_output_carries_no_identifier_from_the_virtual_machine_record(self) -> None:
        # Red if the script echoes any field it did not derive: the VM document carries a serial
        # number and a platform UUID that must never reach an evidence label.
        completed = derive(SHARED_VM)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(len(completed.stdout.splitlines()), 1)
        self.assertNotIn("SECRET", completed.stdout)

    def test_missing_memory_field_is_refused(self) -> None:
        # Red if a record without a memory size yields a label with no memory suffix.
        fields = json.loads(REFERENCE_HOST)["SPHardwareDataType"][0]
        del fields["physical_memory"]
        completed = derive(json.dumps({"SPHardwareDataType": [fields]}))
        self.assertEqual(completed.returncode, 65, completed.stdout)
        self.assertEqual(completed.stdout, "")
        self.assertIn("memory", completed.stderr)

    def test_intel_record_is_refused(self) -> None:
        # Red if the script accepts a record with no "Apple ..." chip and invents a label.
        completed = derive(INTEL_HOST)
        self.assertEqual(completed.returncode, 65, completed.stdout)
        self.assertEqual(completed.stdout, "")
        self.assertIn("Apple", completed.stderr)

    def test_chip_with_no_letters_or_digits_is_refused(self) -> None:
        # Red if the sanitizer is allowed to collapse the chip to nothing and emit "-32gb": a
        # label starting with a dash is an argument-injection footgun for anything scripted
        # around the published profile name.
        completed = derive(record(chip_type="Apple ***", physical_memory="32 GB"))
        self.assertEqual(completed.returncode, 65, completed.stdout)
        self.assertEqual(completed.stdout, "")

    def test_malformed_document_is_refused(self) -> None:
        # Red if a system_profiler failure (empty or non-JSON output) is turned into a label
        # instead of an exit-65 refusal.
        completed = derive("")
        self.assertEqual(completed.returncode, 65, completed.stdout)
        self.assertEqual(completed.stdout, "")


if __name__ == "__main__":
    unittest.main()
