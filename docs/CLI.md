# Command-line contract

This document defines the command syntax, runtime behavior, and process status rules.

## Commands

```text
mlx-guard observe --report PATH [OPTIONS] -- COMMAND [ARG...]
mlx-guard run --max-footprint SIZE --report PATH [OPTIONS] -- COMMAND [ARG...]
```

`run` requires `--max-footprint`. There is no automatic memory limit. `observe` has no memory or
wall-time intervention options and never terminates a command because of footprint.

The `--` separator is mandatory. Everything after it is one literal argv vector passed directly to
the operating system without a shell. At least one command token is required.

Common options:

- `--report PATH` writes schema-v1 JSON. The parent directory must already exist, belong to the
  invoking user, and have mode `0700`. Successful journals are retained; choose a unique path per
  run, or archive/remove both the report and `.<report-name>.journal` before reusing it.
- `--sample-interval DURATION`, default `50ms`, accepts `10ms..=10s`.
- `--cwd PATH` selects the child's working directory. A missing, inaccessible, or non-directory path
  is invalid before launch.
- `--clear-env` starts the child without inherited environment entries.
- `--env KEY=VALUE` sets one UTF-8 child environment value and may be repeated. Keys must be nonempty
  and unique. Values may contain `=`.
- `--on-parent-exit terminate|detach`, default `terminate`. `terminate` watches the process that
  launched `mlx-guard`. A changed `getppid()` is itself the evidence of that process's death — on
  macOS reparenting to pid 1 happens only once the real parent has exited — and the identity check
  against the recorded launcher exists only as a hedge on top of it: the check treats a single
  outcome as inconclusive (the recorded identity provably still alive) and terminates the owned group
  on every other outcome, including a disappeared, stale, or uninspectable identity. `detach` leaves
  the group running and only records the evidence. The watch is checked once per sampling-loop wake-up,
  so detection latency is bounded by `--sample-interval` — up to 10s at the maximum. A launcher that
  re-parents `mlx-guard` away from itself right after starting it — a double-fork daemonizer, or a
  shell that backgrounds `mlx-guard` and then exits — looks the same to an already-established watch
  as a parent dying unexpectedly, and the default terminates the group anyway. Pass
  `--on-parent-exit=detach` for a launcher that intends `mlx-guard` to outlive it.

The watch is not established for every run. Under the default `terminate` behavior, a launch whose
immediate parent is already `launchd` (`parent_is_launchd`) or whose launcher had already disposed
SIGHUP to `SIG_IGN` before `mlx-guard` started — the `nohup` convention (`hangup_ignored`) — skips the
watch entirely, and `terminate` has nothing to act on; `--on-parent-exit=detach` establishes the
watch in both cases anyway, since it only ever collects evidence and never acts on it. A parent whose
identity could not be established at launch (`parent_unobservable`) is never checked either way. Use
`--wall-time` as an independent backstop for a `run` whose parent watch may be off.

`run` also accepts `--wall-time DURATION`, capped at 30 days. When present, it is an enforcement
limit independent of memory.

`run` also accepts `--checkpoint-timeout DURATION`, default `1s`, within `10ms..=60s`: how long a
requested cooperative checkpoint may wait for the worker's authenticated acknowledgement before
the supervisor fails closed and sends TERM. Values below `10ms` are shorter than a scheduler
slice; values above `60s` let a runaway workload keep growing during a requested checkpoint.
Observe mode rejects the option.

## Value grammar

Byte values are positive base-10 integers followed by one case-sensitive binary suffix: `B`, `KiB`,
`MiB`, `GiB`, or `TiB`. For example, `26GiB` means `27,917,287,424` bytes. The enforcement limit must
be at least `2B`, the smallest value that permits ordered recovery, warning, limit, and emergency
bands. SI aliases such as `GB`, bare numbers, fractions, zero, negative values, and overflow are
invalid. A value too close to the `u64` maximum to add the emergency band is also invalid.

Durations are positive base-10 integers followed by `ms`, `s`, `m`, or `h`. Fractions, implicit
units, zero, negative values, other casing, and overflow are invalid.

Policy configuration comes only from CLI options. There is no config file and no
`MLX_GUARD_*` policy environment fallback. This keeps the destructive threshold visible in the
invocation. `--env` and `--clear-env` affect only the child.

## Process status

| Outcome | Exit code |
|---|---:|
| Normal child exit | unchanged child code |
| Child signal | `128 + signal` |
| Invalid command or configuration | 64 |
| Supervisor failure | 70 |
| Partial artifact failure | 74 |
| Policy intervention | 75 |
| Executable found but not runnable | 126 |
| Executable not found | 127 |

