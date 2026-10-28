"""Tests for importing the package when its distribution metadata is absent."""

import shutil
import subprocess
import sys
import tempfile
import unittest
from importlib.metadata import version
from pathlib import Path

import mlx_guard

# The probe runs in a fresh interpreter started with -I -S: no site-packages and no user site,
# so the only `mlx_guard` it can import is the bare copy of the sources, which has no dist-info.
PROBE = """\
import sys
from importlib.metadata import PackageNotFoundError, version
from pathlib import Path

root, step, scratch = sys.argv[1:]
sys.path.insert(0, root)
try:
    version("mlx-guard")
except PackageNotFoundError:
    pass
else:
    print("precondition failed: mlx-guard metadata is visible")
    sys.exit(3)

import mlx_guard

if Path(mlx_guard.__file__).resolve().parent != Path(root, "mlx_guard").resolve():
    print(f"precondition failed: imported {mlx_guard.__file__}")
    sys.exit(3)

launched = Path(scratch, "launched")
config = mlx_guard.RunConfig(
    command=("/usr/bin/touch", str(launched)),
    report=Path(scratch, "report.json"),
    max_footprint_bytes=1 << 30,
)
if step == "import":
    print("imported")
elif step in ("start", "supervisor_argv"):
    try:
        getattr(mlx_guard, step)(config)
    except mlx_guard.SupervisorDiscoveryError as error:
        print(f"SupervisorDiscoveryError: {error}")
    else:
        print("no error")
    print(f"launched={launched.exists()} report={Path(scratch, 'report.json').exists()}")
elif step == "version":
    try:
        value = mlx_guard.__version__
    except PackageNotFoundError as error:
        print(f"PackageNotFoundError: {error.args[0]}")
    else:
        print(f"version {value!r}")
"""


class ImportWithoutMetadataTests(unittest.TestCase):
    """Each test names the one-line defect that would turn it red."""

    temporary: tempfile.TemporaryDirectory[str]
    root: Path
    probe: Path

    @classmethod
    def setUpClass(cls) -> None:
        cls.temporary = tempfile.TemporaryDirectory()
        base = Path(cls.temporary.name)
        cls.root = base / "site"
        shutil.copytree(
            Path(mlx_guard.__file__).resolve().parent,
            cls.root / "mlx_guard",
            ignore=shutil.ignore_patterns("__pycache__"),
        )
        cls.probe = base / "probe" / "probe.py"
        cls.probe.parent.mkdir()
        cls.probe.write_text(PROBE, encoding="utf-8")

    @classmethod
    def tearDownClass(cls) -> None:
        cls.temporary.cleanup()

    def run_probe(self, step: str) -> subprocess.CompletedProcess[str]:
        scratch = tempfile.mkdtemp(dir=self.temporary.name)
        interpreter = [sys.executable, "-I", "-S", "-W", "error"]
        return subprocess.run(
            [*interpreter, str(self.probe), str(self.root), step, scratch],
            capture_output=True,
            text=True,
            check=False,
            timeout=60,
        )

    def test_import_succeeds_without_metadata(self) -> None:
        # Red if `__version__` is read from the metadata at import time again.
        completed = self.run_probe("import")
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(completed.stdout, "imported\n")

    def test_start_raises_discovery_error_before_launching(self) -> None:
        # Red if `start` stops translating the missing metadata into SupervisorDiscoveryError,
        # or launches the command before the supervisor is validated.
        completed = self.run_probe("start")
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(
            completed.stdout,
            "SupervisorDiscoveryError: native supervisor validation failed\n"
            "launched=False report=False\n",
        )

    def test_supervisor_argv_raises_discovery_error(self) -> None:
        # Red if `supervisor_argv` returns an argv for an unverified supervisor.
        completed = self.run_probe("supervisor_argv")
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(
            completed.stdout,
            "SupervisorDiscoveryError: native supervisor validation failed\n"
            "launched=False report=False\n",
        )

    def test_version_raises_package_not_found_without_metadata(self) -> None:
        # Red if reading `__version__` falls back to an invented string instead of raising.
        completed = self.run_probe("version")
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(completed.stdout, "PackageNotFoundError: mlx-guard\n")


class InstalledVersionTests(unittest.TestCase):
    def test_version_is_the_installed_distribution_version(self) -> None:
        # Red if the lazy lookup names another distribution or returns a constant.
        self.assertEqual(mlx_guard.__version__, version("mlx-guard"))

    def test_unknown_attribute_still_raises_attribute_error(self) -> None:
        # Red if the module `__getattr__` answers every name, not only `__version__`.
        with self.assertRaises(AttributeError):
            _ = mlx_guard.no_such_name  # type: ignore[attr-defined]


if __name__ == "__main__":
    unittest.main()
