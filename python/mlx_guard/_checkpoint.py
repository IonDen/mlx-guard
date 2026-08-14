"""Dependency-light worker side of the inherited checkpoint protocol."""

from __future__ import annotations

import os
import signal
import socket
import struct
from collections.abc import Callable
from dataclasses import dataclass
from enum import Enum
from types import FrameType
from typing import Any

_CHECKPOINT_FD_ENV = "MLX_GUARD_CHECKPOINT_FD"
_MAGIC = b"MGCP"
_VERSION = 1
_REQUEST_KIND = 1
_ACKNOWLEDGEMENT_KIND = 2
_HELLO_KIND = 3
_READY_KIND = 4
_MAX_BODY_BYTES = 128
_NEGOTIATION_BODY_BYTES = 38
_REQUEST_BODY_BYTES = 54
_ACKNOWLEDGEMENT_BODY_BYTES = 57
_CHANNEL_TIMEOUT_SECONDS = 1.0


class CheckpointError(RuntimeError):
    """Base class for worker checkpoint failures."""


class CheckpointProtocolError(CheckpointError):
    """The inherited checkpoint channel sent or received an invalid frame."""


class CheckpointCallbackError(CheckpointError):
    """The user checkpoint callback failed or returned an invalid result."""


class CheckpointArtifactKind(str, Enum):
    """Path-free artifact classification allowed on the wire."""

    FILE = "file"
    DIRECTORY = "directory"
    OPAQUE = "opaque"


@dataclass(frozen=True, slots=True)
class CheckpointArtifact:
    """Optional path-free artifact facts declared by the worker."""

    kind: CheckpointArtifactKind
    size_bytes: int | None = None

    def __post_init__(self) -> None:
        size = self.size_bytes
        if size is not None and (type(size) is not int or not 0 <= size <= (1 << 64) - 1):
            raise ValueError("size_bytes must fit an unsigned 64-bit integer")


class _ResponseStatus(Enum):
    COMPLETED = 1
    FAILED = 2
    CANCELLED = 3


@dataclass(frozen=True, slots=True)
class CheckpointResponse:
    """Worker-declared result sent to the supervisor."""

    _status: _ResponseStatus
    artifact: CheckpointArtifact | None = None

    def __post_init__(self) -> None:
        if self._status is not _ResponseStatus.COMPLETED and self.artifact is not None:
            raise ValueError("only a completed checkpoint may include artifact metadata")

    @classmethod
    def completed(cls, artifact: CheckpointArtifact | None = None) -> CheckpointResponse:
        """Declare completion after the callback has made its own durability decision."""
        return cls(_ResponseStatus.COMPLETED, artifact)

    @classmethod
    def failed(cls) -> CheckpointResponse:
        """Declare that the checkpoint attempt failed."""
        return cls(_ResponseStatus.FAILED)

    @classmethod
    def cancelled(cls) -> CheckpointResponse:
        """Declare that the checkpoint attempt was cancelled."""
        return cls(_ResponseStatus.CANCELLED)


@dataclass(frozen=True, slots=True)
class CheckpointRequest:
    """Authenticated request passed to the worker callback."""

    request_id: int
    supervisor_deadline_ns: int


