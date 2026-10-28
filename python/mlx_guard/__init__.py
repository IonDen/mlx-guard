"""Python access to the packaged native ``mlx-guard`` supervisor."""

from importlib.metadata import version
from typing import TYPE_CHECKING

from ._binary import (
    BinaryDiscoveryError,
    BinaryVersionError,
    PackageIntegrityError,
    binary_path,
    binary_version,
)
from ._checkpoint import (
    CheckpointArtifact,
    CheckpointArtifactKind,
    CheckpointCallbackError,
    CheckpointError,
    CheckpointProtocolError,
    CheckpointRequest,
    CheckpointResponse,
    CheckpointWorker,
)
from ._client import (
    ConfigurationError,
    GuardError,
    GuardProcess,
    InvalidReportError,
    MissingReportError,
    ObserveConfig,
    Outcome,
    OutcomeKind,
    OutputEvent,
    OutputStream,
    Report,
    ReportError,
    ReportPathInUseError,
    ResultMismatchError,
    RunConfig,
    RunResult,
    SupervisorDiscoveryError,
    SupervisorStartError,
    load_report,
    run,
    start,
    supervisor_argv,
)

if TYPE_CHECKING:
    __version__: str
else:

    def __getattr__(name: str) -> str:
        # Read on access, so the package imports without its distribution metadata; reading
        # `__version__` then raises PackageNotFoundError.
        if name == "__version__":
            return version("mlx-guard")
        raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


__all__ = [
    "BinaryDiscoveryError",
    "BinaryVersionError",
    "CheckpointArtifact",
    "CheckpointArtifactKind",
    "CheckpointCallbackError",
    "CheckpointError",
    "CheckpointProtocolError",
    "CheckpointRequest",
    "CheckpointResponse",
    "CheckpointWorker",
    "ConfigurationError",
    "GuardError",
    "GuardProcess",
    "InvalidReportError",
    "MissingReportError",
    "ObserveConfig",
    "Outcome",
    "OutcomeKind",
    "OutputEvent",
    "OutputStream",
    "PackageIntegrityError",
    "Report",
    "ReportError",
    "ReportPathInUseError",
    "ResultMismatchError",
    "RunConfig",
    "RunResult",
    "SupervisorDiscoveryError",
    "SupervisorStartError",
    "__version__",
    "binary_path",
    "binary_version",
    "load_report",
    "run",
    "start",
    "supervisor_argv",
]
