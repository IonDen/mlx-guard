# M1 Max 32 GB reference evidence

This bundle records a bounded calibration run for commit
`959bbaef66fb9f3921b7f511eb909ec91a948125`. The worktree was clean when the run started. The host
was a 10-core Apple M1 Max with 32 GB memory, running macOS 26.6.1 (25G76) on arm64. Tool versions
and the sanitized hardware record are in [footprint.json](footprint.json).

These measurements describe this host and commit. They do not define a universally safe memory
limit. Footprint values are multi-call samples of OS-accounted physical footprint, not instantaneous
machine-wide memory totals.

## Results

| Measure | Target | Observed | Result |
|---|---:|---:|---:|
| Anonymous 64 MiB maximum error | 1% or less | 49,224 B (0.073%) | Pass |
| Live anonymous-shared 64 MiB maximum error | 1% or less | 16,432 B (0.024%) | Pass |
| Metal 64 MiB maximum visible-delta error | 10% or less | 1,294,408 B (1.929%) | Pass |
| `proc_pid_rusage` call p95 | 1 ms or less | 1.250 us | Pass |
| 16-member sample-window p95 | 10 ms or less | 0.183 ms | Pass |
| 50 ms sampler CPU, 16 members | 2% of one core or less | 1.6868% | Pass |
| Supervisor maximum RSS | 20 MiB or less | 2.98 MiB | Pass |
| 30-minute footprint growth | 2 MiB or less | 80 KiB | Pass |
| Decision to first signal p95 | 10 ms or less | 8.209 us | Pass |
| External TERM to final report p95 | 100 ms or less | 60.811 ms | Pass |
| External INT to final report p95 | 100 ms or less | 56.186 ms | Pass |
| 128 MiB/s ramp overshoot p95 | 16 MiB or less | 13,386,112 B (12.77 MiB) | Pass |

The 30-minute run retained exactly 256 samples. Its raw minute observations are in
[endurance.json](endurance.json). The Metal allocation remained charged immediately after release in
all ten runs, with a maximum residual of 68,403,272 bytes. None of the 20 ramp runs produced a lower
numeric footprint sample after signaling. Those observations are right-censored and do not support
a prompt-reclamation claim. Group finalization occurred 54 to 61 ms after the first signal. Raw ramp
and external-signal timings are in [runtime.json](runtime.json); nanosecond decision-to-signal samples
are in [intervention.json](intervention.json).

## Lifecycle and safe workloads

[scenarios/scenarios.json](scenarios/scenarios.json) summarizes normal exit, observe-only,
acknowledged checkpoint, checkpoint timeout, TERM, KILL, storage loss, fast root exit, owned-group
cleanup, and session escape. The accompanying `reports/` directory contains the final reports and
durable journals. Four safe workloads completed with zero false interventions.

The `root-fast-exit` scenario records 0.1.0 behavior (measurement-loss fail-closed, exit 75); from
0.2.0 the same scenario ends with the root's status after survivor cleanup — see the 0.2.0 bundle.

## Safety limits and rerun

Synthetic fixtures were capped at 128 MiB aggregate and 10 seconds. The Metal calibration used a
fixed 64 MiB buffer and a 5-second watchdog. No real MLX allocation was requested. The separate
real-MLX integration used the bounded configuration recorded in its own evidence bundle; this
calibration script does not define a real-MLX allocation ceiling.

From a clean checkout of the recorded commit, run:

```bash
./scripts/calibrate-reference-host.sh /private/tmp/mlx-guard-v0.1.0-evidence
```

The script refuses a dirty worktree, an existing output directory, or a host other than an M1 Max
with 32 GB memory. Metal access must be available to the process.
