# Support matrix

## Release target

| Component | Supported in 0.1.x |
|---|---|
| Hardware | Apple Silicon (`arm64`) |
| Verified local runtime | macOS 26.6.1 |
| Required release CI | GitHub `macos-15` arm64 |
| Python | CPython 3.10–3.14 |
| Installation | `py3-none-macosx_11_0_arm64` wheel |
| Rust source build | Rust 1.93 and maturin 1.13.3 |
| Report schema | Major version 1 |

Intel Macs, Linux, Windows, PyPy, an interactive terminal on standard input, shell job control,
sandboxed workers, and Mac App Store distribution are unsupported. Linux CI exercises portable Rust
policy and serialization code; it does not make Linux a runtime target.

## Verified configurations

Reference calibration used an Apple M1 Max with 32 GB unified memory on macOS 26.6.1. Publication is
gated on the complete macOS suite passing on GitHub's `macos-15` arm64 runner. The wheel's
`macosx_11_0_arm64` deployment tag does not establish runtime support on macOS 11–14. Other Apple
Silicon generations are uncalibrated until their own evidence has been collected.

## Getting help

For reproducible defects, open a GitHub issue with the package version, `uname -m`, macOS version,
the path-free command shape, limit values, exit code, and a redacted report. Use the private process
in `SECURITY.md` for vulnerabilities. Performance expectations and safe limits are workload- and
host-specific; support cannot infer a destructive threshold from hardware capacity alone.
