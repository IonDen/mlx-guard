# mlx-guard

External runtime safety supervision for MLX workloads on Apple Silicon.

`mlx-guard` is an application-neutral circuit breaker for MLX commands. A small native parent
samples macOS-accounted process footprint, requests an optional cooperative checkpoint, and escalates
TERM and KILL against an explicit limit. The enforcement loop stays outside Python and the MLX
process.

The project is under active v0.1 development. The Rust CLI now supervises a directly launched
command, samples its owned process group, applies memory and wall-time policy, handles terminal
signals, and writes a crash-resilient local report. Python packaging and the external client are
still in development.

Every run requires a report path in an existing owner-only directory:

```bash
mkdir -m 700 reports
mlx-guard run --max-footprint 26GiB --report reports/train.json -- python train.py
```

See [the command-line contract](docs/CLI.md) for the exact unit grammar, exit codes, signal rules,
and noninteractive terminal boundary. See [the policy contract](docs/POLICY.md) for thresholds,
measurement quality, checkpoint evidence, and escalation timelines. The
[report and privacy contract](docs/REPORTS.md) defines schema v1 and default redaction.
The [process-control contract](docs/PROCESS_CONTROL.md) defines the owned group, direct exec,
signal targets, and noninteractive terminal boundary.
The [identity and containment contract](docs/IDENTITY_AND_CONTAINMENT.md) defines PID reuse,
descendant discovery, escape evidence, aggregation, and cleanup limits.
The [footprint sampling contract](docs/SAMPLING.md) defines measurement windows, freshness,
partial results, bounded history, and sleep/wake behavior.
The [observe and calibration guide](docs/OBSERVE_AND_CALIBRATION.md) explains advisory system
metrics, pre-launch warnings, and how to choose an explicit limit from repeated safe runs.
The [checkpoint protocol](docs/CHECKPOINT_PROTOCOL.md) defines FD-only readiness, nonce-bound frames,
deadline handling, signal safety, and redacted worker acknowledgements.
The [intervention execution contract](docs/INTERVENTION.md) defines action targets, policy-owned
deadlines, typed failures, bounded evidence, and post-action observation.

## Safety boundary

The v0.1 control domain is the process group created for one trusted same-user command. Sampling is
periodic, tree totals are not atomic, and a descendant can leave the group. `mlx-guard` reduces risk;
it cannot promise a hard memory boundary, immediate Metal-driver reclamation, or protection during a
kernel or system-wide failure. It never chooses a destructive limit automatically.

Interactive terminal job control, sandboxed execution, and Mac App Store distribution are outside
the v0.1 scope. Direct CLI and Python-wheel distribution are the target.

## Development

Rust 1.93 is pinned in `rust-toolchain.toml`. The workspace contains the native supervisor, the core
platform and policy library, and hard-bounded real-process fixtures.

```bash
./scripts/test-fast.sh          # formatting, Clippy, and all Rust tests
./scripts/test-full.sh          # fast suite plus RustSec and dependency policy
./scripts/test-metal-fixture.sh # 4 KiB Metal worker on macOS
```

The main suite runs on macOS and Linux. The Metal test compiles Objective-C with warnings denied and
uses a 4 KiB shared buffer for no more than five seconds. Synthetic allocation fixtures reject more
than 128 MiB or ten seconds before doing work.

Independent community project; not affiliated with or endorsed by Apple.

## Licence

Apache License 2.0. The licence permits commercial use without royalties or mandatory payment.
Commercial opportunities, if the project earns adoption, are support, integration, hosted
observability, and enterprise services around the open-source core.
