# mlx-guard 0.1.0

`mlx-guard` 0.1.0 is the first alpha release of an external runtime safety supervisor for MLX
workloads on Apple Silicon. It launches one command in an owned process group, samples the group's
macOS-accounted footprint, and applies an explicit memory limit and optional wall-time limit.

The release includes a native Rust supervisor and a typed Python client for CPython 3.10–3.14. A
worker may also opt into a nonce-bound cooperative checkpoint request before TERM/KILL escalation.
Reports are local schema-v1 JSON with bounded evidence and path-free command identity by default.

The release target is the `py3-none-macosx_11_0_arm64` wheel. macOS 11 or later and Apple Silicon
are required. Intel Macs and other operating systems are not release targets.

Important limits: sampling is periodic rather than atomic; descendants can escape the process
group; same-user hostile workloads are outside the threat model; and Metal allocations may remain
charged after process termination. Use `observe` across repeated representative runs before
choosing a destructive limit.

Start with the [examples](https://github.com/IonDen/mlx-guard/blob/main/docs/EXAMPLES.md), then read
the [support matrix](https://github.com/IonDen/mlx-guard/blob/main/docs/SUPPORT.md) and
[threat model](https://github.com/IonDen/mlx-guard/blob/main/docs/THREAT_MODEL.md).
