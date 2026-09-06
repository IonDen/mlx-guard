# In-house recipe trial — Apple M1 Max 32 GB

This bundle records mlx-guard supervising two real, published command recipes end to end on one
machine: an `mlx-lm` LoRA fine-tune and an `mflux` image generation. Each recipe ran under the
guard's observe mode, to measure its footprint, and under enforce mode with a footprint limit. One
limit was chosen to leave the run untouched; one was chosen to make the guard intervene. A third
pair drives a cooperative checkpoint and resume through the Python adapter. The aim is to show the
supervision paths working against workloads a user would actually run, not against a synthetic
fixture.

The host was a 10-core Apple M1 Max MacBook Pro with 32 GB of unified memory, on macOS 26.6.2
(25G83), arm64. The guard was a release build of `mlx-guard` (reported version 0.1.0, sha256
`3ddb3497…add8f`) whose Rust sources matched commit `b6a0504`. The worktree was clean and the
machine was on AC when the runs started. The raw evidence is in this directory: one schema-v1 report
and its checksummed journal per arm, the per-arm index [arms.json](arms.json), and
[provenance.json](provenance.json). Reports carry no argument vectors, paths, or environment; a scan
of the whole bundle for host paths and user names came back empty.

These measurements describe this host, this commit, and this build profile. They show the
supervision paths behaving as their contracts describe on real workloads. They do not define a
universal timing or memory guarantee.

## The recipes

LoRA fine-tune, run through `uvx` so it needs nothing installed:

```bash
mlx-guard run --max-footprint <bytes> -- \
  uvx --from 'mlx-lm[train]' mlx_lm.lora \
    --model mlx-community/Qwen2.5-0.5B-Instruct-4bit \
    --train --data mlx-community/wikisql --iters 200 --adapter-path adapters
```

Image generation:

```bash
mlx-guard run --max-footprint <bytes> -- \
  mflux-generate --model dev --quantize 8 \
    --prompt "a lighthouse at dusk" --steps 4 --seed 42 --output out.png
```

The cooperative arms run a small resumable LoRA worker, a real fine-tune that saves its adapter and
optimizer state when the guard asks, through the documented Python adapter. That lets the guard
request a checkpoint against a wall-time budget and lets a second run resume from it.

## What each arm shows

| Arm | Recipe | Mode | Limit | Peak footprint | Outcome |
|---|---|---|---:|---:|---|
| L1a, L1b | LoRA | observe | — | 3.80, 3.55 GiB | baseline peaks, two runs for spread |
| L2 | LoRA | enforce | 4.75 GiB | 3.52 GiB | stays silent; a limit with headroom does not fire |
| L3 | LoRA | enforce | 3.38 GiB | 3.47 GiB | breaches by 98 MB, guard sends a graceful TERM |
| C0 | cooperative worker | observe | — | 5.46 GiB | calibration for the cooperative arms |
| C1 | cooperative worker | enforce + wall-time | 8.19 GiB | 6.02 GiB | wall-time budget triggers a cooperative checkpoint at step 183 |
| C2 | cooperative worker | enforce, resume | 8.19 GiB | 6.23 GiB | resumes from C1's checkpoint, step 183 to 450, exits clean |
| F0 | image gen | enforce | 25 GiB | 25.14 GiB | stays silent; one sample grazes the limit, the two-breach rule holds |
| F3 | image gen | enforce | 15 GiB | 15.10 GiB | breaches while loading, guard sends a graceful TERM |
| T0 | terminal check | — | — | — | a real terminal on stdin is refused (exit 64); `< /dev/null` runs |
| L0 | LoRA, as first printed | observe | — | 0.55 GiB | records that the bare `mlx-lm` command needs its `[train]` extra |

Peaks are the guard's OS-accounted `phys_footprint`, the highest sample in each report. The two
`graceful` outcomes, L3 and F3, are TERM interventions: the footprint crossed the limit and stayed
in the band below the emergency threshold for the two consecutive samples the policy requires, so
the guard sent SIGTERM to its owned process group instead of an immediate KILL.

## The image recipe on a 32 GB machine

The FLUX.1-dev transformer loads in bf16 before it quantizes, so this recipe's footprint peaks near
25 GiB, right at this machine's usable ceiling. Two rows in the table follow from that.

F0 keeps its limit at 25 GiB, at the peak. One late sample reached 25.14 GiB, just over the limit,
and the guard did not intervene: the policy acts only on two consecutive samples at or above the
limit, or one at the emergency threshold of 27.5 GiB, which no sample reached. A single transient
spike over the limit is tolerated by design, and F0 is that behavior on a real workload.

F3 makes the guard intervene without pushing the machine to its memory ceiling. Its 15 GiB limit is
a point the run crosses while the model is still loading, so the guard caught the breach at
15.10 GiB, well below the ceiling, and terminated the run. Because the recipe already peaks at the
ceiling, there is no headroom above it for a large-limit image arm on 32 GB; F0 covers the
must-not-fire case at the ceiling instead.

Captured 2026-09-06.