Exit 74 also covers a report path whose journal already exists. The supervisor prints
`journal target already exists; choose a new --report path` and launches nothing.
A signal that reaches an owned group with no live member means the command ended, so it does not
produce 70; see the [policy contract](POLICY.md).

A child may return a number also used by the supervisor. The typed final report distinguishes those
cases; preserving the child's actual status takes priority over making every process code unique.
Exit 70 outranks an intervention: if observation was lost, or the KILL signal itself could not be
delivered, the result is 70 even though TERM or KILL may have been sent — check the report's
signals. A TERM (or forwarded) signal that fails to deliver escalates automatically to KILL instead
and does not by itself force 70. Otherwise, once the policy requests a checkpoint or sends TERM, or
reaches KILL through its own deadline, 75 owns the result even if the command ends on its own; a
command ended by a forwarded terminal signal keeps `128+n`. When the root exits while owned-group
members survive, `run` terminates them (TERM, one-second grace, KILL) and reports the root's own
status unless KILL was needed; `observe` ends at root exit, leaves survivors running, and reports
it. When the launching parent exits under the default `--on-parent-exit=terminate`, both modes treat
that as a supervisor-initiated intervention: TERM, one-second grace, KILL if still alive, and the
result is a policy intervention (75). `--on-parent-exit=detach` never opens this path.

## Plain outcome lines

When `run` or `observe` ends with a non-zero exit, `mlx-guard` writes two more lines on stderr,
after every other line it writes at the end of the run (the stdout summary line and any diagnostic
come first):

```text
mlx-guard: what happened: <sentence>
mlx-guard: what to do: <sentence> More: <link to a section of docs/TROUBLESHOOTING.md>
```

The two lines always come as a pair. They are not printed on exit 0, for `--help` or `--version`,
or when the command ends with its own exit code that `mlx-guard` does not also use (anything but 64,
70, 74, 75, 126, or 127). A command that ends with 64, 70, 74, 75, 126, or 127 on its own gets the
lines, which say so. A command ended by a signal always gets them.

The bytes are identical on a terminal, a pipe, and a file: no colour, no wrapping that depends on
width, and no option or environment variable to turn them off. The lines are built only from the
final report, or from the error `mlx-guard` already holds when no report exists. They never contain
the command or its arguments, environment values, paths (the `--report` path included), the
executable name, or command output. The [troubleshooting page](TROUBLESHOOTING.md) explains each
case. The wording may change between releases; the prefixes, the placement, and the privacy rule are
stable (see [STABILITY](STABILITY.md)).

## Warnings during a run

