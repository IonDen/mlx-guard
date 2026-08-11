"""Python access to the packaged native ``mlx-guard`` supervisor."""

from importlib.metadata import version

from ._binary import (
    BinaryDiscoveryError,
    BinaryVersionError,
    PackageIntegrityError,
    binary_path,
    binary_version,
)

__version__ = version("mlx-guard")

__all__ = [
    "BinaryDiscoveryError",
    "BinaryVersionError",
    "PackageIntegrityError",
    "__version__",
    "binary_path",
    "binary_version",
]
