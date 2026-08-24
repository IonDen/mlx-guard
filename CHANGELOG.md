# Changelog

All notable changes to this project are documented here. The format follows Keep a Changelog, and
versions follow Semantic Versioning.

## [Unreleased]

### Added

- `mlx-guard run` accepts `--checkpoint-timeout DURATION` (`10ms..=60s`) to bound how long a
  requested cooperative checkpoint waits for the worker's authenticated acknowledgement.
- Reports record `outcome.child_status`, the root command's own exit status, independently of
  whichever result ends up owning the process exit code.
- Observe reports record `outcome.owned_group_survivors`, whether owned-group members were still
  running when observation ended at root exit.
- Each recorded signal now carries a `reason` describing why it was sent.
- The policy state machine gained a `RootExited` event and a `SkippedRootExited` checkpoint
  disposition to support cleanup after the root command exits early.

### Changed

- The checkpoint acknowledgement timeout default rose from 100ms to 1s. A real cooperative worker
  on a loaded 3-CPU machine missed the former window, and interventions happen under exactly that
  kind of pressure. The timeout still fails closed: an unresponsive worker receives TERM when it
  expires. Pass `--checkpoint-timeout 100ms` to keep the previous behavior.
- When the root command exits while other members of its owned group are still running, `run` now
  sends a cleanup TERM, waits the usual one-second grace period, and escalates to KILL if needed.
  The root's own exit status is still reported unless KILL was required, in which case the process
  exit code is 75. `observe` previously reported a root exit with surviving group members as
  measurement loss (exit 70); it now ends at root exit, reports the root's own status, leaves
  survivors running, and marks them in the report (`owned_group_survivors`).
- A run that loses measurement while enforcement is active now reports process exit code 70 with
  the signals it sent recorded in the report, instead of 75.

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