Since 0.5.0, `run` prints a line on stderr when the footprint climbs close to the limit, before
`mlx-guard` has stopped anything. `observe` prints none of them, because it has no limit. There are
three lines, shown here with example values (the first ends with `More:` and a link to
[the troubleshooting page](TROUBLESHOOTING.md#warnings-during-a-run)):

```text
mlx-guard: warning: the OS-accounted footprint reached 5.42 GiB at 12.3 s, at or above the 5.40 GiB warning threshold for your 6.00 GiB limit; 2 samples in a row at or above the limit stop the command, and 1 sample at or above the 6.60 GiB emergency threshold kills it at once, with no checkpoint.
mlx-guard: warning: the OS-accounted footprint is 5.51 GiB at 5 min 12 s and has not fallen to the 4.80 GiB recovery threshold since 12.3 s; your limit is 6.00 GiB.
mlx-guard: warning cleared: the OS-accounted footprint fell to 4.79 GiB at 50.1 s, at or below the 4.80 GiB recovery threshold for your 6.00 GiB limit.
```

The first line appears when a sample first reaches the warning threshold, 10 % of the limit below the
limit (see the [policy contract](POLICY.md#runtime-defaults)). The second repeats while the footprint
has not come back down to the recovery threshold, 20 % below the limit, and it prints at most once
every 5 minutes. The third appears when the footprint does come back down, but only if an earlier
line already told you about that climb. There is no option to change the 5 minutes or to turn the
lines off.

A footprint that rises and falls often still produces at most two of these lines in any 5 minutes.
A climb that starts inside that quiet time prints nothing at first; the second line then reports it
once the time is up, and its "since" time is when the climb began. A sample at or above the emergency
threshold from a normal state kills the command at once and prints no warning.

A warning changes nothing about when the command is stopped. Once `mlx-guard` starts stopping the
command (limit, wall time, parent exit, or a signal), the warnings end and the plain outcome lines
above describe the stop. Everything the warnings say is also in the report: the `transitions` list
holds each crossing with its time and footprint.

The lines hold only numbers `mlx-guard` measured and fixed text. They never contain the command or its
arguments, environment values, paths, or command output. The bytes are the same on a terminal, in a
pipe, and in a file. Each line is written in one piece, but if the command has just printed part of a
line (a progress bar, say), the warning follows it on the same line.

Delivery is best effort. If whatever reads stderr has stopped reading, up to 8 lines wait and any
further ones are dropped, because watching memory matters more than printing about it. If the reader
starts again, it gets the waiting lines first and the two plain outcome lines after them. When the
command ends normally and no plain lines follow, `mlx-guard` waits at most a quarter of a second for
waiting lines and then exits, so they can be lost. Never rely on a warning line being delivered; read
the report. The wording may change between releases (see [STABILITY](STABILITY.md)).

### The launch line

`run` can print one more line, right after the start-of-run notice and before the command does
any work. Just before launch it reads how much memory macOS has not locked as wired (free,
speculative included, plus active, inactive, and held by the compressor). If your limit is more than
that number, it prints this and then runs the command with the limit as given:

```text
mlx-guard: warning: your limit of 30.00 GiB is more than the 28.31 GiB macOS reported as not wired just before launch, so a run that reaches it may not fit in memory on this Mac. That number is one estimate at one moment and counts every other program's memory as room. mlx-guard runs with your limit as given. More: https://github.com/IonDen/mlx-guard/blob/v0.5.0/docs/TROUBLESHOOTING.md#warnings-during-a-run
```

It never refuses to run and changes no limit. If macOS gives no number, nothing is printed. A launch
that fails prints no line, but its report still holds the number. `observe` prints nothing new.
No line does not mean the run fits. The report records the number under `prelaunch`, and records
`limit_may_not_fit` whenever the limit is more than that number, including when the launch failed
and no line was printed (see [REPORTS](REPORTS.md)). The line follows the same privacy and delivery
rules as the other warnings, and the wording may change between releases.

## Signals, terminal, and stdio

The native parent handles SIGHUP, SIGINT, and SIGTERM. The first terminal signal is forwarded
unchanged to the validated owned process group. A second terminal signal skips any remaining
checkpoint or TERM grace and requests immediate KILL. The parent continues sampling and writes the
final result before it exits when storage and scheduling remain available.

SIGHUP is captured only when it was not already disposed to `SIG_IGN` before `mlx-guard` started. A
launcher that used the `nohup` convention (`trap '' HUP`, which survives `exec`) keeps that
disposition: `mlx-guard` queries it once at startup and, if already ignored, never installs its own
handler and never forwards a SIGHUP it did not itself receive because the shell already turned it
into a no-op.

Terminal signals are checked at sampler/policy wake-ups, so forwarding latency can approach the
configured `--sample-interval`. If a child ignores the first SIGINT and remains alive through the
one-second grace, mlx-guard escalates and the final result is exit 75 `policy_intervention`; a child
that exits during the grace keeps its observed child outcome.

stdin, stdout, and stderr are inherited by default. Redirected bytes stay separate and are not
parsed as control messages. Child output remains application output and may contain arbitrary bytes;
it is never copied into the guard report or diagnostics. Guard diagnostics use stderr. If stdin is
an interactive terminal, the command receives `/dev/null` instead and the supervisor prints one line
on stderr: `mlx-guard: standard input is a terminal, so the command reads from /dev/null instead`. A
command that must read the terminal cannot run under `mlx-guard`, because the command runs in its
own process group and a read from the terminal would stop it. Any other stdin (a file, a pipe, or
an explicit `< /dev/null`) is passed on unchanged and silently. Foreground transfer, Ctrl-Z, SIGTSTP,
SIGCONT, and shell-style job control are not supported.

The start-of-run notice, the stdin notice, and the [warnings](#warnings-during-a-run) are written by
a separate thread, so a slow or stuck stderr reader cannot delay sampling. That reader delays only
the final lines: the stdout summary line is written first, and the plain outcome lines wait for any
line already being written. If the reader has gone away, `mlx-guard` ignores the failed writes. It
keeps the exit code and writes the report.

## Examples

```bash
mkdir -m 700 reports

mlx-guard observe \
  --sample-interval 100ms \
  --report reports/observe.json \
  -- python inspect_model.py

mlx-guard run \
  --max-footprint 26GiB \
  --wall-time 2h \
  --report reports/train.json \
  --clear-env \
  --env MODEL_ID=mlx-community/model \
  -- python train.py --epochs 2
```
