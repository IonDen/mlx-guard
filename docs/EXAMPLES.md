# Examples

These are the shortest working commands. For a walk through one real job, read the
[tutorial](https://github.com/IonDen/mlx-guard/blob/main/TUTORIAL.md).

Create an owner-only report directory once:

```bash
install -d -m 700 reports
```

Typed into a terminal, each command prints one extra stderr line: the supervised command reads
`/dev/null` instead of the terminal, because it cannot take keyboard input. A file or a pipe on
standard input reaches the command unchanged, and so does an explicit `< /dev/null`, which also
silences the line.

## Measure before enforcing

Run representative work several times with no intervention policy:

```bash
mlx-guard observe --sample-interval 50ms --report reports/observe-1.json -- \
  python train.py --epochs 1
```

Choose a limit from observed peaks plus workload-specific headroom; do not use total machine memory
as the limit. The [calibration guide](OBSERVE_AND_CALIBRATION.md) explains the procedure.

## Enforce memory and time

```bash
mlx-guard run --max-footprint 24GiB --wall-time 2h \
  --report reports/train.json -- python train.py --epochs 10
```

The command exits with the child's status when no intervention occurs and `75` after a policy
intervention. Inspect the typed final report to distinguish outcomes; [REPORTS.md](REPORTS.md)
defines its fields, and the [tutorial](https://github.com/IonDen/mlx-guard/blob/main/TUTORIAL.md)
shows a recorded intervention next to the report it produced.

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
into the report. The supervisor inherits the script's standard input; started from a terminal,
the command reads `/dev/null` instead and one stderr line says so. See the
[Python API guide](PYTHON_API.md) before enabling captured output or cooperative checkpoints.

## Captured smoke run

This output was captured on 2026-11-13 on the M1 Max 32 GB (macOS 27.2) with a 0.5.2
build, with a non-terminal standard input, so it shows no terminal line. The reference
measurements are on macOS 27.2. Timing and footprint
values vary by host, so the stable facts are the typed outcome, sample count, and absence of
signals.

```console
$ mlx-guard observe --sample-interval 10ms --report reports/echo.json -- /bin/echo hello
hello
mlx-guard: child_exited at 11ms; 1 sample, 0 signals
$ echo $?
0
```

The resulting schema-v1 report recorded `child_exited`, child code `0`, one sample, zero signals,
`redacted_before_persistence: true`, file mode `0600`, and upload disabled, plus the observe-only
`calibration` section with its `safety_certified: false` marker.

A run that hits its wall-time limit, captured the same way:

```console
$ mlx-guard run --max-footprint 6GiB --wall-time 1s --report reports/sleep.json -- /bin/sleep 5
mlx-guard: enforcing a 6442450944-byte footprint limit; emergency KILL at 7086696038 bytes, about 10% above the limit
mlx-guard: policy_intervention at 1100ms; 20 samples, 1 signal
mlx-guard: what happened: the run reached the 1.0 s wall-time limit you set, so mlx-guard sent SIGTERM to the command's process group at 1.0 s.
mlx-guard: what to do: raise --wall-time if the command needs longer, or find out why it ran longer than expected. More: https://github.com/IonDen/mlx-guard/blob/v0.5.2/docs/TROUBLESHOOTING.md#75-the-wall-time-limit
$ echo $?
75
```

The last two lines say what happened and what to do, and the
[troubleshooting page](TROUBLESHOOTING.md) explains each stop in more detail.

A limit larger than the memory macOS reported as not wired just before launch prints the launch
line, and the command still runs:

```console
$ mlx-guard run --max-footprint 1TiB --report reports/fit.json -- /bin/echo hello
mlx-guard: enforcing a 1099511627776-byte footprint limit; emergency KILL at 1209462790553 bytes, about 10% above the limit
mlx-guard: warning: your limit of 1024.00 GiB is more than the 27.21 GiB macOS reported as not wired just before launch, so a run that reaches it may not fit in memory on this Mac. That number is one estimate at one moment and counts every other program's memory as room. mlx-guard runs with your limit as given. More: https://github.com/IonDen/mlx-guard/blob/v0.5.2/docs/TROUBLESHOOTING.md#warnings-during-a-run
hello
mlx-guard: child_exited at 55ms; 1 sample, 0 signals
$ echo $?
0
```

A Python script that holds 470 MiB for two seconds under a 512 MiB limit enters the warning band,
prints one warning line, and exits normally:

```console
$ cat hold.py
import time

block = bytearray(b"\x01") * (470 * 1024 * 1024)
time.sleep(2)
$ mlx-guard run --max-footprint 512MiB --report reports/band.json -- python3 hold.py
mlx-guard: enforcing a 536870912-byte footprint limit; emergency KILL at 590558003 bytes, about 10% above the limit
mlx-guard: warning: the OS-accounted footprint reached 477.31 MiB at 109 ms, at or above the 460.80 MiB warning threshold for your 512.00 MiB limit; 2 samples in a row at or above the limit stop the command, and 1 sample at or above the 563.20 MiB emergency threshold kills it at once, with no checkpoint. More: https://github.com/IonDen/mlx-guard/blob/v0.5.2/docs/TROUBLESHOOTING.md#warnings-during-a-run
mlx-guard: child_exited at 2112ms; 39 samples, 0 signals
$ echo $?
0
```
