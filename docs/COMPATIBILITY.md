# Compatibility matrix

`mlx-guard` publishes measured bounds (footprint accuracy, sampling cost, intervention latency,
long-run growth), and every one of them was measured on a specific machine. This matrix says which
hardware setups have such a measurement behind them and which do not. It is indexed by hardware,
not by macOS version: the machines this tool targets run a current macOS, and the version at
capture time is recorded with each bundle as provenance rather than treated as an axis.

## What a cell says

| State | Meaning |
|---|---|
| **verified** | A first-party calibration bundle is committed under `evidence/`, every bound in it passed on that hardware, and the bundle names the commit, the macOS build, and the capture date. |
| **verified, shared VM** | A hosted virtual machine with uncontrolled co-tenancy on which the full macOS test suite passes on every push to `main` and pooled escalation-envelope captures are committed. No calibration bundle exists for it, so the published footprint-accuracy, sampling-cost, endurance, and false-intervention bounds are not measured there, and its timings are never a reference. |
| **community-measured** | A bundle submitted through the issue template below, with the submitter's provenance. It is published as received, with the hardware and macOS lines from its own `provenance.json`. |
| **untested** | No bundle exists. Nothing about the published bounds is known to transfer to that hardware. |

A verified cell does not define a safe memory limit for that machine. The bounds describe the
supervisor's own behaviour on the calibration's workloads, which are synthetic fixtures capped at
128 MiB: how closely its samples track OS-accounted footprint, how much it costs, how fast it
acts. Choosing a limit is still the observe-then-enforce procedure in the
[calibration guide](OBSERVE_AND_CALIBRATION.md), on the workload and machine in question.

## Matrix

Rows are chip tiers; the memory column lists the unified-memory sizes with evidence. A tier that
is not listed is untested as well.

| Chip | Memory sizes with evidence | State | Evidence and provenance |
|---|---|---|---|
| Apple M1 | none | untested | — |
| Apple M1 Pro | none | untested | — |
| Apple M1 Max | 32 GB | verified | [0.2.0 reference bundle](https://github.com/IonDen/mlx-guard/blob/main/evidence/v0.2.0/m1-max-32gb/README.md): MacBook Pro (MacBookPro18,2), 10 cores, macOS 26.6.2 (25G83), commit `59ea303`, captured 2026-09-11; the [soak bundle](https://github.com/IonDen/mlx-guard/blob/main/evidence/v0.2.0/m1-max-32gb/soak/README.md) on the same host |
| Apple M1 Ultra | none | untested | — |
| Apple M1 (Virtual), GitHub `macos-15` runner | 7 GB, 3 vCPU | verified, shared VM | Full macOS suite on every push to `main`; [pooled escalation-envelope captures](https://github.com/IonDen/mlx-guard/blob/main/evidence/v0.2.0/github-macos-15-shared/README.md): workflow run 34591393999, commit `9d42fc8`, captured 2026-09-11 |
| Apple M2 | none | untested | — |
| Apple M2 Pro | none | untested | — |
| Apple M2 Max | none | untested | — |
| Apple M2 Ultra | none | untested | — |
| Apple M3 | none | untested | — |
| Apple M3 Pro | none | untested | — |
| Apple M3 Max | none | untested | — |
| Apple M3 Ultra | none | untested | — |
| Apple M4 | none | untested | — |
| Apple M4 Pro | none | untested | — |
| Apple M4 Max | none | untested | — |
| Apple M5 family | none | untested | The M5 GPU's neural accelerators and their memory behaviour are new; nothing measured on M1 is assumed to carry over. |

The wheel is tagged `macosx_11_0_arm64` because that is the deployment target it was built
against. The tag does not establish runtime support on macOS 11 through 14; the verified cells
above were captured on the macOS builds they name.

## Failure classes no cell covers

Some failures can happen with the supervised process's footprint well inside any limit, so no
calibration bundle can speak to them and a clean `mlx-guard` report is not evidence that the
tool would have prevented them.

| Class | Signature | Status |
|---|---|---|
| IOGPU driver bug under Metal workloads, macOS 26.4 and later | Kernel panic in Apple's IOGPU kernel extension: `IOGPUMemory.cpp:550 completeMemory() prepare count underflow`, also `IOGPUGroupMemory.cpp:219` | Reported independently against MLX inference ([mlx #3346](https://github.com/ml-explore/mlx/issues/3346), [mlx #3186](https://github.com/ml-explore/mlx/issues/3186)), distributed inference ([exo #1972](https://github.com/exo-explore/exo/issues/1972)), and a serving stack ([oMLX #557](https://github.com/jundot/omlx/issues/557)) on M1 Max and M3 Ultra hosts, with the footprint inside any limit; reporters state the only mitigation is not using the GPU. Unfixed as of late August 2026. An external supervisor cannot reach the driver bug. The same panic line also appears under sustained over-allocation, which a footprint limit does address, so the signature alone does not say which case fired. |

The [threat model](THREAT_MODEL.md) lists the other failure classes outside the supervisor's
reach: kernel, driver, hardware, and system-wide failures in general, and the gaps between
samples.

## Filling a cell

One command produces a bundle on any Apple Silicon Mac, from a clean checkout of a tagged
release, with the output directory outside the checkout:

```bash
./scripts/calibrate-host.sh /private/tmp/mlx-guard-calibration
```

It runs six measurements as separate chunks and writes each chunk's JSON the moment it finishes:
footprint accuracy against anonymous, shared, and Metal allocations; ramp overshoot and
external-signal latency; decision-to-signal latency; lifecycle scenarios and safe-workload false
interventions; a 30-minute endurance run; and the escalation envelope. An interrupted run resumes
by skipping the chunks that already passed. The whole run takes about 45 minutes, most of it the
endurance chunk, and needs an idle machine on mains power with the lid open.

The bundle's `provenance.json` carries five hardware facts (model name, model identifier, chip,
core count, memory) and the macOS, kernel, and toolchain versions; the profile label, for example
`m3-pro-36gb`, is derived from the chip and memory fields. Before anything is published,
`scripts/scan-evidence-bundle.sh` searches every file, journals included, for the identifier
labels `system_profiler` prints, for any UUID-shaped value, for the serial number and UUIDs the
host reported, for the host name, for the checkout and output paths, and for the usual local-path
prefixes and your home directory, and refuses to publish if it finds one. The cargo transcripts
land in a sibling `.logs` directory that stays with you. A run with a shortened endurance chunk
says so in its last line and is not publishable.

To have the cell filled, open a
[hardware evidence bundle](https://github.com/IonDen/mlx-guard/issues/new?template=hardware-evidence-bundle.yml)
issue with the published directory attached as one zip. The same scan is run again on the
submitted directory before it is committed, and the cell is then published as community-measured
with the provenance from the bundle. The reference host's own cells use the
same script through `scripts/calibrate-reference-host.sh`, which only adds the check that the
machine is the M1 Max 32 GB.
