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
requires invoking-user ownership and mode `0600`, and checks the package version. `Report.outcome`
is typed; `Report.payload` exposes the complete JSON as recursively immutable mappings and tuples.
Child failures and policy interventions are normal `RunResult` values. Discovery, process startup,
occupied report targets, missing reports, malformed reports, and exit/report contradictions use
distinct `GuardError` subclasses. Successful journals are retained, so use a unique report path per
run or archive/remove both the report and its `.<name>.journal` deliberately.

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

`poll()` invokes the callback on the caller's thread after SIGUSR1 announces a nonce- and
request-bound request. The v0.1 supervisor allows **100 ms total** from request creation to receipt
of the acknowledgement; `request.supervisor_deadline_ns` carries that monotonic deadline. The
timeout is fixed and cannot be extended by callback progress. Python signal handlers run on the main
thread and may not run while it is blocked in a long native `mx.eval()` call. Poll at short safe
boundaries and make the callback finish within the remaining budget—for example, finalize an
incrementally written checkpoint. Do not put an unbounded full-model save in the callback. An
application that safely runs compute elsewhere may keep the main thread polling, but this is an
application-level threading choice, not an mlx-guard guarantee.

The helper sends `completed` only when the callback explicitly returns
`CheckpointResponse.completed()`. The callback, not the helper, decides whether its bytes are
durable. Exceptions and invalid returns send a failed acknowledgement. Artifact metadata contains
only a kind and optional byte count; paths and names never enter the checkpoint frame. The helper
has no MLX dependency.

If native readiness does not arrive within five seconds, the client first sends SIGINT and gives the
supervisor one bounded second to run its process-group cleanup before using SIGKILL. A failure in the
narrow interval after worker launch but before readiness can still prevent a final report; startup
readiness is not an arbitrary-daemon containment guarantee.
