# Python API

The Python package launches the packaged Rust supervisor. Policy evaluation, sampling, process
ownership, and signal escalation stay in the native process.

## Run and observe

Use `ObserveConfig` when you need measurements without intervention. Use `RunConfig` with an
explicit byte limit when the supervisor may intervene.

```python
from pathlib import Path

import mlx_guard

result = mlx_guard.run(
    mlx_guard.RunConfig(
        command=("python", "train.py"),
        report=Path("reports/train.json"),
        max_footprint_bytes=26 * 1024**3,
        wall_time_ms=2 * 60 * 60 * 1000,
    ),
    capture_output=True,
)

print(result.returncode, result.report.outcome.kind)
```

Configurations are frozen dataclasses. Commands remain literal argument tuples and never pass
through a shell. `start()` returns a `GuardProcess` for incremental work. Its `poll()` and `wait()`
methods return the same typed `RunResult` as `run()`. `cancel()` sends SIGINT to the supervisor;
calling it again requests the native immediate-escalation path.

Output inherits the caller's standard streams by default. This keeps the supervisor independent if
the Python client exits unexpectedly. With `capture_output=True`, `iter_output()` yields
`OutputEvent` byte chunks for stdout and stderr. Captured stdout includes both worker output and the
native final summary because the command-line interface shares that stream. A client crash closes
capture pipes, so crash-independent workloads should keep the inherited-stream default.

## Reports and errors

`load_report()` accepts a bounded regular schema-v1 file, rejects symlinks and unsupported schemas,
and checks the package version. `Report.outcome` is typed; `Report.payload` exposes the complete JSON
as recursively immutable mappings and tuples. Child failures and policy interventions are normal
`RunResult` values. Discovery, process startup, missing reports, malformed reports, and exit/report
contradictions use distinct `GuardError` subclasses.

## Cooperative checkpoints

Call `CheckpointWorker.connect()` from the worker's main thread before entering its work loop. It
returns `None` when the process was not launched by `mlx-guard`.

```python
def save_checkpoint(
    request: mlx_guard.CheckpointRequest,
) -> mlx_guard.CheckpointResponse:
    write_and_fsync_checkpoint()
    return mlx_guard.CheckpointResponse.completed(
        mlx_guard.CheckpointArtifact(mlx_guard.CheckpointArtifactKind.DIRECTORY)
    )

worker = mlx_guard.CheckpointWorker.connect(save_checkpoint)
if worker is not None:
    with worker:
        while training:
            train_step()
            worker.poll()
```

`poll()` invokes the callback on the caller's thread after SIGUSR1 announces an authenticated
request. The helper sends `completed` only when the callback explicitly returns
`CheckpointResponse.completed()`. The callback, not the helper, decides whether its bytes are
durable. Exceptions and invalid returns send a failed acknowledgement. Artifact metadata contains
only a kind and optional byte count; paths and names never enter the checkpoint frame. The helper
has no MLX dependency.
