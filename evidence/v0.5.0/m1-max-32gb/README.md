# M1 Max 32 GB reference evidence (0.5.0)

This bundle records the bounded calibration run for commit
`abd0106d27b45cab1e02546c65bbbe70429e106c`, the commit that carries the 0.5.0 version. The
worktree was clean when the run started. The host was a 10-core (8 performance, 2 efficiency) Apple
M1 Max MacBook Pro (MacBookPro18,2) with 32 GB memory, running macOS 27.2 (26B123) on arm64, on AC
power in High Power mode. The binaries are debug builds. Tool versions and the hardware record are
in [provenance.json](provenance.json) and repeated inside [footprint.json](footprint.json).

The earlier [0.3.1 bundle](../../v0.3.1/m1-max-32gb/README.md) was captured on macOS 27.0.1
(26A434). This one repeats the same measurements on macOS 27.2 with the 0.5.0 supervisor, so the
two can be read side by side; the table below gives both.

These measurements describe this host, this commit, and this build profile. They do not define a
universally safe memory limit. Footprint values are multi-call samples of OS-accounted physical
footprint, not instantaneous machine-wide memory totals.

Captured 2026-11-12 (the timestamps inside the files are UTC). The calibration ran from 02:03 to
02:37 local time, the soak from 02:37 to 04:39, and the optimized-build run described below from
04:39 to 05:09. The host did not sleep during any run. The command:

```bash
./scripts/calibrate-reference-host.sh <output-dir>
```

The script runs six chunks (footprint, runtime, intervention, scenarios, endurance, and the
escalation envelope), each written the moment it finishes, and resumes an interrupted run by
skipping the chunks that already passed. The table below lists twelve measures taken from them.
Its cargo transcripts stay in a sibling `.logs` directory that is not committed. The
[soak bundle](soak/README.md) next to this one records the supervisor's own resource use over long
escaping-churn runs on the same host.

## Results

| Measure | Target | Observed | 0.3.1 | Result |
|---|---:|---:|---:|---:|
| Anonymous 64 MiB maximum error | 1% or less | 32,816 B (0.049%) | 32,816 B (0.049%) | Pass |
| Live anonymous-shared 64 MiB maximum error | 1% or less | 16,432 B (0.024%) | 16,432 B (0.024%) | Pass |
| Metal 64 MiB maximum visible-delta error | 10% or less | 2,195,528 B (3.272%) | 2,703,456 B (4.028%) | Pass |
| `proc_pid_rusage` call p95 | 1 ms or less | 1.333 us | 1.250 us | Pass |
| 16-member sample-window p95 | 10 ms or less | 0.183 ms | 0.186 ms | Pass |
| 50 ms sampler CPU, 16 members | none | 2.85% to 2.89% (2 runs) | 1.9038% | Recorded |
| Supervisor maximum RSS | 20 MiB or less | 3.28 MiB | 3.31 MiB | Pass |
| 30-minute footprint growth | 2 MiB or less | 80 KiB | 80 KiB | Pass |
| Decision to first signal p95 | 10 ms or less | 8.166 us | 6.667 us | Pass |
| External TERM to final report p95 | 100 ms or less | 59.545 ms | 56.900 ms | Pass |
| External INT to final report p95 | 100 ms or less | 54.962 ms | 56.170 ms | Pass |
| 128 MiB/s ramp overshoot p95 | 16 MiB or less | 13,631,968 B (13.00 MiB) | 13,189,552 B (12.58 MiB) | Pass |

The sampler CPU row has no target. The 2% target in the 0.3.1 table no longer applies to it, and
the readings in it exceed 2%. The endurance test now fails above 3.5% of one core. That check
catches regressions in mlx-guard; it sets no target for the sampler's cost.

Two figures moved against 0.3.1. The Metal visible-delta error read 3.27% of the 64 MiB buffer
where 0.3.1 recorded 4.03%; the target is 10%. The 16-member sampler used 2.85% to 2.89% of one
core where 0.3.1 recorded 1.90%; the next paragraphs give every reading and the conditions they
were taken under. The other rows differ by amounts that are small against their targets.

The 50 ms sampler with 16 idle members read from 2.58% to 2.89% of one core in six 30-minute debug
runs on macOS 27.2 (26B123) on this host. Four were on 2026-11-09 (2.58%, 2.79%, 2.83% and 2.80%)
and two are the captures in this bundle on 2026-11-12 (2.89% in the calibration, 2.85% in the
soak). The 0.3.1 calibration and soak recorded 1.90% and 1.93% for the same test on macOS 27.0.1.
The percentage is the CPU time of every thread of the test process divided by the test's own
elapsed time, which excludes system sleep, so it covers more than the sampler thread.

