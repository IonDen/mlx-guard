"""Tests for the evidence profile-label derivation used by the one-command host calibration."""

from __future__ import annotations

import subprocess
import unittest
from pathlib import Path

# Resolved from this file, not the CWD: the wheel proof runs the suite from a temp directory.
SCRIPT = Path(__file__).resolve().parents[2] / "scripts/host-profile-label.sh"

REFERENCE_HOST = (
    "Hardware:\n\n"
    "      Model Name: MacBook Pro\n"
    "      Model Identifier: MacBookPro18,2\n"
    "      Model Number: SECRET-SKU\n"
    "      Chip: Apple M1 Max\n"
    "      Total Number of Cores: 10 (8 Performance and 2 Efficiency)\n"
    "      Memory: 32 GB\n"
    "      Serial Number (system): SECRET-SERIAL\n"
    "      Hardware UUID: SECRET-UUID\n"
    "      Provisioning UDID: SECRET-UDID\n"
)

SHARED_VM = (
    "Hardware:\n\n"
    "      Model Name: Apple Virtual Machine 1\n"
    "      Model Identifier: VirtualMac2,1\n"
    "      Chip: Apple M1 (Virtual)\n"
    "      Total Number of Cores: 3\n"
    "      Memory: 7 GB\n"
)

INTEL_HOST = (
    "Hardware:\n\n"
    "      Model Name: MacBook Pro\n"
    "      Processor Name: 8-Core Intel Core i9\n"
    "      Memory: 32 GB\n"
)


def derive(record: str) -> subprocess.CompletedProcess[str]:
    """Feed one hardware record to the script and return the completed process."""
    return subprocess.run(
        [str(SCRIPT)],
        input=record,
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

    def test_output_carries_no_identifier_line(self) -> None:
        # Red if the script echoes any line it did not derive: the raw record on stdin holds
        # a serial number, UUID, and UDID that must never reach an evidence label.
        completed = derive(REFERENCE_HOST)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(len(completed.stdout.splitlines()), 1)
        self.assertNotIn("SECRET", completed.stdout)

    def test_missing_memory_line_is_refused(self) -> None:
        # Red if a record without a memory line yields a label with no memory suffix.
        record = "\n".join(line for line in REFERENCE_HOST.splitlines() if "Memory:" not in line)
        completed = derive(record)
        self.assertEqual(completed.returncode, 65, completed.stdout)
        self.assertEqual(completed.stdout, "")
        self.assertIn("memory", completed.stderr)

    def test_intel_record_is_refused(self) -> None:
        # Red if the script accepts a record with no "Chip: Apple" line and invents a label.
        completed = derive(INTEL_HOST)
        self.assertEqual(completed.returncode, 65, completed.stdout)
        self.assertEqual(completed.stdout, "")
        self.assertIn("Apple", completed.stderr)


if __name__ == "__main__":
    unittest.main()
