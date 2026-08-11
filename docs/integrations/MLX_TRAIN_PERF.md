# mlx-train-perf integration

`mlx-train-perf` runs each benchmark condition in its own Python process. The runner invokes
`python -m mlx_train_perf.bench.worker --config ...` directly, then preserves any result the worker
wrote. Exact identity plus `status="ok"` is required for resume. Other statuses are retried later.

The worker already has three local protections:

- a device-clamped MLX wired limit;
- an advisory MLX memory limit;
- an active-memory and wall-time watchdog.

When the watchdog fires, it atomically writes an honest `aborted_memory_ceiling` or
`aborted_wall_budget` result and hard-exits with code 70. This remains the v0.1 fallback. External
supervision supplements it with process-group ownership and OS-accounted footprint; it does not
replace MLX-specific wired-limit protection.

## Integration boundary

External supervision is opt-in at the benchmark runner's process-launch boundary. The integration
passes the existing literal worker argument vector to `mlx_guard.RunConfig`, with an explicit
OS-accounted footprint limit and a report path beside the condition artifact. Worker internals and
the `mlx-guard` policy engine remain application-neutral.

The worker may connect `mlx_guard.CheckpointWorker` when the inherited checkpoint descriptor is
present. It polls only at safe step or repetition boundaries. On a request, the callback writes and
syncs a partial condition artifact under the existing identity, then returns a path-free artifact
kind and byte count. The acknowledgement means only that the worker completed its callback. It does
not claim that Metal released memory or that the partial result is a complete benchmark.

## Fallback and failure behavior

Direct launch is allowed only when external supervision was requested but could not start a worker:
the Python package is absent, the native binary is missing, or package and binary versions differ.
The runner records the reason. These checks happen before worker launch, so fallback cannot duplicate
a condition.

Once a supervisor process starts, the runner never launches the condition again as a fallback. This
rule applies to cancellation and report failures. A surviving condition artifact is preserved, while
the supervisor failure remains a separate typed result.

| Case | Required result |
|---|---|
| No acknowledgement | The native checkpoint deadline expires and escalation continues. |
| Callback exception | The helper sends a failed response; no completed artifact is claimed. |
| Client cancellation | SIGINT is sent to the supervisor, which owns forwarding and finalization. |
| Missing binary or version mismatch | Direct launch is permitted because no worker was started. |
| Report write failure | The condition is not rerun; any worker artifact remains available. |

This integration adds no `mlx-train-perf` condition kind, status, or policy rule to `mlx-guard` core.
The native report and the benchmark artifact keep separate schemas and responsibilities.
