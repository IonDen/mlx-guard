# Command-line contract

This document defines the v0.1 command syntax, runtime behavior, and process status rules.

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
  launched `mlx-guard` and terminates the owned group when it is confirmed gone; `detach` leaves the
  group running and only records the evidence. The watch is checked once per sampling-loop wake-up,
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

Policy configuration comes only from CLI options in v0.1. There is no config file and no
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
an interactive terminal, the command is rejected with exit 64 before worker launch. Foreground
transfer, Ctrl-Z, SIGTSTP, SIGCONT, and shell-style job control are not supported in v0.1.

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
