# Support matrix

## Release target

| Component | Supported in 0.1.x |
|---|---|
| Hardware | Apple Silicon (`arm64`) |
| Verified local runtime | macOS 26.6.2 (see the [compatibility matrix](COMPATIBILITY.md)) |
| Required release CI | GitHub `macos-15` arm64 |
| Python | CPython 3.10–3.14 |
| Installation | `py3-none-macosx_11_0_arm64` wheel |
| Rust source build | Rust 1.93 and maturin 1.13.3 |
| Report schema | Major version 1 |

Intel Macs, Linux, Windows, PyPy, an interactive terminal on standard input, shell job control,
sandboxed workers, and Mac App Store distribution are unsupported. Linux CI exercises portable Rust
policy and serialization code; it does not make Linux a runtime target.

## Verified configurations

The [compatibility matrix](COMPATIBILITY.md) lists every hardware setup with committed evidence
(the M1 Max 32 GB reference host and GitHub's `macos-15` shared virtual machine), marks the rest
untested, and explains how a bundle from another machine fills a cell. The wheel's
`macosx_11_0_arm64` deployment tag does not establish runtime support on macOS 11–14.

## Getting help

For reproducible defects, open a GitHub issue with the package version, `uname -m`, macOS version,
the path-free command shape, limit values, exit code, and a redacted report. Use the private process
in `SECURITY.md` for vulnerabilities. Performance expectations and safe limits are workload- and
host-specific; support cannot infer a destructive threshold from hardware capacity alone.