The 2% bound failed on the four 2026-11-09 runs. One of them was a build of the v0.3.1 tag, and it
read 2.83% on 27.2, so the rise is not in the 0.5.0 code alone. The cause is not identified. The
soak bundle shows the same rise on its other chunks. The four earlier readings are kept with their
JSON files in the [earlier-runs-2026-11-09](earlier-runs-2026-11-09/README.md) folder next to this
README.

An optimized (`--release`) build of the same test read 1.74% on 2026-11-12.
[release-build/endurance.json](release-build/endurance.json) holds that run: the same 30-minute
endurance test, 16 idle members, 50 ms sampling. It is one run. The test's 3.5% regression check
applied to it and passed; like the other readings in this row, it has no target. It was run on
commit `abd0106`, the commit of this bundle, on macOS 27.2 (26B123) with rustc 1.93.0, on AC power
in High Power mode, from 04:39 to 05:09 local time, and the test exited 0. From a clean checkout of
that commit, the command is:

```bash
MLX_GUARD_ENDURANCE_SECONDS=1800 MLX_GUARD_ENDURANCE_OUTPUT=<out>/endurance.json \
  cargo test --release -p mlx-guard-test-support --test footprint_sampling \
  thirty_minute_sampler_stays_inside_cpu_rss_and_history_bounds -- --ignored --exact --nocapture
```

The 30-minute run retained exactly 256 samples; its raw minute observations are in
[endurance.json](endurance.json). The Metal allocation remained charged immediately after release
in all 10 runs, with a maximum residual of 69,304,392 bytes. In none of the 20 ramp runs did a
later sample show the footprint dropping after the signal, so these runs say nothing about how
fast memory is returned. The ramp runs report a decision-to-first-signal p95 of 0 ms because they
measure at millisecond resolution; the microsecond figure in the table comes from
`intervention.json`. Raw ramp and external-signal timings are in
[runtime.json](runtime.json); nanosecond decision-to-signal samples are in
[intervention.json](intervention.json).

## Lifecycle and safe workloads

[scenarios/scenarios.json](scenarios/scenarios.json) summarizes normal exit, observe-only,
acknowledged checkpoint, checkpoint timeout, TERM, KILL, fast root exit, owned-group cleanup,
session escape, and storage loss. The accompanying `reports/` directory contains the final reports
and durable journals; the journals are the hidden `.<name>.json.journal` files next to each
report. Four safe workloads completed with zero false interventions.

Every `run` scenario uses a 1 TiB limit on purpose, so each of those reports records
`limit_may_not_fit`. That is expected, and it says nothing about the scenario's outcome.

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
| `checkpoint_ack_idle` | 28 / 31 | 29 / 34 | n/a | 0 | 0 |
| `checkpoint_ack_loaded` | 27 / 31 | 29 / 30 | n/a | 0 | 0 |
| `group_term` | n/a | 35 / 36 | n/a | 0 | 0 |
| `group_kill` | n/a | n/a | 30 / 31 | 0 | 20 |

Against the 0.3.1 envelope every p95 is within 2 ms; the largest gaps are `checkpoint_ack_loaded`
term_to_quiet and `group_kill`, 29 ms and 30 ms here against 27 ms and 28 ms there.

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
  members park rather than spin, so they add process-table load; whether they loaded the CPU
  was not measured. The raw `load_at_start_per_repetition` samples in the JSON show the load average
  each repetition started under. Genuinely CPU-starved corroboration is what the shared
  three-vCPU-VM captures in
  [`../../v0.2.0/github-macos-15-shared/`](../../v0.2.0/github-macos-15-shared/README.md) provide.
  The group also self-expires on a 4 s watchdog with nothing signalling it, so a member decaying
  from the previous repetition can still be running when the next one starts.
- `group_term`'s `term_to_quiet` is computed by the same derivation `runtime.json` publishes as
  the external-TERM-to-final-report figure (first delivered group TERM to observed-quiet), but
  under different conditions: a sixteen-member group, a wall-time trigger, and 10 ms sampling here,
  versus a single ramp worker, a footprint trigger, and 50 ms sampling there. The two figures are
  related by construction, not interchangeable values.

## Safety limits and rerun

Synthetic fixtures were capped at 128 MiB aggregate and 10 seconds. The Metal calibration used a
fixed 64 MiB buffer and a 5-second watchdog. No real MLX allocation was requested; the in-house
trial bundle under [`../../v0.2.0/in-house-trial/`](../../v0.2.0/in-house-trial/README.md) is where
real MLX workloads were supervised.

After a squash merge the recorded commit stays reachable from `refs/pull/41/head`
(`git fetch origin refs/pull/41/head`). From a clean checkout of it, run the command above. The
script refuses a dirty
worktree, an existing output or transcript directory, an output directory inside the repository,
or a host other than an M1 Max with 32 GB memory; `scripts/calibrate-host.sh` is the same run
without the host check, for any Apple Silicon Mac. Metal access must be available to the
process.
