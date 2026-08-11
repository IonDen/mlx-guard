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
  invoking user, and have mode `0700`.
- `--sample-interval DURATION`, default `50ms`, accepts `10ms..=10s`.
- `--cwd PATH` selects the child's working directory. A missing, inaccessible, or non-directory path
  is invalid before launch.
- `--clear-env` starts the child without inherited environment entries.
- `--env KEY=VALUE` sets one UTF-8 child environment value and may be repeated. Keys must be nonempty
  and unique. Values may contain `=`.

`run` also accepts `--wall-time DURATION`, capped at 30 days. When present, it is an enforcement
limit independent of memory.

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
Once a policy intervention begins, exit 75 owns the result even if TERM or KILL ends the child.

## Signals, terminal, and stdio

The native parent handles SIGINT and SIGTERM. The first terminal signal is forwarded unchanged to
the validated owned process group. A second terminal signal skips any remaining checkpoint or TERM
grace and requests immediate KILL. The parent continues sampling and writes the final result before
it exits when storage and scheduling remain available.

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
