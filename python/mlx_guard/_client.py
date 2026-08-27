"""Typed Python client for the external native supervisor."""

from __future__ import annotations

import dataclasses
import json
import math
import os
import selectors
import signal
import stat
import subprocess
from collections.abc import Iterable, Iterator, Mapping
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from types import MappingProxyType
from typing import IO, NoReturn, TypeAlias, cast

from ._binary import BinaryDiscoveryError, binary_path, binary_version

_CHECKPOINT_FD_ENV = "MLX_GUARD_CHECKPOINT_FD"
_MAX_U64 = (1 << 64) - 1
_MAX_WALL_TIME_MS = 30 * 24 * 60 * 60 * 1000
_MIN_CHECKPOINT_TIMEOUT_MS = 10
_MAX_CHECKPOINT_TIMEOUT_MS = 60 * 1000
_MAX_REPORT_BYTES = 16 * 1024 * 1024

JsonScalar: TypeAlias = bool | int | float | str | None
JsonValue: TypeAlias = JsonScalar | tuple["JsonValue", ...] | Mapping[str, "JsonValue"]


class GuardError(RuntimeError):
    """Base class for Python client failures."""


class ConfigurationError(GuardError, ValueError):
    """The Python configuration cannot satisfy the native CLI contract."""


class SupervisorDiscoveryError(GuardError):
    """The installed native supervisor failed discovery or version validation."""


class SupervisorStartError(GuardError):
    """The operating system could not start the verified supervisor."""


class ReportError(GuardError):
    """The final native report could not be loaded safely."""


class MissingReportError(ReportError):
    """The supervisor exited without the expected final report."""


class InvalidReportError(ReportError):
    """The final report does not satisfy the Python reader contract."""


class ResultMismatchError(ReportError):
    """The process status and typed final report contradict each other."""


class ReportPathInUseError(ReportError):
    """The report target is occupied by retained journal evidence."""


@dataclass(frozen=True, slots=True)
class ObserveConfig:
    """Immutable configuration for an observe-only run."""

    command: tuple[str, ...]
    report: Path
    sample_interval_ms: int = 50
    cwd: Path | None = None
    clear_env: bool = False
    env: tuple[tuple[str, str], ...] = ()
    on_parent_exit: str | None = dataclasses.field(default=None, kw_only=True)

    def __post_init__(self) -> None:
        _validate_common(self)


