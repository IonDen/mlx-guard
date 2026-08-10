# Observe and calibration

Use `mlx-guard observe` to measure a workload without enforcing a limit. It reads from the same
bounded process and footprint sampler as enforcement. It never builds a memory or wall-time policy,
requests a checkpoint, or sends TERM/KILL. The core path and real-process fixtures work today. The
command-line runtime is still under development.

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
