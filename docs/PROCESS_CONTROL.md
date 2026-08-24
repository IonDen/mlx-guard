# Process-control contract

The native supervisor launches one literal argv vector with `Command`, never a shell. The executable
is argv element zero; remaining elements are passed unchanged. Working directory, environment
clearing and overrides, and all three standard streams are applied before exec.

## Owned process group

The child is configured to lead a new Unix process group before exec. The supervisor stores only a
positive root PID and confirms that a live initial PGID equals that PID. If the root finishes before
the parent can query it, the successful pre-exec setup and retained root status complete validation.
A launch that cannot establish either result fails. Public process-control methods cannot construct
or signal PID or PGID zero.

If the root exits before the supervisor's first identity inspection can bind `(pid, start_abstime)`,
the supervisor preserves the child's exit status and writes a valid report with zero samples and
identity unknown. Only an inspect failure on a still-live child is a supervisor failure (exit 70).

The root and group have separate lifecycles. `wait_root` preserves the root's normal exit code or
terminating signal even when descendants remain. The supervisor retains the validated PGID after a
fast root exit so cleanup can still target the remaining group. Dropping an `OwnedProcess` sends
SIGKILL to that group and reaps the root as a final best-effort cleanup path.

This is an owned-group boundary, not arbitrary daemon containment. A descendant can call `setsid`
and escape. PID reuse and descendant identity evidence require the additional identity tracker.
Group signal delivery does not prove process exit or memory reclamation.

## Root exit with survivors

When the root command's own process exits while other members of its owned group are still
running, the supervisor no longer treats that as an unexplained loss of the thing it is watching.
`run` sends SIGTERM to the group with reason `root_exit_cleanup`, waits the usual one-second grace
period, and escalates to SIGKILL if a member is still alive once that grace period ends. The root's
own exit status is still the reported result unless KILL was needed, in which case the run is
reported as a policy intervention.

This is a real behavior change to plan around: a command that intentionally starts background work
and exits while leaving it running will now have that work terminated. Keep the launching process
alive for as long as its children should keep running, or supervise it with `mlx-guard observe`,
which ends at root exit and leaves any survivors alone.

The cleanup TERM can race an owned-group member that is already exiting on its own. When the kernel
no longer has a target by the time the signal is sent, the supervisor records that attempt with
result `process_missing` rather than treating it as a failure; it is not evidence that anything
went wrong.

## Signal routing

SIGHUP, SIGINT, and SIGTERM are accepted as first external terminal signals. They are forwarded
unchanged to the validated owned group. The policy state machine maps a repeated terminal signal to
the separate SIGKILL group action. SIGTSTP, SIGCONT, and foreground job-control transfer are not
supported.

SIGHUP capture is conditional. `hangup_is_ignored` queries the current SIGHUP disposition with a
null new-action `sigaction` call — a read, never a write — before any handler is installed. When the
disposition is already `SIG_IGN`, the process was launched under the `nohup` convention (a shell's
`trap '' HUP` survives `exec` as `SIG_IGN`), and that is read as deliberate intent for the run to
outlive its launcher. `TerminalSignalMonitor::install` honors it: it installs its own handler for
SIGINT and SIGTERM as usual but skips installing one for SIGHUP, leaving the inherited `SIG_IGN` in
place instead of overriding it. The same probe result also feeds the launching-parent watch
described in the [CLI contract](CLI.md): under the default `terminate` behavior a `nohup`-style
launcher disables the watch entirely (`parent_watch: hangup_ignored`), the same as it disables SIGHUP
forwarding; under `--on-parent-exit=detach` the watch is established anyway, since detach only ever
collects evidence and never acts on it.

Checkpoint delivery has a different target type. A cooperative endpoint must be negotiated as a
positive, live member of the owned group. Membership is checked when the endpoint is created and
again immediately before delivery. The checkpoint signal targets that PID only. TERM and KILL never
use the endpoint target. The inherited channel and FD-only readiness handshake are defined in the
[checkpoint protocol](CHECKPOINT_PROTOCOL.md); no checkpoint signal is enabled before readiness.

## Terminal and stdio behavior

The CLI inherits stdin, stdout, and stderr. If inherited stdin is a terminal, preflight rejects the
launch because v0.1 does not implement foreground process-group transfer and restoration. Piped and
null streams are available to integrations and tests. Child stdout and stderr remain application
data and are never interpreted as checkpoint or supervisor control messages.

## Launch and wait results

Preflight and exec failures distinguish empty argv, invalid cwd, interactive terminal, missing
executable, non-executable file, generic spawn failure, and process-group validation failure. Error
messages omit argv and paths. Root completion distinguishes `Exited(code)` from `Signaled(signal)`.
Signal attempts distinguish delivered, missing target, permission denial, and unexpected system-call
failure.

Real-process acceptance tests cover redirected I/O, literal shell metacharacters, cleared and
overridden environment, cwd, failed exec, a real pseudo-terminal, fast root exit, normal and signaled
root outcomes, TERM-resistant work, SIGINT forwarding, endpoint-only checkpoint delivery, and
kill-on-drop cleanup.
