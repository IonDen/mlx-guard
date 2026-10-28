# mlx-guard 0.3.1

`mlx-guard` 0.3.1 is a patch release of the alpha external runtime safety supervisor for MLX
workloads on Apple Silicon. It launches one command in an owned process group, samples the group's
OS-accounted footprint, and applies an explicit memory limit and optional wall-time limit from
outside the process. The intervention you got is the one reported: a few paths that used to end in a
misleading outcome or a confusing error now end in the right one.

The main fix concerns a command that has already ended. During an escalation, a TERM, KILL, or
forwarded signal that reaches an owned group with no live member now counts as the command having
ended: the intervention stands, `child_status` records the exit, and the exit code is `75`. Before,
a worker that was still tearing down could turn a working intervention into `supervisor_failure`
(exit `70`), because Darwin refuses a group signal with `EPERM` while every member has exited but
none is reaped yet. A refusal against a group that still has a live member, or that cannot be
scanned, stays a failure (`70`). This applies to `run`. `observe` still records a group signal as
delivered or refused, without the liveness check.

Two smaller changes make errors easier to act on. `import mlx_guard` now succeeds when the package
metadata is missing (vendored copies, broken installs, some frozen layouts). In that case `start()`,
`supervisor_argv()`, and `load_report()` raise `SupervisorDiscoveryError`, and `start()` does so
before it launches anything, so the documented fallback to a direct launch covers it. Reading
`mlx_guard.__version__` without the metadata raises `importlib.metadata.PackageNotFoundError`; before,
the import itself failed. And the exit-`74` message for a report path that is already in use now
names the fix:

```
journal target already exists; choose a new --report path
```

`CheckpointWorker.poll()` now raises `CheckpointCallbackError` with the callback's own exception as
`__cause__`, so a traceback shows why the save failed. The error message is unchanged.
`KeyboardInterrupt` and `SystemExit` raised inside the callback still send the failed
acknowledgement, then propagate unchanged. `scripts/calibrate-host.sh` now renders core counts
correctly on macOS 27, which reports them in a new four-part form; the bug only affected the
provenance line of a calibration capture.

The documentation states one limit more plainly. If the supervisor itself is killed with SIGKILL,
the supervised command keeps running: it is reparented to launchd in its own process group, only the
hidden `.<report-name>.journal` remains, and no final report is written. A launcher that stops a
guarded run should send a catchable signal (TERM or INT), which the supervisor forwards. The Python
API guide also says to wait, with a bound, after `GuardProcess.cancel()` before the calling process
exits.

## Re-measured on macOS 27.0.1

The reference measurements were captured again on the M1 Max 32 GB MacBook Pro, now running macOS
27.0.1 (26A434), from a clean tree: the six-chunk calibration, the escalation envelope, and the
four-chunk soak, all under `evidence/v0.3.1/m1-max-32gb/`. The builds are debug builds from the
commit before the version bump, so they report 0.3.0; the supervisor code is identical to this
release. Every bound holds. The
anonymous 64 MiB mapping error is 32,816 B (0.049%), the 16-member sample-window p95 is 0.186 ms,
external TERM to final report is 56.9 ms at p95, the 30-minute endurance run used 1.90% of one core
and grew 80 KiB, and the four safe workloads saw no false intervention. Two figures moved against
the macOS 26.6.2 capture: the Metal visible-delta error is 4.03% of the 64 MiB buffer (3.03% in
0.2.0, target 10%), and the sampler CPU share is 1.90% (1.59% in 0.2.0, target 2%), the bound with
the least room left. The escalation envelope p95s are 27 ms for the acknowledgement, 27 to 34 ms for
TERM to quiet, and 28 ms for KILL to quiet. These numbers describe one host, one commit, and one
build profile; they are not a promise for other Macs.

The calibration bundle's `provenance.json` reads "10 (0 Performance and 8 Efficiency)" because the
capture script misread the core-count field. The host has 8 performance and 2 efficiency cores, the
measurements do not depend on the string, and the file is left as captured. The bundle's README
says so.

The tutorial, the `mlx-train-perf` proof, and the in-house trial were not repeated; they stay under
`evidence/v0.2.0/` as recorded on macOS 26.6.2. The release workflow must also pass on GitHub's
`macos-15` arm64 runner before publication. Intel Macs and other operating systems are not release
targets.

## Upgrading

Exit codes and their meanings are unchanged, and report schema v1 is unchanged. Two things to know:
the exit-`74` message for a report path already in use has new text, so a script that matches the old
wording needs updating, and in `run` a `permission_denied` signal record now means a live member
remained (a group with no live member is recorded as `process_missing`). `observe` records group
signals as before.

The limits from earlier releases still hold. Sampling is periodic rather than atomic, a descendant
can leave the process group, same-user hostile workloads are outside the threat model, and Metal
allocations may remain charged after termination. The IOGPU driver bug that panics macOS 26.4 and
later under Metal workloads can fire with the footprint well inside any limit, and no external
supervisor can reach it. Use `observe` across repeated representative runs before choosing a
destructive limit.

Start with the [tutorial](https://github.com/IonDen/mlx-guard/blob/main/TUTORIAL.md) or the
[examples](https://github.com/IonDen/mlx-guard/blob/main/docs/EXAMPLES.md), then read the
[changelog](https://github.com/IonDen/mlx-guard/blob/main/CHANGELOG.md) and the
[stability table](https://github.com/IonDen/mlx-guard/blob/main/docs/STABILITY.md).
