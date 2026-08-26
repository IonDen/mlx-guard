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
- `mlx-guard run` and `observe` accept `--on-parent-exit terminate|detach` (default `terminate`) to
  control what happens to the owned group when the process that launched `mlx-guard` exits. The
  policy state machine gained a matching `ParentExited` event and `SkippedParentExited` checkpoint
  disposition; unlike root-exit cleanup, a parent exit does not cancel an in-flight checkpoint
  request. Python's `ObserveConfig` and `RunConfig` gained a matching keyword-only `on_parent_exit`.
- The native parent now captures SIGHUP as a third terminal signal alongside SIGINT and SIGTERM,
  forwarding it to the owned group, unless the launching process had already disposed SIGHUP to
  `SIG_IGN` (the `nohup` convention), in which case that disposition is left alone.
- Reports record `configuration.on_parent_exit`, `configuration.parent_watch`, and
  `outcome.parent_exited_at_ms`, and signal records gained the `parent_exit` reason — whether, and
  how, a run watched for its launching parent's exit, and when that exit was confirmed.
- Reports record `escape.escaped_count`, present only when nonzero: a count of distinct
  descendants observed outside the owned process group (kept even past the 64-identity evidence
  cap), alongside the existing `escape.detected` boolean. Real-process tests now cover a `setsid`
  escape into an empty owned group, a daemonized grandchild, a plain double-fork reparent that
  stays contained, and a flood of seventy simultaneous escapees past the 64-identity evidence cap.
- Property tests now exercise argument-to-basename projection and full report serialization
  against a seeded adversarial corpus. Real-process tests plant a marker in every launch channel —
  arguments, environment, working directory, executable path, and process output — against the
  packaged binary, then scan its persisted report and journal for a leak.
- Python's `RunConfig` gained a keyword-only `checkpoint_timeout_ms` field (`10ms..=60s`),
  matching the CLI's `--checkpoint-timeout`.
- A new escalation-envelope instrument measures checkpoint acknowledgement, TERM, and KILL
  timing across real supervised runs. Reference measurements from the M1 Max 32 GB host are
  published in `evidence/v0.2.0/m1-max-32gb/` and summarized in `docs/POLICY.md`, with a
  dispatch-only workflow for shared-VM corroboration.

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
- Before this release, a supervisor whose launching parent exited — its shell was killed, its
  terminal closed, or the Python process that started it crashed — kept supervising the orphaned
  command indefinitely. `run` and `observe` now terminate the owned group by default (TERM, a
  one-second grace, KILL if needed) when that happens, and report a policy intervention (exit 75).
  Pass `--on-parent-exit=detach` (or `on_parent_exit="detach"` from Python) to keep the previous
  behavior.

### Fixed

- A command that exits before the supervisor's first identity inspection now keeps its real
  exit status instead of being reported as supervisor failure (exit 70).
- Supervisor memory no longer grows with every distinct child process observed during a long
  run: containment-escape evidence is bounded to 64 identities, and the per-sample identity copy
  was removed. An opt-in pid-churn endurance test pins the bound.
- Before this release, sending SIGHUP to the supervisor killed it outright with no final report.
  It is now captured the same way as SIGINT and SIGTERM: forwarded to the owned group and recorded
  as a signal.
- Before this release, writing the final result summary panicked and exited 101 whenever whoever
  held the supervisor's stdout had already gone away (for example, a client that closed the read
  end of a piped stdout). The write now tolerates a broken pipe silently instead of panicking; any
  other write or flush error still panics.
- Before this release, the internal escape counter kept incrementing on every sample once the bounded
  evidence list had filled, and could count a child that had merely exited as an escape. Neither was
  visible in a report, which carried only the `escape.detected` boolean; both are fixed ahead of the
  count above being published.
- Before this release, report validation accepted a `detach` parent-exit option paired with an
  enforcing or absent parent watch — states a `detach` run cannot actually produce. It now rejects
  that combination as an invalid configuration.

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
