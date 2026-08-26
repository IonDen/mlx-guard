# Policy contract

Policy contract version 1 is a pure state machine driven by ordered monotonic events. Only the sampled,
OS-accounted aggregate footprint can trigger a memory intervention. An optional wall-time limit, a
forwarded terminal signal, the root command exiting with owned-group survivors, and the launching
parent exiting are the machine's other destructive inputs. Pressure, swap, compressor, wired-memory,
and growth-rate metrics are advisory and cannot change state. MLX's own counters are not read at all
in v0.1.

## States and transitions

| Current state | Event | Condition | Next state | Requested action |
|---|---|---|---|---|
| observe | valid sample | any footprint | observe | record only |
| normal | valid sample | warning threshold or higher | warning | record only |
| warning | valid sample | recovery threshold or lower | normal | record only |
| warning | valid sample | required consecutive limit breaches | checkpoint-requested or terminating | request checkpoint or send TERM |
| normal or warning | valid sample | emergency threshold or higher | emergency | send KILL immediately |
| normal or warning | tick | wall limit reached | checkpoint-requested or terminating | request checkpoint or send TERM |
| checkpoint-requested | nonce- and request-matching acknowledgement | before timeout | terminating | send TERM |
| checkpoint-requested | tick | checkpoint deadline reached | terminating | send TERM |
| checkpoint-requested | checkpoint setup or delivery failure | always | terminating | send TERM |
| terminating | tick | TERM grace reached | emergency | send KILL |
| terminating | TERM delivery failure | always | emergency | send KILL |
| emergency | KILL delivery failure | always | supervisor-error | report typed failure |
| any active state | repeated terminal signal | intervention already started | emergency | send KILL |
| any active state | process exit | always | exited | report observed result |
| normal, warning, or checkpoint-requested | root exited with owned-group survivors | always | terminating | send TERM (cleanup) |
| observe, terminating, emergency, supervisor-error, or exited | root exited | always | unchanged | none |
| normal or warning | launching parent exited | always | terminating | send TERM (parent-exit shutdown) |
| checkpoint-requested | launching parent exited | always | unchanged | none (checkpoint continues) |
| observe, terminating, emergency, supervisor-error, or exited | launching parent exited | always | unchanged | none |

Root-exit cleanup acts from `checkpoint-requested` because the worker that would have acknowledged
the request is already gone. Parent-exit shutdown deliberately does not: from `checkpoint-requested`
it is a no-op, because the worker is still running and was promised its acknowledgement window, and
the parent's death does not change what that workload is doing. A suppressed `ParentExited` produces
no transition and no signal; the evidence that it happened is `outcome.parent_exited_at_ms`.

Values between recovery and warning retain the previous normal or warning state. A sample at or
above the ordinary limit contributes to the consecutive-breach count. A sample below that limit
resets the count. A sample at or above the emergency threshold skips checkpoint and TERM grace.
Overshoot is recorded as aggregate footprint minus the configured limit. Each checkpoint action
includes its state-machine deadline, so the runtime does not reconstruct or extend it.

## v0.1 runtime defaults

The CLI derives a band step as the larger of one byte and one tenth of `--max-footprint`, rounded
down. Warning is `limit - step`, recovery is `limit - 2 * step`, and emergency is `limit + step`.
The limit must be at least `2B` so these bands remain strictly ordered. Two consecutive samples at or
above the limit start the checkpoint or TERM path. Three consecutive unusable samples trigger the
mode-specific observation failure policy.

The checkpoint acknowledgement timeout defaults to one second and accepts `--checkpoint-timeout`
within `10ms..=60s`. The former 100ms default was missed by a real cooperative worker on a loaded
3-CPU machine, and interventions happen under exactly that kind of pressure; the timeout still
fails closed, so an unresponsive worker receives TERM when it expires. TERM grace is one second.
Maximum sample age is twice `--sample-interval`; maximum collection-window width equals the
interval. TERM grace and the sampling-derived maxima are not configurable. The effective values
are recorded in each report.

## Escalation envelope

| Scenario | Measured interval | p95 (ms) | Maximum (ms) |
|---|---|---:|---:|
| `checkpoint_ack_idle` | request to acknowledgement | 29 | 29 |
| `checkpoint_ack_idle` | TERM to quiet | 27 | 28 |
| `checkpoint_ack_loaded` | request to acknowledgement | 29 | 30 |
| `checkpoint_ack_loaded` | TERM to quiet | 28 | 36 |
| `group_term` | TERM to quiet | 37 | 38 |
| `group_kill` | KILL to quiet | 31 | 31 |

