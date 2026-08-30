# mlx-guard

[![CI](https://github.com/IonDen/mlx-guard/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/IonDen/mlx-guard/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/mlx-guard)](https://pypi.org/project/mlx-guard/)
[![Python](https://img.shields.io/badge/python-3.10%2B-blue)](https://pypi.org/project/mlx-guard/)
[![Rust](https://img.shields.io/badge/rust-1.93-orange)](https://github.com/IonDen/mlx-guard/blob/main/rust-toolchain.toml)
[![Platform](https://img.shields.io/badge/platform-macOS%20arm64-lightgrey)](https://github.com/IonDen/mlx-guard/blob/main/docs/SUPPORT.md)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/IonDen/mlx-guard/blob/main/LICENSE)

External runtime safety supervision for MLX workloads on Apple Silicon.

A runaway MLX run does not fail politely. Unified memory lets one training or generation process
push the whole machine into a paging storm, and a limit set inside the process shares the fate of
the process it is supposed to stop. `mlx-guard` supervises from outside: a small native parent
launches your command, samples the OS-accounted memory footprint of the process group it owns,
optionally requests a cooperative checkpoint, escalates TERM and KILL against an explicit limit you
chose, and writes a crash-resilient JSON report of what happened. The enforcement loop never runs
inside Python or the MLX process.

Version 0.1 is an alpha release.

## Installation

```bash
pip install mlx-guard
```

The CLI also works without a Python project: `uvx mlx-guard …` runs it on demand, and
`pipx install mlx-guard` keeps it on your PATH.

Wheels cover macOS 11 or newer on Apple Silicon with Python 3.10 through 3.14 and contain the
precompiled supervisor, so installing needs no Rust toolchain. Building from source needs Rust 1.93
and maturin.

## Quick start

Every run writes a report into an existing owner-only directory. Create one once:

```bash
mkdir -m 700 reports
```

Measure before enforcing. Observe mode samples footprint and never intervenes:

```bash
mlx-guard observe --report reports/observe-1.json -- python train.py --epochs 1
```

Choose a limit from the observed peaks plus workload-specific headroom, not from total machine
memory; the
[calibration guide](https://github.com/IonDen/mlx-guard/blob/main/docs/OBSERVE_AND_CALIBRATION.md)
explains the procedure. Then enforce it:

```bash
mlx-guard run --max-footprint 24GiB --wall-time 2h \
  --report reports/train.json -- python train.py --epochs 10
```

When no intervention occurs the exit code is the child's own. A policy intervention exits `75`, and
the typed report distinguishes the outcomes. Use a unique report name for each run: the owner-only
journal is retained as recovery evidence and must be archived or removed deliberately before a
report path is reused.

The same run from Python:

```python
from pathlib import Path

import mlx_guard

result = mlx_guard.run(
    mlx_guard.RunConfig(
        command=("python", "train.py"),
        report=Path("reports/train.json"),
        max_footprint_bytes=24 * 1024**3,
        wall_time_ms=2 * 60 * 60 * 1000,
    )
)
print(result.returncode, result.report.outcome.kind)
```

Commands are literal argument tuples and never pass through a shell. The
[Python API guide](https://github.com/IonDen/mlx-guard/blob/main/docs/PYTHON_API.md) covers
incremental runs, cancellation, output capture, and the dependency-free `CheckpointWorker` helper
that lets a worker save state when the supervisor asks.

## Safety boundary

The v0.1 control domain is the process group created for one trusted same-user command. Sampling is
periodic, tree totals are not atomic, and a descendant can leave the group. `mlx-guard` reduces risk;
it cannot promise a hard memory boundary, immediate Metal-driver reclamation, or protection during a
kernel or system-wide failure. It never chooses a destructive limit automatically.

Interactive terminal job control, sandboxed execution, and Mac App Store distribution are outside
the v0.1 scope. Direct CLI and Python-wheel distribution are the target.

## MetalGuard and mlx-guard

Both projects exist because a runaway MLX process can take the whole Mac down. They defend
different rings.

| | [MetalGuard](https://github.com/Harperbot/metal-guard) | mlx-guard |
|---|---|---|
| Where it runs | Inside your Python process, around MLX code you write | Outside, as a separate native parent of any command |
| What it measures | `mx.metal.get_active_memory()`, with `vm_stat` system totals as fallback | OS-accounted `phys_footprint` of the owned process group |
| What it needs from you | Import it and route MLX work through its runner and gates | Nothing inside the workload: a command line and a byte limit |
| When things go wrong | Load and unload checks, allocator-aware recovery, crash-burst and kernel-panic cooldowns, panic postmortems, a registry of known-panic models | An optional cooperative checkpoint request, then TERM and KILL against the explicit limit, plus a redacted JSON report |
| Fits | MLX apps that want recovery without a supervisor process | Trainers, servers, benches, shell scripts, anything you can launch |

Running both is reasonable: MetalGuard keeps the workload healthy from the inside, and mlx-guard
is the outer ring for the case where the process itself can no longer be trusted (a limit set
inside a process shares that process's fate). An outside, OS-accounted number also cross-checks
the in-process counters, which MetalGuard's maintainer notes may not see every allocation. He
reviewed this boundary and called the projects complementary, with no overlapping code
([metal-guard #7](https://github.com/Harperbot/metal-guard/issues/7#issuecomment-5307251324)).

## Documentation

Start with the [examples](https://github.com/IonDen/mlx-guard/blob/main/docs/EXAMPLES.md). Each
contract below defines one subsystem.

| Guide | Defines |
|---|---|
| [CLI contract](https://github.com/IonDen/mlx-guard/blob/main/docs/CLI.md) | Unit grammar, exit codes, signal rules, the noninteractive terminal boundary |
| [Policy contract](https://github.com/IonDen/mlx-guard/blob/main/docs/POLICY.md) | Thresholds, measurement quality, checkpoint evidence, escalation timelines |
| [Reports and privacy](https://github.com/IonDen/mlx-guard/blob/main/docs/REPORTS.md) | Schema v1 and default redaction |
| [Process control](https://github.com/IonDen/mlx-guard/blob/main/docs/PROCESS_CONTROL.md) | The owned group, direct exec, signal targets |
| [Identity and containment](https://github.com/IonDen/mlx-guard/blob/main/docs/IDENTITY_AND_CONTAINMENT.md) | PID reuse, descendant discovery, escape evidence, cleanup limits |
| [Footprint sampling](https://github.com/IonDen/mlx-guard/blob/main/docs/SAMPLING.md) | Measurement windows, freshness, partial results, sleep/wake behavior |
| [Observe and calibration](https://github.com/IonDen/mlx-guard/blob/main/docs/OBSERVE_AND_CALIBRATION.md) | Advisory system metrics, pre-launch warnings, choosing a limit |
| [Checkpoint protocol](https://github.com/IonDen/mlx-guard/blob/main/docs/CHECKPOINT_PROTOCOL.md) | FD-only readiness, nonce-bound frames, deadlines, redacted acknowledgements |
| [Intervention execution](https://github.com/IonDen/mlx-guard/blob/main/docs/INTERVENTION.md) | Action targets, policy-owned deadlines, typed failures, post-action observation |
| [Python API](https://github.com/IonDen/mlx-guard/blob/main/docs/PYTHON_API.md) | Typed configuration, incremental runs, cancellation, report loading, worker checkpoints |
| [Python packaging](https://github.com/IonDen/mlx-guard/blob/main/docs/PYTHON_PACKAGING.md) | Wheel support, native-binary discovery, editable installs, sdist policy |
| [Wrap a command](https://github.com/IonDen/mlx-guard/blob/main/docs/integrations/WRAP_A_COMMAND.md) | Supervising a command-line workload with no adapter, from bare to a forced intervention |
| [Python adapter pattern](https://github.com/IonDen/mlx-guard/blob/main/docs/integrations/PYTHON_ADAPTER.md) | Supervising a workload your own library launches, with a cooperative checkpoint and a resume key |
| [mlx-train-perf integration](https://github.com/IonDen/mlx-guard/blob/main/docs/integrations/MLX_TRAIN_PERF.md) | Optional external supervision for its runner, keeping the direct-launch fallback |
| [Support matrix](https://github.com/IonDen/mlx-guard/blob/main/docs/SUPPORT.md) | Supported platforms and release boundaries |
| [Threat model](https://github.com/IonDen/mlx-guard/blob/main/docs/THREAT_MODEL.md) | Trust boundaries and supported failures |
| [Security policy](https://github.com/IonDen/mlx-guard/blob/main/SECURITY.md) | Vulnerability reporting |
| [M1 Max 32 GB evidence](https://github.com/IonDen/mlx-guard/blob/main/evidence/v0.1.0/m1-max-32gb/README.md) | Raw v0.1 accuracy, timing, endurance, lifecycle, and false-intervention measurements |

## Research notes

Two write-ups cover the reasoning behind this design in more depth than a README can, including
the limits the tool cannot clear. They are published at [ineshin.space](https://ineshin.space)
alongside the rest of my Apple Silicon work, and the source Markdown lives under `docs/papers/`.

- [Why the memory limit must live outside the process](https://ineshin.space/papers/why-the-memory-limit-must-live-outside-the-process/)
  — why an in-process cap or watchdog shares the fate of the process it guards, why the counter a
  workload reads is not the charge the OS applies, what macOS gives a supervisor in place of
  cgroups, and what external supervision still cannot promise, with the measured overhead and
  gaps from the committed v0.1 evidence.
- [Measuring a macOS process tree honestly](https://ineshin.space/papers/measuring-a-macos-process-tree-honestly/)
  — the measurement half of the same argument: which OS signal a supervisor can act on, why a
  PID is not an identity, why tree discovery is a race the tool can only record, the rule that
  keeps a partial aggregate from triggering a limit, and the observations in the v0.1 evidence
  that support less than they appear to, including the pages a released Metal buffer does not
  give back within the window watched.

## Development

Rust 1.93 is pinned in `rust-toolchain.toml`. The workspace contains the native supervisor, the core
platform and policy library, and hard-bounded real-process fixtures. Full local verification needs
`cargo-audit`; artifact and Metal scripts use the baseline macOS command-line tools. The release
workflow installs its locked `cargo-audit` version.

```bash
./scripts/test-fast.sh          # formatting, Clippy, and all Rust tests
./scripts/test-full.sh          # fast suite plus RustSec and dependency policy
./scripts/test-metal-fixture.sh # 4 KiB Metal worker on macOS
./scripts/test-wheel.sh         # macOS arm64 wheel across Python 3.10 through 3.14
./scripts/build-release.sh dist # wheel, sdist, SBOM, and SHA-256 manifest
```

The main suite runs on macOS and Linux. The Metal test compiles Objective-C with warnings denied and
uses a 4 KiB shared buffer for no more than five seconds. Synthetic allocation fixtures reject more
than 128 MiB or ten seconds before doing work. The Metal fixture also arms a six-second process alarm
so device setup or a wedged command wait cannot hang the test indefinitely.

Release changes are recorded in the
[changelog](https://github.com/IonDen/mlx-guard/blob/main/CHANGELOG.md).

## Related projects

More MLX tooling for Apple Silicon by the same author:

- [mlx-train-perf](https://github.com/IonDen/mlx-train-perf) — fused, logit-free
  linear-cross-entropy loss, RAM-fit planner, and benchmark harness for MLX fine-tuning; the first
  integration target for external supervision (guide above).
- [mlx-model-doctor](https://github.com/IonDen/mlx-model-doctor) — validate an MLX / Hugging Face
  model repository before you load it.
- [mlx-quant-fidelity](https://github.com/IonDen/mlx-quant-fidelity) — measure what quantization
  costs: KL divergence, perplexity, and top-token agreement for KV cache and weights.
- [mlx-teacache](https://github.com/IonDen/mlx-teacache) — TeaCache step-skipping for FLUX,
  Qwen-Image, and Z-Image diffusion in pure MLX.
- [mlx-taef](https://github.com/IonDen/mlx-taef) — tiny autoencoders (TAESD family) for live
  previews and low-memory latent decode for FLUX and SD models.

Independent community project; not affiliated with or endorsed by Apple.

## Licence

Apache License 2.0. The licence permits commercial use without royalties or mandatory payment.
Commercial opportunities, if the project earns adoption, are support, integration, hosted
observability, and enterprise services around the open-source core. Bundled dependency terms are
listed in [THIRD_PARTY_LICENSES.md](https://github.com/IonDen/mlx-guard/blob/main/THIRD_PARTY_LICENSES.md).
