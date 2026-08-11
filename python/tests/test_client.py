import dataclasses
import signal
import subprocess
import sys
import tempfile
import unittest
from collections.abc import Mapping
from pathlib import Path
from unittest import mock

import mlx_guard


class ClientTests(unittest.TestCase):
    def test_observe_config_is_immutable_and_builds_literal_cli_argv(self) -> None:
        config = mlx_guard.ObserveConfig(
            command=("/bin/echo", "a b", "$(literal)"),
            report=Path("reports/result.json"),
            sample_interval_ms=25,
            cwd=Path("work"),
            clear_env=True,
            env=(("B", "two=parts"), ("A", "one")),
        )

        argv = mlx_guard.supervisor_argv(config)

        self.assertEqual(Path(argv[0]), mlx_guard.binary_path())
        self.assertEqual(
            argv[1:],
            (
                "observe",
                "--sample-interval",
                "25ms",
                "--report",
                "reports/result.json",
                "--cwd",
                "work",
                "--clear-env",
                "--env",
                "A=one",
                "--env",
                "B=two=parts",
                "--",
                "/bin/echo",
                "a b",
                "$(literal)",
            ),
        )
        with self.assertRaises(dataclasses.FrozenInstanceError):
            config.sample_interval_ms = 50  # type: ignore[misc]

    def test_config_rejects_values_the_native_contract_cannot_represent(self) -> None:
        invalid = (
            {"command": (), "report": Path("report.json")},
            {
                "command": ("/bin/true",),
                "report": Path("report.json"),
                "sample_interval_ms": 9,
            },
            {
                "command": ("/bin/true",),
                "report": Path("report.json"),
                "env": (("A", "1"), ("A", "2")),
            },
            {
                "command": ("/bin/true",),
                "report": Path("report.json"),
                "env": (("MLX_GUARD_CHECKPOINT_FD", "4"),),
            },
        )
        for kwargs in invalid:
            with self.subTest(kwargs=kwargs), self.assertRaises(mlx_guard.ConfigurationError):
                mlx_guard.ObserveConfig(**kwargs)  # type: ignore[arg-type]

        with self.assertRaises(mlx_guard.ConfigurationError):
            mlx_guard.RunConfig(
                command=("/bin/true",),
                report=Path("report.json"),
                max_footprint_bytes=1,
            )

    def test_mutable_runtime_inputs_are_copied_into_frozen_config(self) -> None:
        command = ["/bin/echo", "first"]
        environment = [["A", "one"]]
        config = mlx_guard.ObserveConfig(
            command=command,  # type: ignore[arg-type]
            report=Path("report.json"),
            env=environment,  # type: ignore[arg-type]
        )

        command.append("changed")
        environment[0][1] = "changed"

        self.assertEqual(config.command, ("/bin/echo", "first"))
        self.assertEqual(config.env, (("A", "one"),))

    def test_synchronous_observe_captures_output_and_loads_a_typed_report(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary, "observe.json")
            result = mlx_guard.run(
                mlx_guard.ObserveConfig(
                    command=("/bin/echo", "hello"),
                    report=report,
                    sample_interval_ms=10,
                ),
                capture_output=True,
            )

        self.assertEqual(result.returncode, 0)
        assert result.stdout is not None
        self.assertTrue(result.stdout.startswith(b"hello\nmlx-guard: child_exited at "))
        self.assertEqual(result.stderr, b"")
        self.assertEqual(result.report.schema_version, 1)
        self.assertEqual(result.report.package_version, mlx_guard.__version__)
        self.assertEqual(result.report.outcome.kind, mlx_guard.OutcomeKind.CHILD_EXITED)
        self.assertEqual(result.report.outcome.code, 0)

    def test_incremental_output_preserves_stdout_and_stderr_streams(self) -> None:
        script = "import sys; print('out', flush=True); print('err', file=sys.stderr, flush=True)"
        with tempfile.TemporaryDirectory() as temporary:
            process = mlx_guard.start(
                mlx_guard.ObserveConfig(
                    command=(sys.executable, "-c", script),
                    report=Path(temporary, "stream.json"),
                    sample_interval_ms=10,
                ),
                capture_output=True,
            )
            events = tuple(process.iter_output())
            result = process.wait()

        stdout = b"".join(
            event.data for event in events if event.stream is mlx_guard.OutputStream.STDOUT
        )
        stderr = b"".join(
            event.data for event in events if event.stream is mlx_guard.OutputStream.STDERR
        )
        self.assertIn(b"out\n", stdout)
        self.assertIn(b"err\n", stderr)
        assert result.stdout is not None
        self.assertTrue(result.stdout.startswith(b"out\nmlx-guard: child_exited at "))
        self.assertEqual(result.stderr, b"err\n")

    def test_cancellation_is_forwarded_through_the_supervisor(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            process = mlx_guard.start(
                mlx_guard.RunConfig(
                    command=("/bin/sleep", "30"),
                    report=Path(temporary, "cancel.json"),
                    max_footprint_bytes=1024**4,
                    sample_interval_ms=10,
                ),
                capture_output=True,
            )
            self.assertTrue(process.cancel())
            result = process.wait(timeout=5)

        self.assertEqual(result.returncode, 128 + signal.SIGINT)
        self.assertEqual(result.report.outcome.kind, mlx_guard.OutcomeKind.CHILD_SIGNALED)
        self.assertEqual(result.report.outcome.signal, signal.SIGINT)

    def test_cli_and_api_share_normalized_configuration_and_outcome(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            api_path = Path(temporary, "api.json")
            cli_path = Path(temporary, "cli.json")
            api_result = mlx_guard.run(
                mlx_guard.ObserveConfig(
                    command=("/usr/bin/true",),
                    report=api_path,
                    sample_interval_ms=10,
                ),
                capture_output=True,
            )
            cli_result = subprocess.run(
                [
                    mlx_guard.binary_path(),
                    "observe",
                    "--sample-interval",
                    "10ms",
                    "--report",
                    cli_path,
                    "--",
                    "/usr/bin/true",
                ],
                check=False,
                capture_output=True,
            )
            self.assertEqual(cli_result.returncode, 0, cli_result.stderr)
            cli_report = mlx_guard.load_report(cli_path)

        self.assertEqual(cli_result.stderr, b"")
        self.assertEqual(
            api_result.report.payload["configuration"],
            cli_report.payload["configuration"],
        )
        self.assertEqual(api_result.report.outcome.kind, cli_report.outcome.kind)
        self.assertEqual(api_result.report.outcome.code, cli_report.outcome.code)
        api_run = api_result.report.payload["run"]
        cli_run = cli_report.payload["run"]
        self.assertIsInstance(api_run, Mapping)
        self.assertIsInstance(cli_run, Mapping)
        assert isinstance(api_run, Mapping)
        assert isinstance(cli_run, Mapping)
        self.assertEqual(api_run["executable_basename"], cli_run["executable_basename"])

    def test_native_launch_failure_remains_a_typed_result(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            result = mlx_guard.run(
                mlx_guard.ObserveConfig(
                    command=("/definitely/missing/mlx-guard-worker",),
                    report=Path(temporary, "missing.json"),
                ),
                capture_output=True,
            )

        self.assertEqual(result.returncode, 127)
        self.assertEqual(result.report.outcome.kind, mlx_guard.OutcomeKind.LAUNCH_NOT_FOUND)

    def test_real_worker_helper_acknowledges_before_policy_intervention(self) -> None:
        script = """
import time
import mlx_guard

def checkpoint(request):
    return mlx_guard.CheckpointResponse.completed()

worker = mlx_guard.CheckpointWorker.connect(checkpoint)
assert worker is not None
with worker:
    while True:
        worker.poll()
        time.sleep(0.001)
"""
        with tempfile.TemporaryDirectory() as temporary:
            result = mlx_guard.run(
                mlx_guard.RunConfig(
                    command=(sys.executable, "-c", script),
                    report=Path(temporary, "checkpoint.json"),
                    max_footprint_bytes=1024**4,
                    wall_time_ms=500,
                    sample_interval_ms=10,
                ),
                capture_output=True,
            )

        checkpoint = result.report.payload["checkpoint"]
        self.assertEqual(result.stderr, b"", result.stderr)
        self.assertIsInstance(checkpoint, Mapping)
        assert isinstance(checkpoint, Mapping)
        self.assertEqual(checkpoint["status"], "acknowledged_unverified_durability")
        self.assertEqual(result.returncode, 75)
        self.assertEqual(result.report.outcome.kind, mlx_guard.OutcomeKind.POLICY_INTERVENTION)

    def test_discovery_failure_is_mapped_without_starting_a_process(self) -> None:
        config = mlx_guard.ObserveConfig(
            command=("/bin/true",),
            report=Path("report.json"),
        )
        with mock.patch(
            "mlx_guard._client.binary_version",
            side_effect=mlx_guard.BinaryVersionError("version mismatch"),
        ), self.assertRaisesRegex(
            mlx_guard.SupervisorDiscoveryError,
            "^native supervisor validation failed$",
        ):
            mlx_guard.start(config)

    def test_report_loader_rejects_invalid_json_and_schema(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary, "report.json")
            path.write_text("not json", encoding="utf-8")
            with self.assertRaises(mlx_guard.InvalidReportError):
                mlx_guard.load_report(path)
            path.write_text('{"schema_version": 2}', encoding="utf-8")
            with self.assertRaisesRegex(
                mlx_guard.InvalidReportError,
                "^unsupported report schema$",
            ):
                mlx_guard.load_report(path)
            path.write_text(
                '{"schema_version": 1, "schema_version": 1}',
                encoding="utf-8",
            )
            with self.assertRaisesRegex(
                mlx_guard.InvalidReportError,
                "^native supervisor report is not valid JSON$",
            ):
                mlx_guard.load_report(path)
            target = Path(temporary, "target.json")
            target.write_text('{"schema_version": 1}', encoding="utf-8")
            path.unlink()
            path.symlink_to(target)
            with self.assertRaisesRegex(
                mlx_guard.InvalidReportError,
                "^native supervisor report could not be opened safely$",
            ):
                mlx_guard.load_report(path)


if __name__ == "__main__":
    unittest.main()
