# Changelog

All notable changes to this project are documented here. The format follows Keep a Changelog, and
versions follow Semantic Versioning.

## [Unreleased]

No user-visible changes yet.

## [0.1.0] - 2026-08-12

### Added

- Native macOS supervisor for explicit memory and wall-time limits.
- Apple Silicon process-group sampling, containment evidence, checkpoint negotiation, and bounded
  TERM/KILL escalation.
- Crash-resilient, privacy-reduced schema-v1 reports.
- Typed Python client and dependency-free cooperative checkpoint helper for CPython 3.10–3.14.
- Reference M1 Max calibration and a measured `mlx-train-perf` consumer integration.

### Security

- Owner-only report storage, descriptor-anchored artifact operations, package RECORD verification,
  strict Python report permissions, unpredictable checkpoint request IDs, locked dependencies,
  CycloneDX SBOM, release checksums, and a self-contained Trusted Publishing workflow.

[Unreleased]: https://github.com/IonDen/mlx-guard/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/IonDen/mlx-guard/releases/tag/v0.1.0
