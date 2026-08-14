# mlx-guard

External runtime safety supervision for MLX workloads on Apple Silicon.

`mlx-guard` is an application-neutral circuit breaker for MLX commands. A small native parent
samples macOS-accounted process footprint, requests an optional cooperative checkpoint, and escalates
TERM and KILL against an explicit limit. The enforcement loop stays outside Python and the MLX
process.

Version 0.1 is an alpha release. The Rust CLI supervises a directly launched command,
samples its owned process group, applies memory and wall-time policy, handles terminal signals, and
writes a crash-resilient local report. The Python package provides a typed external client and an
optional dependency-free worker checkpoint helper.

Every run requires a report path in an existing owner-only directory:

```bash
mkdir -m 700 reports
mlx-guard run --max-footprint 26GiB --report reports/train.json -- python train.py
```

Use a unique report name for each run. The owner-only journal is retained as recovery evidence and
must be archived or removed deliberately before reusing its report path.

See [the command-line contract](https://github.com/IonDen/mlx-guard/blob/main/docs/CLI.md) for the
exact unit grammar, exit codes, signal rules, and noninteractive terminal boundary. See the
[policy contract](https://github.com/IonDen/mlx-guard/blob/main/docs/POLICY.md) for thresholds,
measurement quality, checkpoint evidence, and escalation timelines. The
[report and privacy contract](https://github.com/IonDen/mlx-guard/blob/main/docs/REPORTS.md) defines
schema v1 and default redaction. The
[process-control contract](https://github.com/IonDen/mlx-guard/blob/main/docs/PROCESS_CONTROL.md)
defines the owned group, direct exec, signal targets, and noninteractive terminal boundary. The
[identity and containment contract](https://github.com/IonDen/mlx-guard/blob/main/docs/IDENTITY_AND_CONTAINMENT.md)
defines PID reuse, descendant discovery, escape evidence, aggregation, and cleanup limits. The
[footprint sampling contract](https://github.com/IonDen/mlx-guard/blob/main/docs/SAMPLING.md) defines
measurement windows, freshness, partial results, bounded history, and sleep/wake behavior. The
[observe and calibration guide](https://github.com/IonDen/mlx-guard/blob/main/docs/OBSERVE_AND_CALIBRATION.md)
explains advisory system metrics, pre-launch warnings, and how to choose an explicit limit from
repeated safe runs. The
[M1 Max 32 GB reference bundle](https://github.com/IonDen/mlx-guard/blob/main/evidence/v0.1.0/m1-max-32gb/README.md)
contains the raw v0.1 accuracy, timing, endurance, lifecycle, and false-intervention measurements.
The [checkpoint protocol](https://github.com/IonDen/mlx-guard/blob/main/docs/CHECKPOINT_PROTOCOL.md)
defines FD-only readiness, nonce-bound frames, deadline handling, signal safety, and redacted worker
acknowledgements. The
[intervention execution contract](https://github.com/IonDen/mlx-guard/blob/main/docs/INTERVENTION.md)
defines action targets, policy-owned deadlines, typed failures, bounded evidence, and post-action
observation. The
[Python packaging contract](https://github.com/IonDen/mlx-guard/blob/main/docs/PYTHON_PACKAGING.md)
defines wheel support, native-binary discovery, editable installs, and source-distribution policy.
The [Python API guide](https://github.com/IonDen/mlx-guard/blob/main/docs/PYTHON_API.md) covers typed
configuration, synchronous and incremental runs, cancellation, output capture, report loading, and
cooperative worker checkpoints. The
[mlx-train-perf integration guide](https://github.com/IonDen/mlx-guard/blob/main/docs/integrations/MLX_TRAIN_PERF.md)
maps its existing worker guardrails and artifacts to optional external supervision without removing
the direct-launch fallback.

Start with the [examples](https://github.com/IonDen/mlx-guard/blob/main/docs/EXAMPLES.md). The
[support matrix](https://github.com/IonDen/mlx-guard/blob/main/docs/SUPPORT.md),
[threat model](https://github.com/IonDen/mlx-guard/blob/main/docs/THREAT_MODEL.md), and
[security policy](https://github.com/IonDen/mlx-guard/blob/main/SECURITY.md) define the release and
trust boundaries.

## Relationship to MetalGuard

[MetalGuard](https://github.com/Harperbot/metal-guard) provides MLX-aware in-application defenses:
load and unload checks, allocator-aware recovery, a Python subprocess runner, and panic cooldowns.
`mlx-guard` operates at a different boundary. It accepts a literal command, measures the
OS-accounted footprint of its owned process group, applies external signal escalation, and writes a
typed report without importing the workload. The projects are complementary and independent.

## Safety boundary

The v0.1 control domain is the process group created for one trusted same-user command. Sampling is
periodic, tree totals are not atomic, and a descendant can leave the group. `mlx-guard` reduces risk;
it cannot promise a hard memory boundary, immediate Metal-driver reclamation, or protection during a
kernel or system-wide failure. It never chooses a destructive limit automatically.

Interactive terminal job control, sandboxed execution, and Mac App Store distribution are outside
the v0.1 scope. Direct CLI and Python-wheel distribution are the target.

## Development

Rust 1.93 is pinned in `rust-toolchain.toml`. The workspace contains the native supervisor, the core
platform and policy library, and hard-bounded real-process fixtures. Full local verification needs
`cargo-audit`; wheel and Metal scripts also need `rg` (ripgrep). The release workflow installs both.

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

Independent community project; not affiliated with or endorsed by Apple.

Release changes are recorded in the
[changelog](https://github.com/IonDen/mlx-guard/blob/main/CHANGELOG.md).

## Licence

Apache License 2.0. The licence permits commercial use without royalties or mandatory payment.
Commercial opportunities, if the project earns adoption, are support, integration, hosted
observability, and enterprise services around the open-source core. Bundled dependency terms are
listed in [THIRD_PARTY_LICENSES.md](https://github.com/IonDen/mlx-guard/blob/main/THIRD_PARTY_LICENSES.md).
