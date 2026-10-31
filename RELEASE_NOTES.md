# mlx-guard 0.4.0

`mlx-guard` 0.4.0 is a minor release of the alpha external runtime safety supervisor for MLX
workloads on Apple Silicon. It launches one command in an owned process group, samples the group's
OS-accounted footprint, and applies an explicit memory limit and optional wall-time limit from
outside the process. This release changes how a finished run is described: when a run ends with a
non-zero exit, `mlx-guard` now says what happened and what to do.

When `run` or `observe` ends with a non-zero exit, stderr carries two more lines after everything
printed before:

```
mlx-guard: what happened: …
mlx-guard: what to do: … More: <link>
```

They explain the outcome from the report and link to the matching section of the new
[troubleshooting page](https://github.com/IonDen/mlx-guard/blob/main/docs/TROUBLESHOOTING.md). They
never contain the command, environment values, paths, or command output, and there is no option to
turn them off. This includes exits of `128+n` after a signal. A command's own exit code that
`mlx-guard` does not also use prints nothing. A contract parse error (exit `64`) now ends its line
with a newline before the new lines.

The Python client exposes the same text as `RunResult.explanation` (`str | None`, keyword-only,
default `None`). It holds the two lines without the `mlx-guard: ` prefix, and it is `None` when no
lines were printed. Outcomes with no report still raise their existing errors.

## What did not change

Exit codes and their meanings, the stdout summary line, and report schema v1 are unchanged. So are
sampling, the policy, identity tracking, and intervention: the lines are written after the last
sample, from the report the run already produced.

## Evidence

The reference measurements remain the 0.3.1 bundle, captured on the M1 Max 32 GB MacBook Pro on
macOS 27.0.1 (26A434), under `evidence/v0.3.1/m1-max-32gb/`. They were not repeated, because this
release changes how a finished run is described, after the last sample, and not sampling, policy,
identity tracking, or intervention. The README, the compatibility matrix, and the packaging notes
still cite that bundle. Those numbers describe one host, one commit, and one build profile; they are
not a promise for other Macs. The tutorial, the `mlx-train-perf` proof, and the in-house trial stay
under `evidence/v0.2.0/` as recorded on macOS 26.6.2. The release workflow must also pass on GitHub's
`macos-15` arm64 runner before publication. Intel Macs and other operating systems are not release
targets.

## Upgrading

Two lines more on stderr is the one change you can notice. A script that asserts an empty stderr, or
compares it exactly, on a failing run will see them. Read the exit code or the report instead; both
are unchanged.

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
