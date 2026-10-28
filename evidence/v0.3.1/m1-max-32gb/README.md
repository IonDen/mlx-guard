# M1 Max 32 GB reference evidence (0.3.1)

This bundle records the bounded calibration run for commit
`add2d722616b5f2f78e269fa602a2cd0430c2b51`, a commit of pull request #39. After the merge it stays
reachable from refs/pull/39/head (`git fetch origin refs/pull/39/head && git checkout add2d72`).
That is the commit before the version bump, so the binaries report version 0.3.0 (the journals
record `package_version` 0.3.0); the supervisor code is identical to the 0.3.1 release. The
worktree was clean when the run started, and the six chunks ran in one uninterrupted pass. The host
was a 10-core (8 performance, 2 efficiency) Apple M1 Max MacBook Pro (MacBookPro18,2) with 32 GB
memory, running macOS 27.0.1 (26A434) on arm64. The binaries are debug builds. Tool versions and the
hardware record are in [provenance.json](provenance.json) and repeated inside
[footprint.json](footprint.json).

The earlier [0.2.0 bundle](../../v0.2.0/m1-max-32gb/README.md) was captured on macOS 26.6.2
(25G83). This one repeats the same measurements on macOS 27.0.1 with the 0.3.1 supervisor, so the
two can be read side by side; the table below gives both.

These measurements describe this host, this commit, and this build profile. They do not define a
universally safe memory limit. Footprint values are multi-call samples of OS-accounted physical
footprint, not instantaneous machine-wide memory totals.

Captured 2026-10-25 (local time; the timestamps inside the files are UTC) with:

```bash
./scripts/calibrate-reference-host.sh <output-dir>
```

The script runs six chunks (footprint, runtime, intervention, scenarios, endurance, and the
escalation envelope), each written the moment it finishes, and resumes an interrupted run by
skipping the chunks that already passed. The table below lists twelve measures taken from them.
Its cargo transcripts stay
in a sibling `.logs` directory that is not committed. The [soak bundle](soak/README.md) next to
this one records the supervisor's own resource use over long escaping-churn runs on the same host.

## A note on the core count in provenance.json

The `hardware` string in this bundle's [provenance.json](provenance.json) reads "10 (0 Performance
and 8 Efficiency)". That is wrong: the capture script misread the core-count field, which macOS 27
reports in a new four-part form. The host has 8 performance and 2 efficiency cores. The hardware
record inside [footprint.json](footprint.json) and [escalation-envelope.json](escalation-envelope.json)
shows the correct line, and so does the soak bundle's provenance, which comes from the text
report. None of the measurements depend on that string. The file is left as captured, and the
script is fixed in 0.3.1.

## Results

| Measure | Target | Observed | 0.2.0 | Result |
|---|---:|---:|---:|---:|
| Anonymous 64 MiB maximum error | 1% or less | 32,816 B (0.049%) | 49,224 B (0.073%) | Pass |
| Live anonymous-shared 64 MiB maximum error | 1% or less | 16,432 B (0.024%) | 16,432 B (0.024%) | Pass |
| Metal 64 MiB maximum visible-delta error | 10% or less | 2,703,456 B (4.028%) | 2,031,688 B (3.027%) | Pass |
| `proc_pid_rusage` call p95 | 1 ms or less | 1.250 us | 1.292 us | Pass |
| 16-member sample-window p95 | 10 ms or less | 0.186 ms | 0.180 ms | Pass |
| 50 ms sampler CPU, 16 members | 2% of one core or less | 1.9038% | 1.5927% | Pass |
| Supervisor maximum RSS | 20 MiB or less | 3.31 MiB | 3.06 MiB | Pass |
| 30-minute footprint growth | 2 MiB or less | 80 KiB | 96 KiB | Pass |
| Decision to first signal p95 | 10 ms or less | 6.667 us | 9.333 us | Pass |
| External TERM to final report p95 | 100 ms or less | 56.900 ms | 51.193 ms | Pass |
| External INT to final report p95 | 100 ms or less | 56.170 ms | 66.170 ms | Pass |
| 128 MiB/s ramp overshoot p95 | 16 MiB or less | 13,189,552 B (12.58 MiB) | 13,484,416 B (12.86 MiB) | Pass |

