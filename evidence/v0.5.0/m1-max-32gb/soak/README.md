# M1 Max 32 GB soak evidence (0.5.0)

This bundle records the supervisor's own footprint, CPU share, retained escape evidence, and
sample-window bounds over long runs for commit `abd0106d27b45cab1e02546c65bbbe70429e106c`, the
commit that carries the 0.5.0 version. The worktree was clean when the run started. The host was a
10-core (8 performance, 2 efficiency) Apple M1 Max MacBook Pro (MacBookPro18,2) with 32 GB memory,
running macOS 27.2 (26B123) on arm64, on AC power in High Power mode; the binaries are debug
builds. Raw records, with the hardware line and git provenance, are in
[provenance.json](provenance.json) and the four chunk files next to it.

The earlier [0.3.1 soak bundle](../../../v0.3.1/m1-max-32gb/soak/README.md) was captured on macOS
27.0.1 (26A434). The tables below give both, so the two can be read side by side.

These measurements describe this host, this commit, and this build profile. They characterize how
the supervisor's own resource use behaves over time under process churn; they do not define a
universal bound.

Captured 2026-11-12, between 02:37 and 04:39 local time (the timestamps inside the files are UTC).
The host did not sleep during the run. The command:

```bash
./scripts/soak-reference-host.sh <out>
```

After a squash merge the recorded commit stays reachable from `refs/pull/41/head`
(`git fetch origin refs/pull/41/head`).

## Results

Each chunk ran for 1800 s after its own warm-up. Footprint growth is the supervisor process's own
`phys_footprint` at the end of the run minus its value after warm-up; for the real-binary chunk it
is the peak observed during the run minus the baseline taken after a 60 s warm-up, which is past
the point where the 4,096-sample report ring has filled.

| Chunk | Workload | Distinct escapes | Footprint growth | Max RSS | CPU of one core | p95 sample window | Ceiling asserted |
|---|---|---:|---:|---:|---:|---:|---|
| `escaping-churn` | 6 spawners, 50 ms `setsid` escapees, in-process sampler at 10 ms | 163,038 | 2.47 MiB (see disclosures) | 5.56 MiB | 4.09 % | 1.28 ms | 20 MiB RSS, 5 % CPU, 10 ms p95, evidence list capped at 64 (held at 64) |
| `real-binary` | 6 spawners, 50 ms `setsid` escapees, `mlx-guard observe` at 10 ms | 168,064 | 352 KiB | n/a | 4.50 % | n/a | 1 MiB footprint growth, 8 % CPU |
| `endurance` | 16 idle members, 50 ms sampling | n/a | 64 KiB | 3.34 MiB | 2.85 % | n/a | 2 MiB growth, 20 MiB RSS, 3.5 % CPU |
| `pid-churn` | serial short-lived children in the owned group, 50 ms sampling | n/a | 816 KiB | 3.64 MiB | 0.57 % | 0.47 ms | 4 MiB growth, 20 MiB RSS, 10 ms p95 |

The 0.3.1 and 0.2.0 soak tables listed 4 MiB growth for the `endurance` row, but the endurance test
asserted 2 MiB in those releases too. The 3.5 % CPU ceiling in that row is a
regression check, not a target; the [calibration README](../README.md) says more about it.

The 0.3.1 run of the same four chunks read:

| Chunk | Distinct escapes | Footprint growth | Max RSS | CPU of one core | p95 sample window |
|---|---:|---:|---:|---:|---:|
| `escaping-churn` | 169,252 | 2.53 MiB | 5.56 MiB | 2.89 % | 0.58 ms |
| `real-binary` | 175,104 | 320 KiB | n/a | 3.10 % | n/a |
| `endurance` | n/a | 64 KiB | 3.30 MiB | 1.93 % | n/a |
| `pid-churn` | n/a | 816 KiB | 3.59 MiB | 0.51 % | 0.45 ms |

Every figure stays inside its ceiling. The CPU share rose on all four chunks. The escaping-churn
chunk went from 2.89 % to 4.09 % under a 5 % ceiling; with the endurance row (2.85 % under
3.5 %) it is one of the two rows with the least room. The real-binary chunk went from 3.10 % to
4.50 % under 8 %, and the endurance chunk from 1.93 % to 2.85 % under 3.5 %. The pid-churn chunk
moved from 0.51 % to 0.57 % and has no ceiling. The escaping-churn p95 sample window also grew,
from 0.58 ms to 1.28 ms against a 10 ms ceiling. The cause of the CPU rise is not identified; the
[calibration README](../README.md) gives the sampler readings from the same host.

The real-binary chunk's observe calibration counted 130,066 samples over the run with none
incomplete, and the final 4,096-sample window carried no unusable sample. A tracked child that
exits counts as a containment event, not a missing sample, so observing six spawners no longer
loses samples. Earlier recordings had to stop at two spawners for that reason.

With the 64-entry evidence cap removed, the same recipe at two spawners grew 2.08 MiB over 1800 s
(60,026 escapes) and the 1 MiB ceiling failed the test; that run is committed in the 0.2.0 bundle
as [real-binary-uncapped.json](../../../v0.2.0/m1-max-32gb/soak/real-binary-uncapped.json) (an
earlier schema without the sample telemetry, captured with the cap guard in `identity.rs` removed
and reverted afterwards). It is the class of retention this chunk exists to catch, and it was not
repeated for 0.5.0.

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
