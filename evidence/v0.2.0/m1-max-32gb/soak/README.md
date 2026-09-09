# M1 Max 32 GB soak evidence

This bundle records the supervisor's own footprint, CPU share, retained escape evidence, and
sample-window bounds over long runs for commit `PENDING (reference run not yet captured)`. The
worktree was clean when the run started. The host was a 10-core (8 performance, 2 efficiency)
Apple M1 Max MacBook Pro with 32 GB memory, running macOS PENDING on arm64. Raw records, with the
sanitized hardware line and git provenance, are in [provenance.json](provenance.json) and the four
chunk files next to it.

These measurements describe this host, this commit, and this build profile. They characterize how
the supervisor's own resource use behaves over time under process churn; they do not define a
universal bound.

Captured PENDING with:

```bash
./scripts/soak-reference-host.sh <out>
```

## Results

Each chunk ran for 1800 s after its own warm-up. Footprint growth is the supervisor process's own
`phys_footprint` at the end of the run minus its value after warm-up; for the real-binary chunk it
is the peak observed during the run minus the post-warm-up baseline.

| Chunk | Workload | Distinct escapes | Footprint growth | Max RSS | CPU of one core | p95 sample window | Ceiling asserted |
|---|---|---:|---:|---:|---:|---:|---|
| `escaping-churn` | 6 spawners, 50 ms `setsid` escapees, in-process sampler at 10 ms | PENDING | PENDING | PENDING | PENDING | PENDING | 20 MiB RSS, 5 % CPU, 10 ms p95, evidence list capped at 64 |
| `real-binary` | 2 spawners, 50 ms `setsid` escapees, `mlx-guard observe` at 10 ms | PENDING | PENDING | n/a | n/a | n/a | PENDING footprint-growth ceiling |
| `endurance` | 16 idle members, 50 ms sampling | n/a | PENDING | PENDING | PENDING | n/a | 4 MiB growth, 20 MiB RSS |
| `pid-churn` | serial short-lived children in the owned group, 50 ms sampling | n/a | PENDING | PENDING | PENDING | PENDING | 4 MiB growth, 20 MiB RSS, 10 ms p95 |

## Honesty disclosures

- Distinct escapes is a floor, not a census. A child that leaves the owned group and its parent
  link faster than one sample interval is never observed, so the count can only undercount.
- The real-binary chunk runs two spawners because the current binary treats a tracked child's exit
  as one unusable sample and fails observation closed after three in a row; two serial spawners at
  10 ms sampling can never produce three consecutive exits, so the run measures sustained
  supervision at roughly 30 escapes per second rather than the harness's maximum rate.
- The footprint-growth ceilings guard the retention class the 2026-08 supervisor growth belonged
  to: memory that scales with distinct pids ever observed. The 64-entry evidence cap is asserted
  directly in the in-process chunk; the real-binary chunk sees it only through footprint growth.
- The measured binaries are unoptimized debug builds run through `cargo test`, the same path the
  rest of the reference bundle uses.
- The sample-window bound is taken from the in-process sampler; the real-binary chunk reads the
  binary's footprint from outside and does not time its samples.
