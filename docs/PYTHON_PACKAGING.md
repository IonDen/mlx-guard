# Python packaging contract

The `mlx-guard` wheel uses maturin `bin` bindings. It contains the Rust `mlx-guard` executable and a
pure-Python `mlx_guard` package. There is no PyO3 module and no enforcement loop inside Python.

## Supported wheel

v0.1 publishes `py3-none-macosx_11_0_arm64`. The Python code supports CPython 3.10 through 3.14,
while the native executable requires Apple Silicon and macOS 11 or later. The wheel includes the
Apache license, a PEP 561 `py.typed` marker, and maturin's CycloneDX Rust SBOM.

`mlx_guard.__version__` comes from installed package metadata. `mlx_guard.binary_version()` runs the
packaged executable with `--version` and requires an exact package-version match.

## Executable discovery

`mlx_guard.binary_path()` does not search `PATH`, inspect the current directory, or accept an
environment override. It resolves `mlx-guard` from the active interpreter's scripts directory,
requires a regular executable file rather than a symlink, finds the matching wheel RECORD entry,
and verifies its SHA-256 digest on every call. This rejects a shadow or replaced binary even if it
prints a plausible version.

Failures use stable, path-free messages. The public exception classes and exact messages are:

| Condition | Exception | Message |
|---|---|---|
| Missing file | `BinaryDiscoveryError` | `packaged supervisor is missing` |
| Symlink or non-file | `BinaryDiscoveryError` | `packaged supervisor is not a regular file` |
| No execute bit | `BinaryDiscoveryError` | `packaged supervisor is not executable` |
| Missing RECORD | `PackageIntegrityError` | `package installation has no file integrity metadata` |
| Binary absent from RECORD | `PackageIntegrityError` | `packaged supervisor is absent from file integrity metadata` |
| Missing SHA-256 in RECORD | `PackageIntegrityError` | `packaged supervisor has no SHA-256 integrity metadata` |
| Binary cannot be read | `PackageIntegrityError` | `packaged supervisor integrity check failed` |
| RECORD digest mismatch | `PackageIntegrityError` | `packaged supervisor does not match file integrity metadata` |
| Version command fails | `BinaryVersionError` | `packaged supervisor version check failed` |
| Version output differs | `BinaryVersionError` | `packaged supervisor version does not match the Python package` |

## Development and source builds

`uv sync --locked` installs the project and its development tools from `uv.lock`; the project is
editable and its native binary is rebuilt. The same RECORD and version checks apply. `uv run --locked ruff check
python`, `uv run --locked mypy --strict python`, and `uv run --locked pytest python` are the local
Python quality gates. Pytest treats warnings as errors.

The sdist includes the locked Rust workspace, Python sources, README, and license. Building it
requires Rust 1.93 and maturin 1.13.3. v0.1 release automation should publish the macOS arm64 wheel,
not the sdist, so installers on unsupported platforms do not attempt a local native build.

Run `./scripts/test-wheel.sh` to rebuild the wheel, install it in fresh CPython 3.10 through 3.14
environments, run the complete client and worker-helper suite, test shadow and tamper rejection,
kill the Python launcher during a supervised run, exercise editable installation, and build/install
the sdist.
