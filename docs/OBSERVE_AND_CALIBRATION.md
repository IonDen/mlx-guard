# Observe and calibration

Use `mlx-guard observe` to measure a workload without enforcing a limit. It reads from the same
bounded process and footprint sampler as enforcement. It never builds a memory or wall-time policy,
requests a checkpoint, or sends TERM/KILL because of footprint. Observe ends when the root command
exits and reports its status. Owned-group members still running at that point are neither signalled
nor waited for; the report marks `owned_group_survivors` and stderr says so. Because observe stops
at root exit, the reported final footprint is the last sample taken before the root exited, not a
fresh measurement of whatever survivors are left running. If three consecutive samples are
unusable, observe writes a supervisor-error report and stops without signalling the command.

Terminal signals and the launching parent exiting are observe's only destructive exceptions, and
they end the run two different ways. A forwarded SIGHUP, SIGINT, or SIGTERM reaches the owned group
unchanged, with no grace timer; observe keeps sampling and the run ends when the root exits,
reporting the root's own signaled status (`128+n`), not a policy intervention. A second terminal
signal skips that and requests an immediate KILL instead. Under the default
`--on-parent-exit=terminate`, the process that launched `mlx-guard` exiting takes the other path:
TERM to the owned group, a one-second grace, KILL if it is still alive, and the run is reported as a
policy intervention (exit 75) instead of a plain root exit. `--on-parent-exit=detach` turns the
parent-exit case back into the natural-root-exit case above: the group is left running and the
report only records that the parent was gone.

## Advisory measurements

Each advisory field records a scope, public API source, observation timestamp, freshness, and a
typed value state. They provide context, not worker-attributable enforcement inputs.

| Field | Scope | Source | Initial behavior |
|---|---|---|---|
| Pressure event count and level | System | Dispatch memory-pressure source | `unknown` until the first event |
| Swap bytes used | System | `sysctlbyname("vm.swapusage")` | value, typed denial, or unsupported |
| Compressor bytes | System | `host_statistics64(HOST_VM_INFO64)` | value or typed error |
| Wired bytes | System | `host_statistics64(HOST_VM_INFO64)` | value or typed error |
| Footprint growth rate | Owned process group | Consecutive complete footprint samples | `unknown` until two samples; stale after a gap |

Pressure notifications are best effort and system-wide. An absent first event is not “normal.” A
warning or critical event produces a pre-launch warning, but v0.1 does not invent a rejection
threshold. The same applies when footprint or advisory capabilities are unavailable.

## Calibration artifact

An observe run records total, complete, and incomplete sample counts; observed duration; the highest
complete aggregate footprint; and the highest positive growth rate. The artifact always records
`observation_only: true`, `safety_certified: false`, and no automatic limit.

Use the artifact to choose a limit deliberately:

1. Repeat the intended workload with representative models, batch sizes, concurrency, data, and
   other applications running on the Mac.
2. Investigate partial, stale, clock-error, or capability-error samples. Do not treat their known
   subtotal as a peak.
3. Choose an explicit limit above the highest repeatable complete peak, with operator-selected
   headroom for run-to-run variation and growth between sample intervals. No fixed percentage is
   universally safe.
4. Validate the chosen limit in staging and recalibrate after workload, MLX, macOS, or hardware
   changes.

Observation reduces guesswork; it does not certify that a workload is safe or that macOS will remain
responsive under every system-wide condition.
