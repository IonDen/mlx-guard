# Changelog

All notable changes to this project are documented here. The format follows Keep a Changelog, and
versions follow Semantic Versioning.

## [Unreleased]

### Added

- `mlx-guard run` accepts `--checkpoint-timeout DURATION` (`10ms..=60s`) to bound how long a
  requested cooperative checkpoint waits for the worker's authenticated acknowledgement.

### Changed

- The checkpoint acknowledgement timeout default rose from 100ms to 1s. A real cooperative worker
  on a loaded 3-CPU machine missed the former window, and interventions happen under exactly that
  kind of pressure. The timeout still fails closed: an unresponsive worker receives TERM when it
  expires. Pass `--checkpoint-timeout 100ms` to keep the previous behavior.

### Fixed

- A command that exits before the supervisor's first identity inspection now keeps its real
  exit status instead of being reported as supervisor failure (exit 70).
- Supervisor memory no longer grows with every distinct child process observed during a long
  run: containment-escape evidence is bounded to 64 identities, and the per-sample identity copy
  was removed. An opt-in pid-churn endurance test pins the bound.

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
