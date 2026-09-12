# Examples

These are the shortest working commands. For a walk through one real job, read the
[tutorial](https://github.com/IonDen/mlx-guard/blob/main/TUTORIAL.md).

Create an owner-only report directory once:

```bash
install -d -m 700 reports
```

## Measure before enforcing

Run representative work several times with no intervention policy:

```bash
mlx-guard observe --sample-interval 50ms --report reports/observe-1.json -- \
  python train.py --epochs 1
```

Choose a limit from observed peaks plus workload-specific headroom; do not use total machine memory
as the limit. The calibration guide explains the procedure.

## Enforce memory and time

```bash
mlx-guard run --max-footprint 24GiB --wall-time 2h \
  --report reports/train.json -- python train.py --epochs 10
```

The command exits with the child's status when no intervention occurs and `75` after a policy
intervention. Inspect the typed final report to distinguish outcomes.

## Launch from Python

```python
from pathlib import Path

import mlx_guard

result = mlx_guard.run(
    mlx_guard.RunConfig(
        command=("python", "train.py"),
        report=Path("reports/train.json"),
        max_footprint_bytes=24 * 1024**3,
        wall_time_ms=2 * 60 * 60 * 1000,
    )
)
print(result.returncode, result.report.outcome.kind)
```

Arguments are passed directly without a shell. Output is inherited by default and is never copied
into the report. See `PYTHON_API.md` before enabling captured output or cooperative checkpoints.

## Captured smoke run

This output was captured on 2026-09-12 on the M1 Max reference host (macOS 26.6.2) with the 0.2.0
release build. Timing and footprint values vary by host, so the stable facts are the typed outcome,
sample count, and absence of signals.

```console
$ mlx-guard observe --sample-interval 10ms --report reports/echo.json -- /bin/echo hello
hello
mlx-guard: child_exited at 12ms; 1 sample, 0 signals
$ echo $?
0
```

The resulting schema-v1 report recorded `child_exited`, child code `0`, one sample, zero signals,
`redacted_before_persistence: true`, file mode `0600`, and upload disabled, plus the observe-only
`calibration` section with its `safety_certified: false` marker.