@dataclass(frozen=True, slots=True)
class RunConfig:
    """Immutable configuration for an enforcing run."""

    command: tuple[str, ...]
    report: Path
    max_footprint_bytes: int
    sample_interval_ms: int = 50
    wall_time_ms: int | None = None
    cwd: Path | None = None
    clear_env: bool = False
    env: tuple[tuple[str, str], ...] = ()
    on_parent_exit: str | None = dataclasses.field(default=None, kw_only=True)
    checkpoint_timeout_ms: int | None = dataclasses.field(default=None, kw_only=True)

    def __post_init__(self) -> None:
        _validate_common(self)
        limit = self.max_footprint_bytes
        if type(limit) is not int or limit < 2 or limit > _MAX_U64:
            raise ConfigurationError("max_footprint_bytes must be within 2..=u64::MAX")
        band_step = max(limit // 10, 1)
        if limit + band_step > _MAX_U64:
            raise ConfigurationError("max_footprint_bytes cannot represent the emergency band")
        wall_time = self.wall_time_ms
        if wall_time is not None and (
            type(wall_time) is not int or not 1 <= wall_time <= _MAX_WALL_TIME_MS
        ):
            raise ConfigurationError("wall_time_ms must be within 1ms..=30d")
        checkpoint_timeout = self.checkpoint_timeout_ms
        if checkpoint_timeout is not None and (
            type(checkpoint_timeout) is not int
            or not _MIN_CHECKPOINT_TIMEOUT_MS
            <= checkpoint_timeout
            <= _MAX_CHECKPOINT_TIMEOUT_MS
        ):
            raise ConfigurationError("checkpoint_timeout_ms must be within 10ms..=60s")


Config: TypeAlias = ObserveConfig | RunConfig


class OutcomeKind(str, Enum):
    """Schema-v1 terminal outcome kind."""

    CHILD_EXITED = "child_exited"
    CHILD_SIGNALED = "child_signaled"
    LAUNCH_NOT_FOUND = "launch_not_found"
    LAUNCH_NOT_EXECUTABLE = "launch_not_executable"
    INVALID_CONFIGURATION = "invalid_configuration"
    POLICY_INTERVENTION = "policy_intervention"
    SUPERVISOR_FAILURE = "supervisor_failure"
    PARTIAL_ARTIFACT_FAILURE = "partial_artifact_failure"


@dataclass(frozen=True, slots=True)
class Outcome:
    """Typed terminal fields from a schema-v1 report."""

    kind: OutcomeKind
    at_ms: int
    code: int | None = None
    signal: int | None = None


@dataclass(frozen=True, slots=True)
class Report:
    """Validated report summary plus an immutable full payload."""

    schema_version: int
    package_version: str
    outcome: Outcome
    payload: Mapping[str, JsonValue]


class OutputStream(str, Enum):
    """Source stream for one incremental byte chunk."""

    STDOUT = "stdout"
    STDERR = "stderr"


@dataclass(frozen=True, slots=True)
class OutputEvent:
    """One byte chunk read from a captured worker stream."""

    stream: OutputStream
    data: bytes


@dataclass(frozen=True, slots=True)
class RunResult:
    """Completed supervisor process and its matching typed report."""

    returncode: int
    report: Report
    stdout: bytes | None
    stderr: bytes | None


class GuardProcess:
    """Incremental handle for one authoritative native supervisor."""

    def __init__(
        self,
        process: subprocess.Popen[bytes],
        config: Config,
        *,
        capture_output: bool,
    ) -> None:
        self._process = process
        self._config = config
        self._capture_output = capture_output
        self._stdout = bytearray()
        self._stderr = bytearray()
        self._streams_drained = False
        self._result: RunResult | None = None

    @property
    def pid(self) -> int:
        """Return the native supervisor process ID."""
        return self._process.pid

    def cancel(self) -> bool:
        """Request cancellation through the supervisor's SIGINT path.

        Repeating this call triggers the native runtime's immediate escalation behavior.
        """
        if self._process.poll() is not None:
            return False
        try:
            self._process.send_signal(signal.SIGINT)
        except ProcessLookupError:
            return False
        return True

    def poll(self) -> RunResult | None:
        """Return the completed result without blocking, or ``None`` while running."""
        if self._result is not None:
            return self._result
        if self._process.poll() is None:
            return None
        return self._finish(timeout=None)

    def wait(self, timeout: float | None = None) -> RunResult:
        """Wait for the supervisor and validate its final report."""
        if self._result is not None:
            return self._result
        return self._finish(timeout=timeout)

    def iter_output(
        self,
        *,
        chunk_size: int = 64 * 1024,
    ) -> Iterable[OutputEvent]:
        """Yield captured stdout and stderr chunks until both streams reach EOF."""
        if not self._capture_output:
            raise GuardError("output capture was not enabled")
        if chunk_size <= 0:
            raise ValueError("chunk_size must be positive")
        if self._streams_drained:
            return ()
        return _OutputIterator(self, chunk_size)

    def _finish(self, timeout: float | None) -> RunResult:
        if self._capture_output and not self._streams_drained:
            stdout, stderr = self._process.communicate(timeout=timeout)
            self._stdout.extend(stdout or b"")
            self._stderr.extend(stderr or b"")
            self._streams_drained = True
        else:
            self._process.wait(timeout=timeout)
        returncode = self._process.returncode
        if returncode is None:
            raise GuardError("native supervisor status is unavailable")
        report = load_report(self._config.report)
        expected = _expected_exit_code(report.outcome)
        if returncode != expected:
            raise ResultMismatchError("native supervisor status does not match its report")
        self._result = RunResult(
            returncode=returncode,
            report=report,
            stdout=bytes(self._stdout) if self._capture_output else None,
            stderr=bytes(self._stderr) if self._capture_output else None,
        )
        return self._result


class _OutputIterator:
    def __init__(self, owner: GuardProcess, chunk_size: int) -> None:
        self._owner = owner
        self._chunk_size = chunk_size
        self._events = self._generate()

    def __iter__(self) -> _OutputIterator:
        return self

    def __next__(self) -> OutputEvent:
        return next(self._events)

    def _generate(self) -> Iterator[OutputEvent]:
        selector = selectors.DefaultSelector()
        streams: tuple[tuple[IO[bytes] | None, OutputStream], ...] = (
            (self._owner._process.stdout, OutputStream.STDOUT),
            (self._owner._process.stderr, OutputStream.STDERR),
        )
        try:
            for file_object, source in streams:
                if file_object is None:
                    raise GuardError("captured output stream is unavailable")
                selector.register(file_object, selectors.EVENT_READ, source)
            while selector.get_map():
                for key, _mask in selector.select(timeout=0.1):
                    data = os.read(key.fd, self._chunk_size)
                    if not data:
                        selector.unregister(key.fileobj)
                        cast(IO[bytes], key.fileobj).close()
                        continue
                    event_stream = cast(OutputStream, key.data)
                    if event_stream is OutputStream.STDOUT:
                        self._owner._stdout.extend(data)
                    else:
                        self._owner._stderr.extend(data)
                    yield OutputEvent(stream=event_stream, data=data)
        finally:
            selector.close()
        self._owner._streams_drained = True
        self._owner._process.wait()


def supervisor_argv(config: Config) -> tuple[str, ...]:
    """Return the exact verified native invocation for a typed configuration."""
    try:
        executable = binary_path()
    except BinaryDiscoveryError as error:
        raise SupervisorDiscoveryError("native supervisor validation failed") from error
    return _argv_for(executable, config)


def start(config: Config, *, capture_output: bool = False) -> GuardProcess:
    """Start the native supervisor and return an incremental process handle."""
    _ensure_report_target_available(config.report)
    try:
        binary_version()
        executable = binary_path()
    except BinaryDiscoveryError as error:
        raise SupervisorDiscoveryError("native supervisor validation failed") from error
    ready_read, ready_write = os.pipe()
    argv = _argv_for(executable, config, client_ready_fd=ready_write)
    try:
        process = subprocess.Popen(  # noqa: S603 - the verified binary receives a literal argv.
            argv,
            stdin=None,
            stdout=subprocess.PIPE if capture_output else None,
            stderr=subprocess.PIPE if capture_output else None,
            pass_fds=(ready_write,),
        )
    except OSError as error:
        os.close(ready_read)
        os.close(ready_write)
        raise SupervisorStartError("native supervisor could not be started") from error
    os.close(ready_write)
    try:
        _await_native_ready(ready_read, process)
    finally:
        os.close(ready_read)
    return GuardProcess(process, config, capture_output=capture_output)


def run(config: Config, *, capture_output: bool = False) -> RunResult:
    """Run the native supervisor synchronously and validate its final report."""
    return start(config, capture_output=capture_output).wait()


def load_report(path: Path) -> Report:
    """Load one bounded, regular schema-v1 report without following a symlink."""
    descriptor = -1
    missing = False
    unsafe = False
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    except FileNotFoundError:
        missing = True
    except OSError:
        unsafe = True
    if missing:
        raise MissingReportError("native supervisor did not produce a report")
    if unsafe:
        raise InvalidReportError("native supervisor report could not be opened safely")
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise InvalidReportError("native supervisor report is not a regular file")
        if metadata.st_uid != os.geteuid() or stat.S_IMODE(metadata.st_mode) != 0o600:
            raise InvalidReportError("native supervisor report has unsafe ownership or permissions")
        if metadata.st_size > _MAX_REPORT_BYTES:
            raise InvalidReportError("native supervisor report exceeds the reader limit")
        with os.fdopen(descriptor, "rb", closefd=True) as report_file:
            descriptor = -1
            encoded_bytes = report_file.read(_MAX_REPORT_BYTES + 1)
    except InvalidReportError:
        raise
    except (OSError, UnicodeError) as error:
        raise InvalidReportError("native supervisor report could not be read safely") from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    if len(encoded_bytes) > _MAX_REPORT_BYTES:
        raise InvalidReportError("native supervisor report exceeds the reader limit")
    try:
        encoded = encoded_bytes.decode("utf-8")
        decoded = cast(
            object,
            json.loads(
                encoded,
                parse_constant=_reject_json_constant,
                object_pairs_hook=_unique_json_object,
            ),
        )
    except (UnicodeError, ValueError) as error:
        raise InvalidReportError("native supervisor report is not valid JSON") from error
    if not isinstance(decoded, dict):
        raise InvalidReportError("native supervisor report must be a JSON object")
    schema_version = _required_int(decoded, "schema_version")
    if schema_version != 1:
        raise InvalidReportError("unsupported report schema")
    package_version = _required_str(decoded, "package_version")
    try:
        expected_version = binary_version()
    except BinaryDiscoveryError as error:
        raise SupervisorDiscoveryError("native supervisor validation failed") from error
    if package_version != expected_version:
        raise InvalidReportError("report package version does not match the installed package")
    outcome_value = decoded.get("outcome")
    if not isinstance(outcome_value, dict):
        raise InvalidReportError("native supervisor report has no typed outcome")
    outcome = _parse_outcome(outcome_value)
    frozen = _freeze_json(decoded)
    if not isinstance(frozen, Mapping):
        raise InvalidReportError("native supervisor report must be a JSON object")
    return Report(
        schema_version=schema_version,
        package_version=package_version,
        outcome=outcome,
        payload=frozen,
    )


def _validate_common(config: Config) -> None:
    raw_command = cast(object, config.command)
    if isinstance(raw_command, (str, bytes)) or not isinstance(raw_command, Iterable):
        raise ConfigurationError("command must be a sequence of strings")
    command = tuple(cast(object, item) for item in raw_command)
    if not command:
        raise ConfigurationError("command must not be empty")
    if any(not isinstance(value, str) or "\x00" in value for value in command):
        raise ConfigurationError("command values must be NUL-free strings")
    object.__setattr__(config, "command", cast(tuple[str, ...], command))
    try:
        report = Path(config.report)
        cwd = None if config.cwd is None else Path(config.cwd)
    except TypeError as error:
        raise ConfigurationError("report and cwd must be filesystem paths") from error
    if "\x00" in os.fspath(report) or (cwd is not None and "\x00" in os.fspath(cwd)):
        raise ConfigurationError("report and cwd must be NUL-free paths")
    object.__setattr__(config, "report", report)
    object.__setattr__(config, "cwd", cwd)
    if type(config.clear_env) is not bool:
        raise ConfigurationError("clear_env must be a boolean")
    interval = config.sample_interval_ms
    if type(interval) is not int or not 10 <= interval <= 10_000:
        raise ConfigurationError("sample_interval_ms must be within 10..=10000")
    keys: set[str] = set()
    normalized: list[tuple[str, str]] = []
    raw_environment = cast(object, config.env)
    if not isinstance(raw_environment, (tuple, list)):
        raise ConfigurationError("env must contain KEY, VALUE pairs")
    for entry in raw_environment:
        if not isinstance(entry, (tuple, list)) or len(entry) != 2:
            raise ConfigurationError("env must contain KEY, VALUE pairs")
        key, value = entry
        if not isinstance(key, str) or not isinstance(value, str):
            raise ConfigurationError("environment keys and values must be strings")
        if not key or "=" in key or "\x00" in key or "\x00" in value:
            raise ConfigurationError("environment entries must be representable as KEY=VALUE")
        if key == _CHECKPOINT_FD_ENV:
            raise ConfigurationError("the checkpoint descriptor environment key is reserved")
        if key in keys:
            raise ConfigurationError("environment keys must be unique")
        keys.add(key)
        normalized.append((key, value))
    object.__setattr__(config, "env", tuple(sorted(normalized)))
    if config.on_parent_exit is not None and config.on_parent_exit not in ("terminate", "detach"):
        raise ConfigurationError("on_parent_exit must be 'terminate' or 'detach'")


def _await_native_ready(descriptor: int, process: subprocess.Popen[bytes]) -> None:
    selector = selectors.DefaultSelector()
    try:
        selector.register(descriptor, selectors.EVENT_READ)
        events = selector.select(timeout=5.0)
        if not events:
            _stop_unready_supervisor(process)
            raise SupervisorStartError("native supervisor readiness timed out")
        if os.read(descriptor, 1):
            _stop_unready_supervisor(process)
            raise SupervisorStartError("native supervisor readiness protocol failed")
    finally:
        selector.close()


def _stop_unready_supervisor(process: subprocess.Popen[bytes]) -> None:
    """Give native process-group cleanup a bounded opportunity before forced termination."""
    if process.poll() is not None:
        return
    try:
        process.send_signal(signal.SIGINT)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=1.0)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def _ensure_report_target_available(path: Path) -> None:
    journal = path.with_name(f".{path.name}.journal")
    try:
        os.lstat(journal)
    except FileNotFoundError:
        return
    except OSError:
        raise ReportPathInUseError("report target availability could not be verified") from None
    raise ReportPathInUseError("report target has a retained journal")