Two figures moved by more than noise against 0.2.0, and both still pass. The Metal visible-delta
error rose from 3.03% to 4.03% of the 64 MiB buffer; the target is 10%. The 16-member sampler
used 1.90% of one core where 0.2.0 recorded 1.59%; the target is 2%, so this is the bound with
the least room left on this host, and a rerun on another machine may land on either side of it.
The other rows differ by amounts that are small against their targets.

The 30-minute run retained exactly 256 samples; its raw minute observations are in
[endurance.json](endurance.json). The Metal allocation remained charged immediately after release
in all 10 runs, with a maximum residual of 69,812,320 bytes. None of the 20 ramp runs produced a
lower numeric footprint sample after signaling; those observations are right-censored and do not
support a prompt-reclamation claim. The ramp runs report a decision-to-first-signal p95 of 0 ms at
millisecond resolution. Raw ramp and external-signal timings are in
[runtime.json](runtime.json); nanosecond decision-to-signal samples are in
[intervention.json](intervention.json).

## Lifecycle and safe workloads

[scenarios/scenarios.json](scenarios/scenarios.json) summarizes normal exit, observe-only,
acknowledged checkpoint, checkpoint timeout, TERM, KILL, fast root exit, owned-group cleanup,
session escape, and storage loss. The accompanying `reports/` directory contains the final reports
and durable journals; the journals are the hidden `.<name>.json.journal` files next to each
report. Four safe workloads completed with zero false interventions.

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
| `checkpoint_ack_idle` | 27 / 27 | 29 / 32 | n/a | 0 | 0 |
| `checkpoint_ack_loaded` | 27 / 27 | 27 / 29 | n/a | 0 | 0 |
| `group_term` | n/a | 34 / 35 | n/a | 0 | 0 |
| `group_kill` | n/a | n/a | 28 / 31 | 0 | 20 |

Against the 0.2.0 envelope every p95 is within 8 ms; the largest gap is `group_kill`, 28 ms here
and 36 ms there.

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
  started under (1.41 to 2.63 across the four scenarios). Genuinely CPU-starved corroboration is
  what the shared three-vCPU-VM captures in
  [`../../v0.2.0/github-macos-15-shared/`](../../v0.2.0/github-macos-15-shared/README.md) provide. The group also
  self-expires on a 4 s watchdog with nothing signalling it, so a member decaying from the previous
  repetition can still be running when the next one starts.
- `group_term`'s `term_to_quiet` is computed by the same derivation `runtime.json` publishes as
  the external-TERM-to-final-report figure (first delivered group TERM to observed-quiet), but
  under different conditions: a sixteen-member group, a wall-time trigger, and 10 ms sampling here,
  versus a single ramp worker, a footprint trigger, and 50 ms sampling there. The two figures are
  related by construction, not interchangeable values.

## Safety limits and rerun

Synthetic fixtures were capped at 128 MiB aggregate and 10 seconds. The Metal calibration used a
fixed 64 MiB buffer and a 5-second watchdog. No real MLX allocation was requested; the in-house
trial bundle under [`../../v0.2.0/in-house-trial/`](../../v0.2.0/in-house-trial/README.md) is where real MLX workloads
were supervised.

From a clean checkout of the recorded commit, run the command above. The script refuses a dirty
worktree, an existing output or transcript directory, an output directory inside the repository,
or a host other than an M1 Max with 32 GB memory; `scripts/calibrate-host.sh` is the same run
without the host check, for any Apple Silicon Mac. Metal access must be available to the
process.
