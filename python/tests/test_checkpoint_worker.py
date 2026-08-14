from __future__ import annotations

import os
import signal
import socket
import struct
import threading
import traceback
import unittest
from collections.abc import Iterator
from contextlib import contextmanager
from typing import cast
from unittest import mock

import mlx_guard

_NONCE = bytes(range(32))


class CheckpointWorkerTests(unittest.TestCase):
    def test_connect_returns_none_outside_a_supervised_worker(self) -> None:
        with mock.patch.dict(os.environ, {}, clear=True):
            self.assertIsNone(mlx_guard.CheckpointWorker.connect(_invalid_callback))

    def test_completed_callback_round_trips_only_path_free_artifact_facts(self) -> None:
        request_id = 41
        supervisor, inherited_fd = _checkpoint_pair()
        supervisor.sendall(_hello())
        seen: list[mlx_guard.CheckpointRequest] = []

        def checkpoint(request: mlx_guard.CheckpointRequest) -> mlx_guard.CheckpointResponse:
            seen.append(request)
            return mlx_guard.CheckpointResponse.completed(
                mlx_guard.CheckpointArtifact(
                    kind=mlx_guard.CheckpointArtifactKind.FILE,
                    size_bytes=123,
                )
            )

        with _worker_environment(inherited_fd):
            worker = mlx_guard.CheckpointWorker.connect(checkpoint)
        assert worker is not None
        with worker:
            self.assertEqual(_read_frame(supervisor), _ready())
            supervisor.sendall(_request(request_id=request_id, deadline_ns=5_000_000))
            os.kill(os.getpid(), signal.SIGUSR1)
            response = worker.poll()

        acknowledgement = _read_frame(supervisor)
        supervisor.close()
        self.assertEqual(len(seen), 1)
        self.assertEqual(seen[0].request_id, request_id)
        self.assertEqual(seen[0].supervisor_deadline_ns, 5_000_000)
        self.assertEqual(
            response,
            mlx_guard.CheckpointResponse.completed(
                mlx_guard.CheckpointArtifact(
                    kind=mlx_guard.CheckpointArtifactKind.FILE,
                    size_bytes=123,
                )
            ),
        )
        self.assertEqual(acknowledgement[4:10], b"MGCP\x01\x02")
        self.assertEqual(acknowledgement[10:42], _NONCE)
        self.assertEqual(struct.unpack(">Q", acknowledgement[42:50])[0], request_id)
        self.assertEqual(acknowledgement[50:53], b"\x01\x01\x01")
        self.assertEqual(struct.unpack(">Q", acknowledgement[53:61])[0], 123)

    def test_callback_exception_sends_failed_acknowledgement(self) -> None:
        supervisor, inherited_fd = _checkpoint_pair()
        supervisor.sendall(_hello())
        callback_canary = "CALLBACK_SECRET_CANARY_9d83"

        def checkpoint(_request: mlx_guard.CheckpointRequest) -> mlx_guard.CheckpointResponse:
            raise RuntimeError(callback_canary)

        with _worker_environment(inherited_fd):
            worker = mlx_guard.CheckpointWorker.connect(checkpoint)
        assert worker is not None
        with worker:
            _read_frame(supervisor)
            supervisor.sendall(_request(request_id=7, deadline_ns=9_000_000))
            os.kill(os.getpid(), signal.SIGUSR1)
            with self.assertRaisesRegex(
                mlx_guard.CheckpointCallbackError,
                "^checkpoint callback failed$",
            ) as raised:
                worker.poll()

        acknowledgement = _read_frame(supervisor)
        supervisor.close()
        self.assertEqual(acknowledgement[50], 2)
        rendered = "".join(
            traceback.format_exception(
                type(raised.exception), raised.exception, raised.exception.__traceback__
            )
        )
        self.assertNotIn(callback_canary, rendered)
        self.assertIsNone(raised.exception.__cause__)
        self.assertIsNone(raised.exception.__context__)

    def test_callback_must_explicitly_declare_a_result(self) -> None:
        supervisor, inherited_fd = _checkpoint_pair()
        supervisor.sendall(_hello())
        with _worker_environment(inherited_fd):
            worker = mlx_guard.CheckpointWorker.connect(_invalid_callback)
        assert worker is not None
        with worker:
            _read_frame(supervisor)
            supervisor.sendall(_request(request_id=8, deadline_ns=9_000_000))
            os.kill(os.getpid(), signal.SIGUSR1)
            with self.assertRaisesRegex(
                mlx_guard.CheckpointCallbackError,
                "^checkpoint callback must return CheckpointResponse$",
            ):
                worker.poll()

        acknowledgement = _read_frame(supervisor)
        supervisor.close()
        self.assertEqual(acknowledgement[50], 2)

    def test_malformed_hello_is_rejected_before_signal_handler_installation(self) -> None:
        previous = signal.getsignal(signal.SIGUSR1)
        supervisor, inherited_fd = _checkpoint_pair()
        supervisor.sendall(_frame(b"NOPE\x01\x03" + _NONCE))
        with _worker_environment(inherited_fd), self.assertRaisesRegex(
            mlx_guard.CheckpointProtocolError,
            "^checkpoint hello is malformed$",
        ):
            mlx_guard.CheckpointWorker.connect(_invalid_callback)
        supervisor.close()
        self.assertEqual(signal.getsignal(signal.SIGUSR1), previous)

    def test_reserved_stdio_descriptor_is_rejected_before_socket_ownership(self) -> None:
        with _worker_environment(2), mock.patch(
            "mlx_guard._checkpoint.socket.socket",
            side_effect=AssertionError("reserved descriptor must not become a socket"),
        ) as socket_factory, self.assertRaisesRegex(
            mlx_guard.CheckpointProtocolError,
            "^checkpoint descriptor is invalid$",
        ):
            mlx_guard.CheckpointWorker.connect(_invalid_callback)
        socket_factory.assert_not_called()

    def test_non_main_thread_installation_preserves_the_typed_error(self) -> None:
        supervisor, inherited_fd = _checkpoint_pair()
        self.addCleanup(supervisor.close)
        supervisor.sendall(_hello())
        errors: list[BaseException] = []

        def connect() -> None:
            try:
                mlx_guard.CheckpointWorker.connect(_invalid_callback)
            except BaseException as error:
                errors.append(error)

        with _worker_environment(inherited_fd):
            thread = threading.Thread(target=connect)
            thread.start()
            thread.join(timeout=2)

        self.assertFalse(thread.is_alive())
        self.assertEqual(len(errors), 1)
        self.assertIsInstance(errors[0], mlx_guard.CheckpointProtocolError)


