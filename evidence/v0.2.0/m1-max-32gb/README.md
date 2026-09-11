# M1 Max 32 GB reference evidence

This bundle records the bounded calibration run for commit
`59ea303d9bf341f0894b3ce5864672b74d382d01`. The worktree was clean when the run started. The host
was a 10-core (8 performance, 2 efficiency) Apple M1 Max MacBook Pro with 32 GB memory, running
macOS 26.6.2 (25G83) on arm64. Tool versions and the
sanitized hardware record are in [provenance.json](provenance.json) and repeated inside
[footprint.json](footprint.json).

These measurements describe this host, this commit, and this build profile. They do not define a
universally safe memory limit. Footprint values are multi-call samples of OS-accounted physical
footprint, not instantaneous machine-wide memory totals.

Captured 2026-09-11 with:

```bash
./scripts/calibrate-reference-host.sh /private/tmp/mlx-guard-v0.2.0-calibration
```

The script runs the six measurements below as chunks, each written the moment it finishes, and
resumes an interrupted run by skipping the chunks that already passed; its cargo transcripts stay
in a sibling `.logs` directory that is not committed. The [soak bundle](soak/README.md) next to
this one records the supervisor's own resource use over long escaping-churn runs on the same host.

## Results

| Measure | Target | Observed | Result |
|---|---:|---:|---:|
| Anonymous 64 MiB maximum error | 1% or less | 49,224 B (0.073%) | Pass |
| Live anonymous-shared 64 MiB maximum error | 1% or less | 16,432 B (0.024%) | Pass |
| Metal 64 MiB maximum visible-delta error | 10% or less | 2,031,688 B (3.027%) | Pass |
| `proc_pid_rusage` call p95 | 1 ms or less | 1.292 us | Pass |
| 16-member sample-window p95 | 10 ms or less | 0.180 ms | Pass |
| 50 ms sampler CPU, 16 members | 2% of one core or less | 1.5927% | Pass |
| Supervisor maximum RSS | 20 MiB or less | 3.06 MiB | Pass |
| 30-minute footprint growth | 2 MiB or less | 96 KiB | Pass |
| Decision to first signal p95 | 10 ms or less | 9.333 us | Pass |
| External TERM to final report p95 | 100 ms or less | 51.193 ms | Pass |
| External INT to final report p95 | 100 ms or less | 66.170 ms | Pass |
| 128 MiB/s ramp overshoot p95 | 16 MiB or less | 13,484,416 B (12.86 MiB) | Pass |

The 30-minute run retained exactly 256 samples; its raw minute observations are in
[endurance.json](endurance.json). The Metal allocation remained charged immediately after release
in all 10 runs, with a maximum residual of 69,140,552 bytes. None of the
20 ramp runs produced a lower numeric footprint sample after signaling; those
observations are right-censored and do not support a prompt-reclamation claim. Raw ramp and
external-signal timings are in [runtime.json](runtime.json); nanosecond decision-to-signal samples
are in [intervention.json](intervention.json).

## Lifecycle and safe workloads

[scenarios/scenarios.json](scenarios/scenarios.json) summarizes normal exit, observe-only,
acknowledged checkpoint, checkpoint timeout, TERM, KILL, fast root exit, owned-group cleanup,
session escape, and storage loss. The accompanying `reports/` directory contains the final reports
and durable journals. Four safe workloads completed with zero false interventions.

Two scenarios read differently from the 0.1.0 bundle by design. `root-fast-exit` now ends with
the root's own status after survivor cleanup (exit 23), where 0.1.0 recorded a
measurement-loss intervention (exit 75). `checkpoint-timeout` pins `--checkpoint-timeout 100ms`
because the default acknowledgement deadline rose from 100 ms to 1 s in 0.2.0 and the scenario's
worker withholds its acknowledgement for 500 ms; the scenario still records the missed deadline it
is named for.

## Escalation envelope

[escalation-envelope.json](escalation-envelope.json) records the checkpoint- and
signal-escalation timing envelope, captured by the same run. All values are milliseconds, 20
repetitions per scenario, `resolution_ms: 10`.

| Scenario | request_to_ack p95 / max | term_to_quiet p95 / max | kill_to_quiet p95 / max | timed_out | escalated_to_kill |
|---|---:|---:|---:|---:|---:|
| `checkpoint_ack_idle` | 31 / 31 | 33 / 33 | — | 0 | 0 |
| `checkpoint_ack_loaded` | 29 / 30 | 24 / 25 | — | 0 | 0 |
| `group_term` | — | 39 / 41 | — | 0 | 0 |
| `group_kill` | — | — | 36 / 39 | 0 | 20 |

`checkpoint_ack_idle` and `checkpoint_ack_loaded` request a cooperative checkpoint from an
acknowledging worker and then TERM it once the acknowledgement lands. `group_term` sends TERM to a
sixteen-member fanout-stall group with the default disposition and observes the first escalation
step end it. `group_kill` sends TERM to a TERM-deaf sixteen-member group and observes every
repetition escalate to KILL; it publishes no `term_to_quiet` interval, since a killpg delivered to
a group that never responds to TERM would only measure noise.

### Honesty disclosures

- All envelope marks are supervisor-loop timestamps quantized by the 10 ms sample interval. The
  `request_to_ack`, TERM, and KILL stamps are policy event times, not delivery times; "quiet" is
  the loop observing the root reaped **and** the owned group empty, never the reap instant itself.
- The measured binaries are unoptimized debug builds run through `cargo test`. These numbers
  characterize the supervision path's timing shape, not an optimized release build.
- The `checkpoint_ack_loaded` arm's background load is a sixteen-member `fanout-stall` group whose
  members park rather than spin, so this is process-table load, not CPU-bound work; the raw
  `load_at_start_per_repetition` samples in the JSON show the 1-minute load each repetition
  started under (1.40 to 3.70 across the four scenarios). Genuinely CPU-starved corroboration is
  what the shared three-vCPU-VM captures in
  [`../github-macos-15-shared/`](../github-macos-15-shared/README.md) provide. The group also
  self-expires on a 4 s watchdog with nothing signalling it, so a member decaying from the previous
  repetition can still be running when the next one starts.
- `group_term`'s `term_to_quiet` is computed by the same derivation `runtime.json` publishes as
  the external-TERM-to-final-report figure — first delivered group TERM to observed-quiet — but
  under different conditions (a sixteen-member group, a wall-time trigger, and 10 ms sampling here,
  versus a single ramp worker, a footprint trigger, and 50 ms sampling there), so the two figures
  are related by construction, not interchangeable values.

## Safety limits and rerun

Synthetic fixtures were capped at 128 MiB aggregate and 10 seconds. The Metal calibration used a
fixed 64 MiB buffer and a 5-second watchdog. No real MLX allocation was requested; the in-house
trial bundle under [`../in-house-trial/`](../in-house-trial/README.md) is where real MLX workloads
were supervised.

From a clean checkout of the recorded commit, run the command above. The script refuses a dirty
worktree, an existing output directory, an output directory inside the repository, or a host
other than an M1 Max with 32 GB memory; `scripts/calibrate-host.sh` is the same run without the
host check, for any Apple Silicon Mac. Metal access must be available to the process.