Measured on the M1 Max 32 GB reference host (macOS 26.6.1, 25G76), 20 repetitions per scenario,
`resolution_ms: 10` — every mark is a supervisor-loop timestamp quantized to that interval, not an
instantaneous event time. `*_to_quiet` marks the loop observing the root reaped and the owned
group empty, never the reap instant itself. The measured binaries are unoptimized debug builds, so
these numbers characterize the supervision path's timing shape rather than an optimized release
build. The `checkpoint_ack_loaded` background is parked process-table load rather than CPU
starvation, so the idle/loaded pair does not show ack latency under CPU contention; the shared-VM
workflow below covers the CPU-starved case. `group_term`'s TERM-to-quiet interval is computed by
the same derivation
`reference_runtime_calibration.rs` publishes as `finalization_latency_milliseconds` — first
delivered group TERM to observed-quiet — but measured under different conditions (a sixteen-member
group, a wall-time trigger, and 10 ms sampling here, versus a single ramp worker, a footprint
trigger, and 50 ms sampling there), so the two figures are related by construction, not
interchangeable. The full raw record is `evidence/v0.2.0/m1-max-32gb/escalation-envelope.json`.

A dispatch-only workflow captures the same artifact on GitHub's shared macos-15 VM (3 vCPU / 7 GB)
as corroboration; those runs are labeled shared-VM, pooled across at least five captures, and
never gate releases.

## Measurement quality and clocks

A sample is usable only when it contains an aggregate, was captured no later than it was processed,
is no older than the configured maximum age, and fits inside the configured collection window. A
usable sample resets the missing-sample count. Missing, stale, reversed, or overly wide samples do
not count as zero.

At the configured consecutive-missing limit, observe mode stops with a supervisor error but does not
signal the command. Enforcement mode fails closed: it sends TERM, enters supervisor-error, and later
escalates to KILL if the process remains alive. A monotonic clock regression causes the same failure
immediately because existing deadlines can no longer be trusted.

Deadlines use elapsed monotonic time, so sleep counts toward wall, checkpoint, and TERM limits. One
input event advances at most one cooperative safety phase. After a long sleep, the first observed
deadline sends TERM; a later tick may send KILL. This preserves action order while avoiding an
unbounded grace extension.

## Checkpoint evidence

Checkpoint support is negotiated before enforcement begins. Delivery of a request alone is
`requested_unverified`; it is not success. Only a nonce- and request-matching worker acknowledgement
allows the state machine to record `acknowledged_unverified_durability`. Even that acknowledgement
does not prove that checkpoint bytes are complete or durable. A missing, late, mismatched, or
non-matching acknowledgement cannot delay TERM beyond the checkpoint timeout.

Checkpoint setup, channel, endpoint, or worker failures are distinct from a command that never
negotiated checkpoint support. Both paths proceed to TERM, but the recorded disposition remains
different.

A root that exits mid-request abandons the request; its status stays `requested_unverified` and
the cleanup TERM carries reason `root_exit_cleanup`.

The supervisor continues sampling after TERM or KILL when possible. The final report counts these
post-signal observations and records a final footprint only when one was actually measured. Signal
delivery itself never proves process exit, footprint reclamation, or Metal reclamation.

## Golden timelines

| Scenario | Ordered result |
|---|---|
| `95, 85, 80` with warning `90`, recovery `80` | warning, warning, normal |
| `100, 101` with two required breaches | record, then request checkpoint with 1 byte overshoot |
| wrong request ID, wrong nonce, matching acknowledgement | ignore, ignore, send TERM |
| checkpoint timeout, then TERM-grace timeout | send TERM, then send KILL |
| checkpoint delivery failure | send TERM with checkpoint-failure disposition |
| TERM failure, then KILL failure | send KILL, then report terminal supervisor error |
| aggregate `151` with limit `100`, emergency `150` | record, send KILL, record 51-byte overshoot |
| stale, wide, missing with allowance `3` | record each missing result, then fail according to mode |

These timelines are executable in `crates/mlx-guard-core/tests/policy_timelines.rs`.
