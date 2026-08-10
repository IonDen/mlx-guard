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

The root and group have separate lifecycles. `wait_root` preserves the root's normal exit code or
terminating signal even when descendants remain. The supervisor retains the validated PGID after a
fast root exit so cleanup can still target the remaining group. Dropping an `OwnedProcess` sends
SIGKILL to that group and reaps the root as a final best-effort cleanup path.

This is an owned-group boundary, not arbitrary daemon containment. A descendant can call `setsid`
and escape. PID reuse and descendant identity evidence require the additional identity tracker.
Group signal delivery does not prove process exit or memory reclamation.

## Signal routing

Only SIGINT and SIGTERM are accepted as first external terminal signals. They are forwarded
unchanged to the validated owned group. The policy state machine maps a repeated terminal signal to
the separate SIGKILL group action. SIGTSTP, SIGCONT, and foreground job-control transfer are not
supported.

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
