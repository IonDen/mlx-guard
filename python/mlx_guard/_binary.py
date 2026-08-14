"""Locate and validate the native supervisor installed by the wheel."""

from __future__ import annotations

import base64
import hashlib
import os
import stat
import subprocess
import sysconfig
from importlib.metadata import Distribution, PackagePath, distribution, version
from pathlib import Path

_DISTRIBUTION_NAME = "mlx-guard"
_BINARY_NAME = "mlx-guard"


class BinaryDiscoveryError(RuntimeError):
    """The packaged native supervisor cannot be located safely."""


class PackageIntegrityError(BinaryDiscoveryError):
    """The installed native supervisor does not match wheel metadata."""


class BinaryVersionError(BinaryDiscoveryError):
    """The packaged native supervisor and Python package versions disagree."""


def binary_path() -> Path:
    """Return the verified executable installed for the active Python interpreter."""
    package = distribution(_DISTRIBUTION_NAME)
    candidate = Path(sysconfig.get_path("scripts"), _BINARY_NAME)
    _validate_file(candidate)
    record = _find_record(package, candidate)
    _validate_hash(candidate, record)
    return candidate


def binary_version() -> str:
    """Run the verified executable and return its matching package version."""
    expected = version(_DISTRIBUTION_NAME)
    try:
        completed = subprocess.run(  # noqa: S603 - wheel RECORD authenticates the exact binary.
            [binary_path(), "--version"],
            check=False,
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=5,
            env={"LC_ALL": "C", "PATH": os.defpath},
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise BinaryVersionError("packaged supervisor version check failed") from error
    expected_output = f"mlx-guard {expected}\n"
    if completed.returncode != 0 or completed.stderr or completed.stdout != expected_output:
        raise BinaryVersionError("packaged supervisor version does not match the Python package")
    return expected


def _validate_file(candidate: Path) -> None:
    try:
        metadata = candidate.lstat()
    except OSError as error:
        raise BinaryDiscoveryError("packaged supervisor is missing") from error
    if candidate.is_symlink() or not stat.S_ISREG(metadata.st_mode):
        raise BinaryDiscoveryError("packaged supervisor is not a regular file")
    if metadata.st_mode & 0o111 == 0:
        raise BinaryDiscoveryError("packaged supervisor is not executable")


def _find_record(package: Distribution, candidate: Path) -> PackagePath:
    records = package.files
    if records is None:
        raise PackageIntegrityError("package installation has no file integrity metadata")
    for record in records:
        try:
            if Path(str(package.locate_file(record))).samefile(candidate):
                return record
        except OSError:
            continue
    raise PackageIntegrityError("packaged supervisor is absent from file integrity metadata")


def _validate_hash(candidate: Path, record: PackagePath) -> None:
    recorded_hash = record.hash
    if recorded_hash is None or recorded_hash.mode != "sha256":
        raise PackageIntegrityError("packaged supervisor has no SHA-256 integrity metadata")
    digest = hashlib.sha256()
    try:
        with candidate.open("rb") as executable:
            for block in iter(lambda: executable.read(1024 * 1024), b""):
                digest.update(block)
    except OSError as error:
        raise PackageIntegrityError("packaged supervisor integrity check failed") from error
    actual = base64.urlsafe_b64encode(digest.digest()).rstrip(b"=").decode("ascii")
    if actual != recorded_hash.value:
        raise PackageIntegrityError("packaged supervisor does not match file integrity metadata")
