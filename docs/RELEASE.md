# Release procedure

Releases are built from a clean, reviewed `main` commit. Version values in `Cargo.toml`, crate
manifests, `Cargo.lock`, and the changelog must agree before tagging.

For each new release, update every literal version pin in `release.yml` and `test-wheel.sh`, including
tag filters, artifact names, expected package metadata, and test filenames. The tag workflow installs
its own locked `cargo-audit` version instead of relying on the mutable runner image; artifact checks
otherwise use baseline macOS command-line tools.

## Repository setup

Create a protected GitHub environment named `pypi`. Configure a pending PyPI Trusted Publisher for
owner `IonDen`, repository `mlx-guard`, workflow `release.yml`, and environment `pypi`. Do not store a
PyPI API token in repository secrets. Require approval for the environment if the repository policy
supports it.

## Preflight

On an Apple Silicon Mac, run:

```bash
./scripts/test-full.sh
./scripts/build-release.sh dist
./scripts/test-wheel.sh dist
(cd dist && shasum -a 256 -c SHA256SUMS)
```

Local preflight requires `cargo-audit`; artifact and Metal scripts use baseline macOS tools.

Review `RELEASE_NOTES.md`, the generated wheel, source distribution, external CycloneDX SBOM, and
`SHA256SUMS`. Confirm that archive scans report no checkout paths, workspace-only files, or
secret-like material. Re-run the reference calibration only when runtime behavior, timing, policy,
or measurement code has changed.

## Publish

Create and push the signed tag `v0.1.0` only after the preflight and branch checks pass. The release
workflow rebuilds and tests the artifacts on macOS, then the isolated `pypi` job uses Trusted
Publishing to upload the wheel with a PyPI publication attestation. The sdist remains a reviewed
source artifact because unsupported platforms must not fall back to a native build. Publication is
intentionally not performed from a developer workstation.

No standalone binary is published in v0.1 because it is not Developer-ID signed and notarized.
Homebrew distribution remains a separate future validation and is not part of this release.

After publication, verify the PyPI files and attestations against `SHA256SUMS`, install the wheel in
a fresh environment, run `mlx-guard --version` and `mlx_guard.binary_version()`, create the GitHub
release from `RELEASE_NOTES.md`, and update the changelog comparison links for the next version.