def _argv_for(
    executable: Path,
    config: Config,
    *,
    client_ready_fd: int | None = None,
) -> tuple[str, ...]:
    common = [
        os.fspath(executable),
        "run" if isinstance(config, RunConfig) else "observe",
    ]
    if isinstance(config, RunConfig):
        common.extend(("--max-footprint", f"{config.max_footprint_bytes}B"))
        if config.wall_time_ms is not None:
            common.extend(("--wall-time", f"{config.wall_time_ms}ms"))
        if config.checkpoint_timeout_ms is not None:
            common.extend(("--checkpoint-timeout", f"{config.checkpoint_timeout_ms}ms"))
    common.extend(("--sample-interval", f"{config.sample_interval_ms}ms"))
    common.extend(("--report", os.fspath(config.report)))
    if client_ready_fd is not None:
        common.extend(("--client-ready-fd", str(client_ready_fd)))
    if config.cwd is not None:
        common.extend(("--cwd", os.fspath(config.cwd)))
    if config.clear_env:
        common.append("--clear-env")
    for key, value in config.env:
        common.extend(("--env", f"{key}={value}"))
    if config.on_parent_exit is not None:
        common.extend(("--on-parent-exit", config.on_parent_exit))
    common.append("--")
    common.extend(config.command)
    return tuple(common)


