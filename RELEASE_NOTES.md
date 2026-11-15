# mlx-guard 0.6.0

`mlx-guard` 0.6.0 is a minor release of the alpha external runtime safety supervisor for MLX
workloads on Apple Silicon. It launches one command in an owned process group, samples the group's
OS-accounted footprint, and applies a memory limit and an optional wall-time limit from outside the
process. This release adds one thing: `mlx-guard run --max-footprint auto`, which picks the memory
limit for you. You choose it by passing `auto` instead of a size. If you keep typing a size, nothing
changes for you.

## What is new

### An automatic memory limit

Until now you had to type a size after `--max-footprint`. With `auto`, `mlx-guard` takes one reading
just before it launches your command: the memory macOS has not locked as wired. It sets the limit to
75 % of that reading, rounded down to a whole MiB, and enforces it like a size you typed.

```
mlx-guard run --max-footprint auto --report reports/run.json -- <command>
```

Auto is for catching a run that grows out of control when you do not yet know a good size. It is
not a substitute for a size you chose from what your job needs.

It is opt-in. A `run` without `--max-footprint` still stops with exit code 64, as before.

Know these limits before you use it:

- The reading is taken once, just before launch. The limit does not move afterwards.
- Memory that other programs are using, but that is not wired, counts as room. A run can push those
  programs into compression or swap before it is stopped.
- Each auto run takes 75 % of its own reading, made just before its launch. Two runs started
  together can read nearly the same number, so together they can claim more memory than the Mac has.
- It knows nothing about what your command needs. A job that needs more memory than that limit is
  stopped by it. Give it a size, with headroom above a peak you observed.
- The 75 % share was chosen on one 32 GB M1 Max, and only tried there. On that Mac the automatic
  limit sat above the memory free at launch in every reading. Other memory sizes are not verified.
- A later release may change the share. It would do so under a new rule id, and the changelog would
  say so.

Auto reduces the risk that a runaway uses up this Mac's memory. It does not prevent slowdowns,
paging, crashes or a kernel panic.

`run` prints one `mlx-guard: automatic limit:` line after the start-of-run notice, and a run stopped
at that limit says the limit was chosen automatically. If macOS gives no usable number, or the limit
it would pick is under the 1 GiB minimum, `run` stops with exit code 64 and asks for a size. Nothing
is launched and no report is written.

### Two report fields

Every `run` report now has `configuration.max_footprint_source`, either `explicit` (you typed a
size) or `auto`. An automatic run also has `prelaunch.auto_limit`, with the rule name and the limit
in bytes. Report schema v1 is unchanged.

### Python

`RunConfig(max_footprint_bytes="auto")` does the same from Python. The field is now typed
`int | Literal["auto"]` and is still required. Code that does arithmetic on it needs a type check.
An automatic run that stops before launch with exit code 64 raises `ConfigurationError`. The usual
cause is that macOS gave no usable memory number, or one too small for the 1 GiB minimum; other
refusals before launch raise it too.

## What did not change

For an accepted size, the stderr and the enforced policy are the same as in 0.5.2. The one
difference is in the report, which gains `max_footprint_source: "explicit"`. Exit codes and their
meanings, the other flags, the defaults and report schema v1 are unchanged.

One printed line did change: the advice for a rejected or missing `--max-footprint` now also offers
`auto`.

## Evidence

There are no new reference measurements for the supervisor itself. The sampler and the policy state
machine are unchanged. The 0.5.0 reference bundle under `evidence/v0.5.0/m1-max-32gb/` still
applies. Its numbers describe one host and the commits and build profiles recorded with them; they
are not a promise for other Macs. The release workflow runs on GitHub's `macos-15` arm64 runner.
Intel Macs and other operating systems are not release targets.

## Upgrading

If you type an accepted size, you will see no change in what is enforced or printed. Your report
gains one field, `max_footprint_source: "explicit"`. If you read reports with a strict parser, allow
it. Python code that does arithmetic on `RunConfig.max_footprint_bytes` needs a check for `"auto"`.

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
