# M1 Max 32 GB soak evidence (0.3.1)

This bundle records the supervisor's own footprint, CPU share, retained escape evidence, and
sample-window bounds over long runs for commit `add2d722616b5f2f78e269fa602a2cd0430c2b51`, a commit
of pull request #39, the commit before the version bump, so the binaries report version
0.3.0; the supervisor code is identical to the 0.3.1 release. The worktree was clean when the run
started. The host was a 10-core (8
performance, 2 efficiency) Apple M1 Max MacBook Pro (MacBookPro18,2) with 32 GB memory, running
macOS 27.0.1 (26A434) on arm64; the binaries are debug builds. Raw records, with the hardware line
and git provenance, are in [provenance.json](provenance.json) and the four chunk files next to it.

The earlier [0.2.0 soak bundle](../../../v0.2.0/m1-max-32gb/soak/README.md) was captured on macOS
26.6.2 (25G83). The tables below give both, so the two can be read side by side.

These measurements describe this host, this commit, and this build profile. They characterize how
the supervisor's own resource use behaves over time under process churn; they do not define a
universal bound.

Captured 2026-10-25 (local time; the timestamps inside the files are UTC) with:

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
| `escaping-churn` | 6 spawners, 50 ms `setsid` escapees, in-process sampler at 10 ms | 169,252 | 2.53 MiB (see disclosures) | 5.56 MiB | 2.89 % | 0.58 ms | 20 MiB RSS, 5 % CPU, 10 ms p95, evidence list capped at 64 (held at 64) |
| `real-binary` | 6 spawners, 50 ms `setsid` escapees, `mlx-guard observe` at 10 ms | 175,104 | 320 KiB | n/a | 3.10 % | n/a | 1 MiB footprint growth, 8 % CPU |
| `endurance` | 16 idle members, 50 ms sampling | n/a | 64 KiB | 3.30 MiB | 1.93 % | n/a | 4 MiB growth, 20 MiB RSS |
| `pid-churn` | serial short-lived children in the owned group, 50 ms sampling | n/a | 816 KiB | 3.59 MiB | 0.51 % | 0.45 ms | 4 MiB growth, 20 MiB RSS, 10 ms p95 |

The 0.2.0 run of the same four chunks read:

| Chunk | Distinct escapes | Footprint growth | Max RSS | CPU of one core | p95 sample window |
|---|---:|---:|---:|---:|---:|
| `escaping-churn` | 174,538 | 2.56 MiB | 5.28 MiB | 2.82 % | 0.49 ms |
| `real-binary` | 178,677 | 256 KiB | n/a | 2.96 % | n/a |
| `endurance` | n/a | 64 KiB | 3.06 MiB | 1.85 % | n/a |
| `pid-churn` | n/a | 800 KiB | 3.31 MiB | 0.56 % | 0.42 ms |

Every figure stays inside its ceiling. The largest relative moves are the real-binary growth
(320 KiB against 256 KiB, with a 1 MiB ceiling) and the escaping-churn p95 sample window (0.58 ms
against 0.49 ms, with a 10 ms ceiling).

The real-binary chunk's observe calibration counted 130,765 samples over the run with none
incomplete, and the final 4,096-sample window carried no unusable sample. A tracked child that
exits counts as a containment event, not a missing sample, so observing six spawners no longer
loses samples. Earlier recordings had to stop at two spawners for that reason.

With the 64-entry evidence cap removed, the same recipe at two spawners grew 2.08 MiB over 1800 s
(60,026 escapes) and the 1 MiB ceiling failed the test; that run is committed in the 0.2.0 bundle
as [real-binary-uncapped.json](../../../v0.2.0/m1-max-32gb/soak/real-binary-uncapped.json) (an
earlier schema without the sample telemetry, captured with the cap guard in `identity.rs` removed
and reverted afterwards). It is the class of retention this chunk exists to catch, and it was not
repeated for 0.3.1.

## Honesty disclosures

- The in-process chunk's footprint growth includes the test harness's own bookkeeping in the same
  process it measures: a progress record every 30 s and the per-sample window log, pre-sized so it
  does not reallocate inside the window. That column therefore carries a loose 20 MiB resident
  ceiling rather than a tight one; the tight growth ceiling is asserted on the real-binary chunk,
  whose footprint is read from outside the measured process.
- Distinct escapes is a floor, not a census. A child that leaves the owned group and its parent
  link faster than one sample interval is never observed, so the count can only undercount.
- The footprint-growth ceilings guard the retention class the 2026-08 supervisor growth belonged
  to: memory that scales with distinct pids ever observed. The 64-entry evidence cap is asserted
  directly in the in-process chunk; the real-binary chunk sees it only through footprint growth,
  which the uncapped 0.2.0 run shows is enough.
- The measured binaries are unoptimized debug builds run through `cargo test`, the same path the
  rest of the reference bundle uses.
- The sample-window bound is taken from the in-process sampler; the real-binary chunk reads the
  binary's footprint and CPU time from outside and does not time its samples.
- The core count in this bundle's [provenance.json](provenance.json) is correct. The calibration
  bundle's own provenance file misreads it; see the note in its [README](../README.md).