def _parse_outcome(value: Mapping[str, object]) -> Outcome:
    kind_text = _required_str(value, "kind")
    try:
        kind = OutcomeKind(kind_text)
    except ValueError as error:
        raise InvalidReportError("native supervisor report has an unknown outcome") from error
    at_ms = _required_int(value, "at_ms")
    if at_ms < 0:
        raise InvalidReportError("native supervisor report has an invalid outcome time")
    code = _optional_int(value, "code")
    signal_number = _optional_int(value, "signal")
    if kind is OutcomeKind.CHILD_EXITED:
        if code is None or not 0 <= code <= 255 or signal_number is not None:
            raise InvalidReportError("native supervisor report has invalid child exit fields")
    elif kind is OutcomeKind.CHILD_SIGNALED:
        if signal_number is None or not 1 <= signal_number <= 127 or code is not None:
            raise InvalidReportError("native supervisor report has invalid child signal fields")
    elif code is not None or signal_number is not None:
        raise InvalidReportError("native supervisor report has unexpected outcome fields")
    return Outcome(kind=kind, at_ms=at_ms, code=code, signal=signal_number)


def _expected_exit_code(outcome: Outcome) -> int:
    if outcome.kind is OutcomeKind.CHILD_EXITED:
        if outcome.code is None:
            raise ResultMismatchError("child exit report has no status code")
        return outcome.code
    if outcome.kind is OutcomeKind.CHILD_SIGNALED:
        if outcome.signal is None:
            raise ResultMismatchError("child signal report has no signal number")
        return 128 + outcome.signal
    try:
        return {
            OutcomeKind.INVALID_CONFIGURATION: 64,
            OutcomeKind.SUPERVISOR_FAILURE: 70,
            OutcomeKind.PARTIAL_ARTIFACT_FAILURE: 74,
            OutcomeKind.POLICY_INTERVENTION: 75,
            OutcomeKind.LAUNCH_NOT_EXECUTABLE: 126,
            OutcomeKind.LAUNCH_NOT_FOUND: 127,
        }[outcome.kind]
    except KeyError:
        raise InvalidReportError("native supervisor report has an unknown outcome kind") from None


