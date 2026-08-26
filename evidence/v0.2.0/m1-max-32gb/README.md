# M1 Max 32 GB escalation-envelope evidence

This bundle records the checkpoint- and signal-escalation timing envelope for commit
`72154a50fb91367280dd25ef27b65c933554b285`. The worktree was clean when the run started. The host
was a 10-core (8 performance, 2 efficiency) Apple M1 Max MacBook Pro with 32 GB memory, running
macOS 26.6.1 (25G76) on arm64. The raw record, including the sanitized hardware line, git
provenance, and per-repetition system load, is in
[escalation-envelope.json](escalation-envelope.json).

These measurements describe this host, this commit, and this build profile. They characterize the
supervision path's timing shape; they do not define a universal latency guarantee.

Captured 2026-08-26 with:

```bash
./scripts/measure-escalation-envelope.sh m1-max-32gb <out>
```

## Results

All values are milliseconds, 20 repetitions per scenario, `resolution_ms: 10`.

| Scenario | request_to_ack p95 / max | term_to_quiet p95 / max | kill_to_quiet p95 / max | timed_out | escalated_to_kill |
|---|---:|---:|---:|---:|---:|
| `checkpoint_ack_idle` | 29 / 29 | 27 / 28 | — | 0 | 0 |
| `checkpoint_ack_loaded` | 29 / 30 | 28 / 36 | — | 0 | 0 |
| `group_term` | — | 37 / 38 | — | 0 | 0 |
| `group_kill` | — | — | 31 / 31 | 0 | 20 |

`checkpoint_ack_idle` and `checkpoint_ack_loaded` request a cooperative checkpoint from an
acknowledging worker and then TERM it once the acknowledgement lands. `group_term` sends TERM to a
sixteen-member fanout-stall group with the default disposition and observes the first escalation
step end it. `group_kill` sends TERM to a TERM-deaf sixteen-member group and observes every
repetition escalate to KILL; it publishes no `term_to_quiet` interval, since a killpg delivered to
a group that never responds to TERM would only measure noise.

## Honesty disclosures

- All marks are supervisor-loop timestamps quantized by the 10 ms sample interval. The
  `request_to_ack`, TERM, and KILL stamps are policy event times, not delivery times; "quiet" is
  the loop observing the root reaped **and** the owned group empty, never the reap instant itself.
- The measured binaries are unoptimized debug builds — the capture script builds and runs through
  `cargo test`, the same path `scripts/calibrate-reference-host.sh` uses for the rest of the
  reference bundle. These numbers characterize the supervision path's timing shape, not an
  optimized release build.
- The `checkpoint_ack_loaded` arm's background load is a sixteen-member group that self-expires on
  a 4 s watchdog with nothing signalling it. A member decaying from the previous repetition can
  still be running when the next repetition starts, adding load beyond what that repetition itself
  spawned; conversely, a worst-case slow-acknowledgement repetition can run longer than 4 s and end
  up measured partially unloaded. Both directions are visible in the raw
  `load_at_start_per_repetition` samples in the JSON.
- `group_term`'s `term_to_quiet` is computed by the same derivation the committed reference
  calibration publishes as `finalization_latency_milliseconds`
  (`crates/mlx-guard-cli/tests/reference_runtime_calibration.rs`) — first delivered group TERM to
  observed-quiet — but measured under different conditions (a sixteen-member group, a wall-time
  trigger, and 10 ms sampling here, versus a single ramp worker, a footprint trigger, and 50 ms
  sampling there), so the two figures are related by construction, not interchangeable values.

## Corroboration

A dispatch-only workflow (`.github/workflows/envelope.yml`) captures the same artifact on GitHub's
shared macos-15 VM (3 vCPU / 7 GB) under uncontrolled co-tenancy. Those runs are labeled
`github-macos-15-shared-runN` and never gate a release; a human folds at least five dispatches by
pooling every raw interval across runs and computing p95 over the pooled set, since a single run's
internal p95 at `n = 20` sits near the maximum.

## Rerun

From a clean checkout of the recorded commit, run:

```bash
./scripts/measure-escalation-envelope.sh m1-max-32gb /private/tmp/mlx-guard-v0.2.0-envelope
```

The script refuses an existing output directory and an empty profile label. It has no host-model
check of its own; `scripts/calibrate-reference-host.sh` folds this capture into the rest of the
reference bundle, which still requires an M1 Max with 32 GB memory and a clean worktree.
