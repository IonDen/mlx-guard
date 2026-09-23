# mlx-guard 0.3.0

`mlx-guard` 0.3.0 is the third alpha of the external runtime safety supervisor for MLX workloads on
Apple Silicon. It launches one command in an owned process group, samples the group's
macOS-accounted footprint, and applies an explicit memory limit and optional wall-time limit from
outside the process. This is a small release about the first minutes with the tool: the commands in
the README now work when you type them into Terminal, and the documentation follows one real job from
the first run to the fix.

One behavior change needs attention when upgrading. Through 0.2.0, a terminal on the supervisor's
standard input refused the launch with exit 64 and `interactive terminal input is unsupported`,
before anything started. Every quick-start command did exactly that when typed into Terminal, and a
retry with the same report path then exited 74. From 0.3.0 the supervised command reads `/dev/null`
instead, and the supervisor prints one line on stderr:

```
mlx-guard: standard input is a terminal, so the command reads from /dev/null instead
```

A file, a pipe, or an explicit `< /dev/null` still reaches the command unchanged, and nothing extra
is printed, so scripts that already redirect standard input see no difference. The Python client
behaves the same way, because it passes the calling script's standard input to the supervisor. Shell
job control is still unsupported: the command runs in its own process group, so a program that must
read the keyboard cannot run under `mlx-guard`.

The new tutorial, `TUTORIAL.md`, follows one ordinary job through the guard: a local-LLM document
summarizer with a real memory leak, run under `observe`, then under a footprint limit, then with a
cooperative checkpoint and a resume loop, and finally fixed. Its transcripts and reports were
recorded on the reference host. The README now opens with that recorded intervention and a figure
drawn from its report, puts the quick start in numbered steps, and adds a table that says, for each
exit code, what happened and what to do next. The Python API guide says when a library may fall back
to running a command without the guard, and what a worker should do when its checkpoint callback
fails.

`mlx-train-perf` 0.8.0 is the first library to ship an `mlx-guard` integration. Its proof was
re-recorded on the published 0.2.0 wheel and lives under `evidence/v0.2.0/mlx-train-perf/`: a
checkpointed wall-time intervention with the saved partial artifact and matching request ids, plus
the failure paths the library's own tests pin.

No sampling, policy, identity, or intervention code changed in this release, so the 0.2.0
measurements under `evidence/v0.2.0/` still describe it: the M1 Max 32 GB reference bundle, the
two-hour soak, and the escalation envelope. The reference-host evidence is from macOS 26.6.2. The
release workflow must also pass on GitHub's `macos-15` arm64 runner before publication. Intel Macs
and other operating systems are not release targets.

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
