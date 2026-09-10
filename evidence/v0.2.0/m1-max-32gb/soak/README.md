# M1 Max 32 GB soak evidence

This bundle records the supervisor's own footprint, CPU share, retained escape evidence, and
sample-window bounds over long runs for commit
`a5a5cb93abc306f572e789f245eeb535a7e9bc50`. The
worktree was clean when the run started. The host was a 10-core (8 performance, 2 efficiency)
Apple M1 Max MacBook Pro with 32 GB memory, running
macOS 26.6.2 (25G83) on arm64. Raw records, with the
sanitized hardware line and git provenance, are in [provenance.json](provenance.json) and the four
chunk files next to it.

These measurements describe this host, this commit, and this build profile. They characterize how
the supervisor's own resource use behaves over time under process churn; they do not define a
universal bound.

Captured 2026-09-09 with:

```bash
./scripts/soak-reference-host.sh <out>
```

## Results

Each chunk ran for 1800 s after its own warm-up. Footprint growth is the supervisor process's own
`phys_footprint` at the end of the run minus its value after warm-up; for the real-binary chunk it
is the peak observed during the run minus the post-warm-up baseline.

| Chunk | Workload | Distinct escapes | Footprint growth | Max RSS | CPU of one core | p95 sample window | Ceiling asserted |
|---|---|---:|---:|---:|---:|---:|---|
| `escaping-churn` | 6 spawners, 50 ms `setsid` escapees, in-process sampler at 10 ms | 177,469 | 4.81 MiB (see disclosures) | 7.06 MiB | 2.78 % | 0.50 ms | 20 MiB RSS, 5 % CPU, 10 ms p95, evidence list capped at 64 (held at 64) |
| `real-binary` | 2 spawners, 50 ms `setsid` escapees, `mlx-guard observe` at 10 ms | 57,222 | 112 KiB | n/a | n/a | n/a | 1 MiB footprint growth |
| `endurance` | 16 idle members, 50 ms sampling | n/a | 64 KiB | 2.94 MiB | 1.83 % | n/a | 4 MiB growth, 20 MiB RSS |
| `pid-churn` | serial short-lived children in the owned group, 50 ms sampling | n/a | 816 KiB | 3.33 MiB | 0.65 % | 0.58 ms | 4 MiB growth, 20 MiB RSS, 10 ms p95 |

The real-binary chunk's growth plateaued: its delta reached 96 KiB by 480 s and rose once more, to
112 KiB, before 1800 s, so the binary's footprint does not track the 57,222 distinct escapes it
counted. With the 64-entry evidence cap removed, the same recipe grew 2.08 MiB over 1800 s (60,026
escapes) and the 1 MiB ceiling failed the test, which is the class of retention this chunk exists to
catch.

## Honesty disclosures

- The in-process chunk's footprint growth includes the test harness's own per-sample bookkeeping:
  it retains one window duration per sample (135,358 samples here) and a progress record every
  30 s in the same process it measures, which is why that column carries a loose 20 MiB resident
  ceiling rather than a tight one. The tight growth ceiling is asserted on the real-binary chunk,
  whose footprint is read from outside the measured process.
- Distinct escapes is a floor, not a census. A child that leaves the owned group and its parent
  link faster than one sample interval is never observed, so the count can only undercount.
- The real-binary chunk ran two spawners because the binary at commit `a5a5cb9`, before the
  child-exit fix, treated a tracked child's exit as one unusable sample and failed observation
  closed after three in a row; two serial spawners at
  10 ms sampling can never produce three consecutive exits, so the run measures sustained
  supervision at roughly 30 escapes per second rather than the harness's maximum rate.
- The footprint-growth ceilings guard the retention class the 2026-08 supervisor growth belonged
  to: memory that scales with distinct pids ever observed. The 64-entry evidence cap is asserted
  directly in the in-process chunk; the real-binary chunk sees it only through footprint growth.
- The measured binaries are unoptimized debug builds run through `cargo test`, the same path the
  rest of the reference bundle uses.
- The sample-window bound is taken from the in-process sampler; the real-binary chunk reads the
  binary's footprint from outside and does not time its samples.