class CheckpointWorker:
    """At-most-once worker endpoint for the inherited checkpoint channel."""

    def __init__(
        self,
        channel: socket.socket,
        nonce: bytes,
        callback: Callable[[CheckpointRequest], CheckpointResponse],
    ) -> None:
        self._channel = channel
        self._nonce = nonce
        self._callback = callback
        self._pending = False
        self._handled = False
        self._closed = False
        self._previous_handler: Any = None

    @classmethod
    def connect(
        cls,
        callback: Callable[[CheckpointRequest], CheckpointResponse],
    ) -> CheckpointWorker | None:
        """Negotiate the inherited channel, or return ``None`` outside mlx-guard."""
        raw_fd = os.environ.get(_CHECKPOINT_FD_ENV)
        if raw_fd is None:
            return None
        try:
            fd = int(raw_fd, 10)
            if fd < 3:
                raise ValueError
        except ValueError as error:
            raise CheckpointProtocolError("checkpoint descriptor is invalid") from error
        try:
            channel = socket.socket(fileno=fd)
        except OSError as error:
            raise CheckpointProtocolError("checkpoint descriptor is invalid") from error
        try:
            channel.settimeout(_CHANNEL_TIMEOUT_SECONDS)
            channel.set_inheritable(False)
        except OSError as error:
            channel.close()
            raise CheckpointProtocolError("checkpoint descriptor is invalid") from error
        worker: CheckpointWorker | None = None
        try:
            nonce = _decode_hello(_read_frame(channel))
            worker = cls(channel, nonce, callback)
            worker._install_handler()
            channel.sendall(_negotiation_frame(_READY_KIND, nonce))
        except BaseException:
            if worker is None:
                channel.close()
            else:
                worker.close()
            raise
        return worker

    def __enter__(self) -> CheckpointWorker:
        return self

    def __exit__(self, _type: object, _value: object, _traceback: object) -> None:
        self.close()

    @property
    def pending(self) -> bool:
        """Return whether SIGUSR1 announced a request that has not been handled."""
        return self._pending

    def poll(self) -> CheckpointResponse | None:
        """Run at most one pending callback on the caller's thread."""
        if self._closed:
            raise CheckpointError("checkpoint worker is closed")
        if not self._pending:
            return None
        self._pending = False
        if self._handled:
            raise CheckpointProtocolError("checkpoint request was repeated")
        self._handled = True
        request = _decode_request(_read_frame(self._channel), self._nonce)
        callback_failed = False
        try:
            response: object = self._callback(request)
        except BaseException:
            self._send(CheckpointResponse.failed(), request.request_id)
            response = None
            callback_failed = True
        if callback_failed:
            raise CheckpointCallbackError("checkpoint callback failed")
        if not isinstance(response, CheckpointResponse):
            self._send(CheckpointResponse.failed(), request.request_id)
            raise CheckpointCallbackError("checkpoint callback must return CheckpointResponse")
        self._send(response, request.request_id)
        return response

    def close(self) -> None:
        """Restore the prior SIGUSR1 handler and close the inherited channel."""
        if self._closed:
            return
        self._closed = True
        if self._previous_handler is not None:
            signal.signal(signal.SIGUSR1, self._previous_handler)
            self._previous_handler = None
        self._channel.close()

    def _install_handler(self) -> None:
        try:
            previous_handler = signal.getsignal(signal.SIGUSR1)
            signal.signal(signal.SIGUSR1, self._on_signal)
        except (OSError, ValueError) as error:
            raise CheckpointProtocolError(
                "checkpoint handler must be installed from the main thread"
            ) from error
        self._previous_handler = previous_handler

    def _on_signal(self, _signal_number: int, _frame: FrameType | None) -> None:
        self._pending = True

    def _send(self, response: CheckpointResponse, request_id: int) -> None:
        artifact = response.artifact
        if artifact is None:
            kind = 0
            has_size = 0
            size = 0
        else:
            kind = {
                CheckpointArtifactKind.FILE: 1,
                CheckpointArtifactKind.DIRECTORY: 2,
                CheckpointArtifactKind.OPAQUE: 3,
            }[artifact.kind]
            has_size = int(artifact.size_bytes is not None)
            size = artifact.size_bytes or 0
        body = (
            _MAGIC
            + bytes((_VERSION, _ACKNOWLEDGEMENT_KIND))
            + self._nonce
            + struct.pack(">Q", request_id)
            + bytes((response._status.value, kind, has_size))
            + struct.pack(">Q", size)
        )
        if len(body) != _ACKNOWLEDGEMENT_BODY_BYTES:
            raise CheckpointProtocolError("checkpoint acknowledgement construction failed")
        try:
            self._channel.sendall(_frame(body))
        except OSError as error:
            raise CheckpointProtocolError("checkpoint acknowledgement write failed") from error


def _read_frame(channel: socket.socket) -> bytes:
    try:
        header = _read_exact(channel, 4)
        body_length = struct.unpack(">I", header)[0]
        if body_length > _MAX_BODY_BYTES:
            raise CheckpointProtocolError("checkpoint frame is oversized")
        return header + _read_exact(channel, body_length)
    except CheckpointProtocolError:
        raise
    except (OSError, struct.error) as error:
        raise CheckpointProtocolError("checkpoint frame read failed") from error


def _read_exact(channel: socket.socket, length: int) -> bytes:
    chunks: list[bytes] = []
    remaining = length
    while remaining:
        chunk = channel.recv(remaining)
        if not chunk:
            raise CheckpointProtocolError("checkpoint channel closed during a frame")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def _decode_hello(frame: bytes) -> bytes:
    body = _exact_body(frame, _NEGOTIATION_BODY_BYTES, _HELLO_KIND, "checkpoint hello is malformed")
    return body[6:38]


def _decode_request(frame: bytes, expected_nonce: bytes) -> CheckpointRequest:
    body = _exact_body(
        frame,
        _REQUEST_BODY_BYTES,
        _REQUEST_KIND,
        "checkpoint request is malformed",
    )
    if body[6:38] != expected_nonce:
        raise CheckpointProtocolError("checkpoint request nonce does not match")
    request_id = struct.unpack(">Q", body[38:46])[0]
    deadline_ns = struct.unpack(">Q", body[46:54])[0]
    if request_id == 0 or deadline_ns == 0:
        raise CheckpointProtocolError("checkpoint request is malformed")
    return CheckpointRequest(request_id=request_id, supervisor_deadline_ns=deadline_ns)


def _exact_body(frame: bytes, length: int, kind: int, message: str) -> bytes:
    if len(frame) != 4 + length:
        raise CheckpointProtocolError(message)
    body_length = struct.unpack(">I", frame[:4])[0]
    body = frame[4:]
    if (
        body_length != length
        or body[:4] != _MAGIC
        or body[4] != _VERSION
        or body[5] != kind
    ):
        raise CheckpointProtocolError(message)
    return body


def _negotiation_frame(kind: int, nonce: bytes) -> bytes:
    return _frame(_MAGIC + bytes((_VERSION, kind)) + nonce)


def _frame(body: bytes) -> bytes:
    return struct.pack(">I", len(body)) + body
