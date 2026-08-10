# mlx-guard

External runtime safety supervision for MLX workloads on Apple Silicon.

`mlx-guard` is an application-neutral circuit breaker for MLX commands. A small native parent will
sample macOS-accounted process footprint, request an optional cooperative checkpoint, and escalate
TERM and KILL against an explicit limit. The enforcement loop stays outside Python and the MLX
process.

The project is under active v0.1 development. The Darwin footprint API, process identity checks,
owned process-group cleanup, signal forwarding, noninteractive terminal behavior, and bounded Metal
response have working feasibility tests. The checked-in CLI now validates the frozen command
contract, but the supervision runtime is not wired yet.

The intended command shape is:

```bash
mlx-guard run --max-footprint 26GiB -- python train.py
```

See [the command-line contract](docs/CLI.md) for the exact unit grammar, exit codes, signal rules,
and noninteractive terminal boundary. See [the policy contract](docs/POLICY.md) for thresholds,
measurement quality, checkpoint evidence, and escalation timelines. The
[report and privacy contract](docs/REPORTS.md) defines schema v1 and default redaction.

## Safety boundary

The v0.1 control domain is the process group created for one trusted same-user command. Sampling is
periodic, tree totals are not atomic, and a descendant can leave the group. `mlx-guard` reduces risk;
it cannot promise a hard memory boundary, immediate Metal-driver reclamation, or protection during a
kernel or system-wide failure. It never chooses a destructive limit automatically.

Interactive terminal job control, sandboxed execution, and Mac App Store distribution are outside
the v0.1 scope. Direct CLI and Python-wheel distribution are the target.

## Development

Rust 1.93 is pinned in `rust-toolchain.toml`. The workspace currently contains the native command
shell, the core platform boundary, and hard-bounded real-process fixtures.

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
