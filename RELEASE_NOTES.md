# mlx-guard 0.5.0

`mlx-guard` 0.5.0 is a minor release of the alpha external runtime safety supervisor for MLX
workloads on Apple Silicon. It launches one command in an owned process group, samples the group's
OS-accounted footprint, and applies an explicit memory limit and optional wall-time limit from
outside the process. This release reports problems sooner: `run` can tell you before the start,
and while the command runs, that the footprint is close to the limit.

## What you will see

At launch, `run` adds up the memory macOS has not locked as wired (free, speculative included, plus
active, inactive, and held by the compressor). If your limit is more than that number, it prints one
line right after the start-of-run notice:

```
mlx-guard: warning: your limit of 30.00 GiB is more than the 28.31 GiB macOS reported as not wired just before launch, so a run that reaches it may not fit in memory on this Mac. …
```

The sizes in this example are illustrative; yours come from your limit and your Mac.

The number in the line is one estimate at one moment, and it counts every other
program's memory as room. `mlx-guard` runs the command with your limit as given and never refuses to
start. If macOS gives no number, nothing is printed, and no line does not mean the run fits. The same
numbers appear in a new optional `prelaunch` field of `run` reports.

While the command runs, `run` prints a line on stderr when the footprint first reaches 10 % below the
limit, repeats it at most every 5 minutes while the footprint has not fallen to the recovery threshold
(20 % below the limit), and prints a line when it falls back. The prefixes are `mlx-guard: warning:` and
`mlx-guard: warning cleared:`. A warning does not change when the command is stopped. The lines hold
only numbers `mlx-guard` measured and fixed text, never the command, environment values, paths, or
command output. Delivery is best effort: a line can be lost when nothing reads stderr. `observe`
prints none, and there is no option to turn them off. Both lines are explained in the
[troubleshooting page](https://github.com/IonDen/mlx-guard/blob/main/docs/TROUBLESHOOTING.md).

Four fixes ride along:

- A stderr reader that has gone away, a closed pipe for example, no longer ends the run with exit
  code `101` or loses the report. You get the right exit code and the report.
- A full stderr pipe that nobody reads no longer freezes `mlx-guard` before it starts watching
  memory.
- Cleanup waits at most 2 seconds for a command that its kill did not reach. Before, it could wait
  forever. The command may still be running when `mlx-guard` exits, as it already was during the wait.
- The outcome lines no longer say the command had ended when the only evidence is an empty process
  group. They say that TERM or KILL found no process left in the group, and name the command's exit
  only when the report records it.

## What did not change

Exit codes and their meanings, report schema v1, the command-line flags, and the Python
configuration objects are unchanged. A `run` that stays well under its limit prints what it printed
before, unless the launch line applies. Warnings never change when a command is stopped; your
limits do that, as before.

## Evidence

0.5.0 re-captured the reference calibration and soak on the M1 Max 32 GB MacBook Pro on macOS 27.2
(26B123), under `evidence/v0.5.0/m1-max-32gb/`. Every bound passed. One bound changed: the 2 %
sampler CPU bound failed on four 30-minute runs on 2026-11-09 (2.58 to 2.83 %). The sampler CPU
row now records its reading with no 2 % target, and the endurance test fails above 3.5 % as a
regression check. Six debug readings on 27.2 run from 2.58 % to 2.89 %, against 1.90 % and 1.93 %
on 27.0.1. A build of the 0.3.1 code read 2.83 % on 27.2, so the rise is not in the 0.5.0 code
alone, and the cause is not identified. The soak chunks show the same rise: escaping-churn went
from 2.89 % to 4.09 % under a 5 % ceiling, and real-binary from 3.10 % to 4.50 % under 8 %.
Every figure is inside its ceiling. The README, the compatibility matrix, and the packaging notes
cite this bundle. Those numbers describe one host and the commits and build profiles recorded
with them; they are not a promise for other Macs. The tutorial, the
`mlx-train-perf` proof, and the in-house trial stay under `evidence/v0.2.0/` as recorded on macOS
26.6.2. The release workflow must also pass on GitHub's `macos-15` arm64 runner before publication.
Intel Macs and other operating systems are not release targets.

## Upgrading

A script that compares stderr exactly can now see more lines on a `run`: the launch line, and the
warning lines when the footprint climbs close to the limit. Read the exit code or the report
instead; both are unchanged.

The limits from earlier releases still hold. Sampling is periodic rather than atomic, a descendant
can leave the process group, same-user hostile workloads are outside the threat model, and Metal
allocations may remain charged after termination. The IOGPU driver bug that panics macOS 26.4 and
later under Metal workloads can fire with the footprint well inside any limit, and no external
supervisor can reach it. Warnings reduce risk by giving you time to react; they do not stop a
failure. Use `observe` across repeated representative runs before choosing a destructive limit.

Start with the [tutorial](https://github.com/IonDen/mlx-guard/blob/main/TUTORIAL.md) or the
[examples](https://github.com/IonDen/mlx-guard/blob/main/docs/EXAMPLES.md), then read the
[changelog](https://github.com/IonDen/mlx-guard/blob/main/CHANGELOG.md) and the
[stability table](https://github.com/IonDen/mlx-guard/blob/main/docs/STABILITY.md).

## Fixed after 0.5.0

This fix stops a false "warning cleared" line, and a false return to normal at 0 B in the report,
after a run that crossed the warning threshold and then ended normally. The report's final footprint
can still read 0 B after such a run; this change does not address it. The reference measurements
were not re-run for this fix, because it changes only how the runtime handles a sample taken after
every process in the group has exited; the sampling and policy code are unchanged.
