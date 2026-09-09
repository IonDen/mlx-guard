# Release procedure

Releases are built from a clean, reviewed `main` commit. The workspace version in `Cargo.toml`
is the single source: the release workflow accepts any final `vX.Y.Z` tag, refuses a tag whose
commit is not on `main` or whose version does not equal `Cargo.toml`, and the wheel proof derives
every expected artifact name from it. Pre-release tags are not published.

Two files must never contain the version — `scripts/test-wheel.sh` and
`.github/workflows/release.yml` — and `scripts/check-release-literals.sh` (run in CI) enforces
that, along with requiring the documents below to name the current version. Each release bumps
the version in one commit and refreshes, by hand:

- `Cargo.toml` `[workspace.package] version`, the `=X.Y.Z` core pins in
  `crates/mlx-guard-cli/Cargo.toml` and `crates/mlx-guard-test-support/Cargo.toml` (cargo fails
  closed if they drift), and `Cargo.lock` via `cargo update --workspace` — one commit.
- `CHANGELOG.md`: move `[Unreleased]` into a dated `[X.Y.Z]` section and add the comparison link.
- `RELEASE_NOTES.md`: rewrite for the release (the GitHub Release body and an sdist member).
- `THIRD_PARTY_LICENSES.md`: the version line, and the crate table if dependency pins changed
  (it ships inside the wheel's licence metadata).
- `docs/EXAMPLES.md`: regenerate the captured transcript with the release build and re-date it.
- `README.md`: the status line ("Version X.Y is ..."), and any scope sentences that name the
  previous version.
- Evidence: re-run and commit reference measurements only when runtime, timing, policy, or
  measurement code changed since the last release; when a new bundle lands, move the links in
  `README.md` and `docs/integrations/MLX_TRAIN_PERF.md` to it.
- Escalation envelope: `scripts/calibrate-reference-host.sh` captures
  `escalation-envelope.json` alongside the rest of the reference bundle whenever the checkpoint
  acknowledgement, TERM, or KILL path changed since the last release; re-run it standalone with
  `scripts/measure-escalation-envelope.sh m1-max-32gb <out>` if only that artifact is stale.
- Soak gate: run `scripts/soak-reference-host.sh <out>` on the M1 Max whenever supervision,
  sampling, identity-tracking, or intervention code changed since the last release, and commit the
  bundle it writes to `evidence/vX.Y.Z/m1-max-32gb/soak/` with a README that records the measured
  supervisor footprint, CPU, and sample-window numbers. The four chunks run 30 minutes each and
  the script resumes a run that was interrupted, so a bundle costs about two hours of an otherwise
  idle machine. The soak tests stay `#[ignore]`d in CI.
- If a prior evidence bundle's README notes a supervision-behavior discontinuity and points ahead
  to this version (for example, "see the 0.2.0 bundle"), regenerate the reference-host bundle for
  this version so that reference does not linger unfulfilled.

The tag workflow installs its own locked `cargo-audit` version instead of relying on the mutable
runner image; artifact checks otherwise use baseline macOS command-line tools.

## Repository setup

Create a protected GitHub environment named `pypi`. Configure a pending PyPI Trusted Publisher for
owner `IonDen`, repository `mlx-guard`, workflow `release.yml`, and environment `pypi`. Do not store a
PyPI API token in repository secrets. Require approval for the environment if the repository policy
supports it.

Restrict the environment's deployment branches and tags to `v*` tags, add a tag ruleset that
limits who may create `v*` tags, and protect `main` so releases come only from reviewed merges.
With a single maintainer the environment reviewer is the same person who pushes the tag; the
workflow's `main`-ancestry check is the compensating control until a second reviewer exists.

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

Create and push the signed tag `vX.Y.Z` only after the preflight and branch checks pass. The release
workflow rebuilds and tests the artifacts on macOS, then the isolated `pypi` job uses Trusted
Publishing to upload the wheel with a PyPI publication attestation. The sdist remains a reviewed
source artifact because unsupported platforms must not fall back to a native build. Publication is
intentionally not performed from a developer workstation.

No standalone binary is published because it is not Developer-ID signed and notarized.
Homebrew distribution remains a separate future validation and is not part of this release.

After publication, verify the PyPI files and attestations against `SHA256SUMS`, install the wheel in
a fresh environment, run `mlx-guard --version` and `mlx_guard.binary_version()`, create the GitHub
release from `RELEASE_NOTES.md`, and update the changelog comparison links for the next version.