def _required_int(value: Mapping[str, object], key: str) -> int:
    result = value.get(key)
    if type(result) is not int:
        raise InvalidReportError("native supervisor report has an invalid integer field")
    return result


def _optional_int(value: Mapping[str, object], key: str) -> int | None:
    result = value.get(key)
    if result is None:
        return None
    if type(result) is not int:
        raise InvalidReportError("native supervisor report has an invalid integer field")
    return result


def _required_str(value: Mapping[str, object], key: str) -> str:
    result = value.get(key)
    if not isinstance(result, str):
        raise InvalidReportError("native supervisor report has an invalid string field")
    return result


def _freeze_json(value: object) -> JsonValue:
    if value is None or isinstance(value, (bool, int, str)):
        return value
    if isinstance(value, float):
        if not math.isfinite(value):
            raise InvalidReportError("native supervisor report contains a non-finite number")
        return value
    if isinstance(value, list):
        return tuple(_freeze_json(item) for item in value)
    if isinstance(value, dict) and all(isinstance(key, str) for key in value):
        return MappingProxyType({str(key): _freeze_json(item) for key, item in value.items()})
    raise InvalidReportError("native supervisor report contains an unsupported JSON value")


def _reject_json_constant(_value: str) -> NoReturn:
    raise ValueError("nonstandard JSON constant")


def _unique_json_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON object key")
        result[key] = value
    return result
