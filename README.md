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
the process it is supposed to stop.

`mlx-guard` supervises from outside. A small native parent launches your command in a process
group of its own and, many times a second, adds up the memory footprint macOS charges to every
process in that group. When the total crosses a limit you chose, the parent can first ask the
workload to save a checkpoint, then sends TERM, then KILL, and leaves a crash-resilient JSON report
of what happened. The enforcement loop never runs inside Python or the MLX process, and the
workload needs no changes to be supervised.

Version 0.2 is an alpha release. The
[stability table](https://github.com/IonDen/mlx-guard/blob/main/docs/STABILITY.md) says which
surfaces may still change before 1.0. If you already use MetalGuard, the two tools guard against
different failures and work together; the
[comparison](https://github.com/IonDen/mlx-guard#metalguard-and-mlx-guard) is at the end of this
page.

## What an intervention looks like

A document summarizer with a memory leak, run under a 6 GiB limit on an M1 Max. The transcript is
abridged from the [tutorial](https://github.com/IonDen/mlx-guard/blob/main/TUTORIAL.md), which
records the whole session, and the report it produced is committed with the
[tutorial bundle](https://github.com/IonDen/mlx-guard/tree/main/evidence/v0.2.0/tutorial).

```console
$ mlx-guard run --max-footprint 6GiB --wall-time 10m --report reports/run-limit.json -- python examples/tutorial/summarize_docs.py --no-checkpoint --progress reports/run-limit-progress.json
mlx-guard: enforcing a 6442450944-byte footprint limit; emergency KILL at 7086696038 bytes, about 10% above the limit
71 pages, 71 to do, keep_caches=True
page 1/71 in 1.2s; mlx active 1.79 GiB; caches held 1
...
page 39/71 in 1.0s; mlx active 4.99 GiB; caches held 39
page 40/71 in 0.8s; mlx active 5.07 GiB; caches held 40
mlx-guard: policy_intervention at 40016ms; 702 samples, 1 signal
$ echo $?
75
```

Forty seconds in, two consecutive samples were at or above the limit. The supervisor sent `SIGTERM`
to the whole process group and the command exited `75`, the code reserved for a policy
intervention. The report says the same thing in a form a script can read:

```json
"outcome": {
  "at_ms": 40016,
  "kind": "policy_intervention",
  "final_footprint_bytes": { "status": "available", "value": 6499453256 },
  "child_status": { "status": "signaled", "signal": 15 }
},
"signals": [
  { "at_ms": 39964, "signal": 15, "target": "owned_process_group", "result": "delivered", "reason": "footprint" }
]
```

The job's own MLX counter read 5.07 GiB on its last line while macOS was charging the process
6.05 GiB. The difference is MLX's buffer cache, the Metal runtime and Python, none of which the
in-process figure includes. The supervisor acts on the number the machine has to find.

## Installation

```bash
pip install mlx-guard
```

The CLI also works without a Python project: `uvx mlx-guard …` runs it on demand, and
`pipx install mlx-guard` keeps it on your PATH.

Wheels are built for Apple Silicon with Python 3.10 through 3.14 and contain the precompiled
supervisor, so installing needs no Rust toolchain. Their `macosx_11_0_arm64` tag is the build's
deployment target, not a runtime claim: the hardware and macOS builds with measured evidence are
in the [compatibility matrix](https://github.com/IonDen/mlx-guard/blob/main/docs/COMPATIBILITY.md).
Building from source needs Rust 1.93 and maturin.

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
memory. Two consecutive samples at or above the limit start the checkpoint and TERM path; a sample
about 10 % above it skips straight to KILL. Budget for that band, because the real ceiling is a
little higher than the number you set. The
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

Commands are literal argument tuples and never pass through a shell. Since 0.2, if the process
that launched the supervisor dies, the supervised command is stopped with it;
`on_parent_exit="detach"` (or `--on-parent-exit detach`) lets it keep running. The
[Python API guide](https://github.com/IonDen/mlx-guard/blob/main/docs/PYTHON_API.md) covers
incremental runs, cancellation, output capture, and the dependency-free `CheckpointWorker` helper
that lets a worker save state when the supervisor asks.

## Safety boundary

The control domain is the process group created for one trusted same-user command. Sampling is
periodic, tree totals are not atomic, and a descendant can leave the group. `mlx-guard` reduces risk;
it cannot promise a hard memory boundary, immediate Metal-driver reclamation, or protection during a
kernel or system-wide failure. It never chooses a destructive limit automatically. One kernel
failure has a name: the IOGPU driver bug that panics macOS 26.4 and later under Metal workloads
(unfixed as of late August 2026), which can fire with the process footprint well inside any limit
and which no external supervisor can reach. The
[compatibility matrix](https://github.com/IonDen/mlx-guard/blob/main/docs/COMPATIBILITY.md) carries
its signature, and [MetalGuard](https://github.com/Harperbot/metal-guard) is the project that
works on that failure, from inside the MLX process.

An interactive terminal on standard input and shell job control are outside the supported scope,
along with sandboxed execution and Mac App Store distribution. Direct CLI and Python-wheel
distribution are the target.

## Documentation

New here? Read the [tutorial](https://github.com/IonDen/mlx-guard/blob/main/TUTORIAL.md): one real job, a document summarizer with a memory
leak, followed from the first `observe` to a resumed run, with every transcript recorded on the
reference host. For the shortest working commands, see the [examples](https://github.com/IonDen/mlx-guard/blob/main/docs/EXAMPLES.md).

Using it:

| Guide | Covers |
|---|---|
| [Tutorial](https://github.com/IonDen/mlx-guard/blob/main/TUTORIAL.md) | One summarizer job with a memory leak, followed through observe, a limit, a checkpoint, a resume, and the fix |
| [Examples](https://github.com/IonDen/mlx-guard/blob/main/docs/EXAMPLES.md) | The shortest working commands, with one captured run |
| [Observe and calibration](https://github.com/IonDen/mlx-guard/blob/main/docs/OBSERVE_AND_CALIBRATION.md) | Advisory system metrics, pre-launch warnings, choosing a limit |
| [Wrap a command](https://github.com/IonDen/mlx-guard/blob/main/docs/integrations/WRAP_A_COMMAND.md) | Supervising a command-line workload with no adapter, from bare to a forced intervention |
| [Python API](https://github.com/IonDen/mlx-guard/blob/main/docs/PYTHON_API.md) | Typed configuration, incremental runs, cancellation, report loading, worker checkpoints |
| [Python adapter pattern](https://github.com/IonDen/mlx-guard/blob/main/docs/integrations/PYTHON_ADAPTER.md) | Supervising a workload your own library launches, with a cooperative checkpoint and a resume key |
| [mlx-train-perf integration](https://github.com/IonDen/mlx-guard/blob/main/docs/integrations/MLX_TRAIN_PERF.md) | Optional external supervision for its runner, keeping the direct-launch fallback |

Contracts, one per subsystem. A behavior change updates the matching document in the same release:

| Contract | Defines |
|---|---|
| [CLI](https://github.com/IonDen/mlx-guard/blob/main/docs/CLI.md) | Unit grammar, exit codes, signal rules, the noninteractive terminal boundary |
| [Policy](https://github.com/IonDen/mlx-guard/blob/main/docs/POLICY.md) | Thresholds, measurement quality, checkpoint evidence, escalation timelines |
| [Reports and privacy](https://github.com/IonDen/mlx-guard/blob/main/docs/REPORTS.md) | Schema v1 and default redaction |
| [Footprint sampling](https://github.com/IonDen/mlx-guard/blob/main/docs/SAMPLING.md) | Measurement windows, freshness, partial results, sleep/wake behavior |
| [Process control](https://github.com/IonDen/mlx-guard/blob/main/docs/PROCESS_CONTROL.md) | The owned group, direct exec, signal targets |
| [Identity and containment](https://github.com/IonDen/mlx-guard/blob/main/docs/IDENTITY_AND_CONTAINMENT.md) | PID reuse, descendant discovery, escape evidence, cleanup limits |
| [Checkpoint protocol](https://github.com/IonDen/mlx-guard/blob/main/docs/CHECKPOINT_PROTOCOL.md) | FD-only readiness, nonce-bound frames, deadlines, redacted acknowledgements |
| [Intervention execution](https://github.com/IonDen/mlx-guard/blob/main/docs/INTERVENTION.md) | Action targets, policy-owned deadlines, typed failures, post-action observation |

Support, security and evidence:

| Document | Covers |
|---|---|
| [Stability](https://github.com/IonDen/mlx-guard/blob/main/docs/STABILITY.md) | What may still change before 1.0, how, and what freezes |
| [Support matrix](https://github.com/IonDen/mlx-guard/blob/main/docs/SUPPORT.md) | Supported platforms and release boundaries |
| [Compatibility matrix](https://github.com/IonDen/mlx-guard/blob/main/docs/COMPATIBILITY.md) | Which hardware setups have measured evidence, which are untested, and how to fill a cell |
| [Python packaging](https://github.com/IonDen/mlx-guard/blob/main/docs/PYTHON_PACKAGING.md) | Wheel support, native-binary discovery, editable installs, sdist policy |
| [Threat model](https://github.com/IonDen/mlx-guard/blob/main/docs/THREAT_MODEL.md) | Trust boundaries and supported failures |
| [Security policy](https://github.com/IonDen/mlx-guard/blob/main/SECURITY.md) | Vulnerability reporting |
| [M1 Max 32 GB evidence](https://github.com/IonDen/mlx-guard/blob/main/evidence/v0.2.0/m1-max-32gb/README.md) | Raw 0.2 accuracy, timing, endurance, lifecycle, false-intervention, and escalation-envelope measurements |

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

## MetalGuard and mlx-guard

Both projects exist because MLX work can take a whole Mac down. They go after different failures
from different places.

| | [MetalGuard](https://github.com/Harperbot/metal-guard) | mlx-guard |
|---|---|---|
| The failure it goes after | The Apple GPU driver bug that kernel-panics the whole Mac during MLX work. It avoids the known triggers, and after a panic it explains the report and holds new runs back for a cooldown | One command whose memory footprint or run time gets out of hand: a leak, a paging storm, a stuck job |
| Where it runs | Mostly inside your Python process, as a library around the MLX code you write. It also ships a CLI, an optional shell guard that pauses MLX launches during a cooldown, and a runner that puts MLX in a child process | Outside, as a separate native parent of any command |
| What it measures | `mx.metal.get_active_memory()`, with `vm_stat` system totals as fallback | The `phys_footprint` macOS accounts to each process in the owned group, summed |
| What it needs from you | Import it and route model loads, unloads and inference through its gates, or install the shell guard | Nothing inside the workload: a command line and a byte limit |
| When things go wrong | Load and unload checks, allocator-aware recovery, crash-burst and kernel-panic cooldowns, panic postmortems, a registry of known-panic models | An optional cooperative checkpoint request, then TERM and KILL against the explicit limit, plus a redacted JSON report that carries a resume key |
| Across runs | Remembers load cadence and panic history, with a circuit breaker and a lockout that survive a reboot | Remembers nothing: each run stands alone and hands over one report |
| Fits | MLX apps, servers and pipelines that load and unload models and want panic avoidance and recovery built in | Trainers, servers, benches, shell scripts, anything you can launch, in any language |

Running both is reasonable. MetalGuard keeps the workload healthy from the inside and is the only
one of the two that does anything about the driver panic. mlx-guard is the outer ring for the case
where the process itself can no longer be trusted, since a limit set inside a process shares that
process's fate. An outside, OS-accounted number also cross-checks the in-process counters: the run
at the top of this page shows a gigabyte that MLX's active-memory figure leaves out by design, and
MetalGuard's maintainer notes that in-process counters may not see every allocation either. He
reviewed this boundary and called the projects complementary, with no overlapping code
([metal-guard #7](https://github.com/Harperbot/metal-guard/issues/7#issuecomment-5307251324)).

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