def _checkpoint_pair() -> tuple[socket.socket, int]:
    supervisor, worker = socket.socketpair()
    return supervisor, worker.detach()


def _invalid_callback(_request: mlx_guard.CheckpointRequest) -> mlx_guard.CheckpointResponse:
    return cast(mlx_guard.CheckpointResponse, None)


@contextmanager
def _worker_environment(fd: int) -> Iterator[None]:
    with mock.patch.dict(os.environ, {"MLX_GUARD_CHECKPOINT_FD": str(fd)}):
        yield


def _frame(body: bytes) -> bytes:
    return struct.pack(">I", len(body)) + body


def _hello() -> bytes:
    return _frame(b"MGCP\x01\x03" + _NONCE)


def _ready() -> bytes:
    return _frame(b"MGCP\x01\x04" + _NONCE)


def _request(*, request_id: int, deadline_ns: int) -> bytes:
    return _frame(
        b"MGCP\x01\x01"
        + _NONCE
        + struct.pack(">Q", request_id)
        + struct.pack(">Q", deadline_ns)
    )


def _read_frame(channel: socket.socket) -> bytes:
    header = _read_exact(channel, 4)
    return header + _read_exact(channel, struct.unpack(">I", header)[0])


def _read_exact(channel: socket.socket, length: int) -> bytes:
    chunks: list[bytes] = []
    remaining = length
    while remaining:
        chunk = channel.recv(remaining)
        if not chunk:
            raise AssertionError("checkpoint channel closed before the complete frame")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


if __name__ == "__main__":
    unittest.main()
