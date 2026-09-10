# M1 Max 32 GB soak evidence

This bundle records the supervisor's own footprint, CPU share, retained escape evidence, and
sample-window bounds over long runs for commit
`66212586c21d13797b5b959e2c18ba2110805e4a`. The worktree was clean when the run started. The host
was a 10-core (8 performance, 2 efficiency) Apple M1 Max MacBook Pro with 32 GB memory, running
macOS 26.6.2 (25G83) on arm64. Raw records, with the sanitized hardware line and git provenance,
are in [provenance.json](provenance.json) and the four chunk files next to it. Commits after
`6621258` on the same branch changed tests and documentation only; the supervisor binary they
build is identical to the one measured here.

These measurements describe this host, this commit, and this build profile. They characterize how
the supervisor's own resource use behaves over time under process churn; they do not define a
universal bound.

Captured 2026-09-10 with:

```bash
./scripts/soak-reference-host.sh <out>
```

## Results

Each chunk ran for 1800 s after its own warm-up. Footprint growth is the supervisor process's own
`phys_footprint` at the end of the run minus its value after warm-up; for the real-binary chunk it
is the peak observed during the run minus the baseline taken after a 60 s warm-up, which is past
the point where the 4,096-sample report ring has filled.

| Chunk | Workload | Distinct escapes | Footprint growth | Max RSS | CPU of one core | p95 sample window | Ceiling asserted |
|---|---|---:|---:|---:|---:|---:|---|
| `escaping-churn` | 6 spawners, 50 ms `setsid` escapees, in-process sampler at 10 ms | 174,538 | 2.56 MiB (see disclosures) | 5.28 MiB | 2.82 % | 0.49 ms | 20 MiB RSS, 5 % CPU, 10 ms p95, evidence list capped at 64 (held at 64) |
| `real-binary` | 6 spawners, 50 ms `setsid` escapees, `mlx-guard observe` at 10 ms | 178,677 | 256 KiB | n/a | 2.96 % | n/a | 1 MiB footprint growth, 8 % CPU |
| `endurance` | 16 idle members, 50 ms sampling | n/a | 64 KiB | 3.06 MiB | 1.85 % | n/a | 4 MiB growth, 20 MiB RSS |
| `pid-churn` | serial short-lived children in the owned group, 50 ms sampling | n/a | 800 KiB | 3.31 MiB | 0.56 % | 0.42 ms | 4 MiB growth, 20 MiB RSS, 10 ms p95 |

The real-binary chunk's growth stepped in 16 KiB pages to 256 KiB by 600 s and stayed there for
the remaining 1200 s, so the binary's footprint does not track the 178,677 distinct escapes it
counted. The observe calibration counted 130,220 samples over the run with none incomplete, and
the final 4,096-sample window carried no unusable sample: a tracked child's exit is a containment
event now, not a missing sample, so six spawners can be observed where two was the ceiling before.

With the 64-entry evidence cap removed, the same recipe at two spawners grew 2.08 MiB over 1800 s
(60,026 escapes) and the 1 MiB ceiling failed the test; that run is committed as
[real-binary-uncapped.json](real-binary-uncapped.json) (an earlier schema without the sample
telemetry, captured with the cap guard in `identity.rs` removed and reverted afterwards). It is
the class of retention this chunk exists to catch.

## Honesty disclosures

- The in-process chunk's footprint growth includes the test harness's own bookkeeping in the same
  process it measures: a progress record every 30 s and the per-sample window log, now pre-sized
  so it no longer reallocates inside the window (an earlier capture without the pre-sizing read
  4.81 MiB). That column therefore carries a loose 20 MiB resident ceiling rather than a tight
  one; the tight growth ceiling is asserted on the real-binary chunk, whose footprint is read from
  outside the measured process.
- Distinct escapes is a floor, not a census. A child that leaves the owned group and its parent
  link faster than one sample interval is never observed, so the count can only undercount.
- The footprint-growth ceilings guard the retention class the 2026-08 supervisor growth belonged
  to: memory that scales with distinct pids ever observed. The 64-entry evidence cap is asserted
  directly in the in-process chunk; the real-binary chunk sees it only through footprint growth,
  which the uncapped run above shows is enough.
- The measured binaries are unoptimized debug builds run through `cargo test`, the same path the
  rest of the reference bundle uses.
- The sample-window bound is taken from the in-process sampler; the real-binary chunk reads the
  binary's footprint and CPU time from outside and does not time its samples.
