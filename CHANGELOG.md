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
- Reports record `checkpoint.request_id`, `checkpoint.reason`, and `checkpoint.artifact`
  (path-free `kind`/`size_bytes` facts echoed from a completed acknowledgement), so a later
  process can join an interrupted run back to whatever the worker actually saved.
  `docs/PYTHON_API.md` documents the id-tagging idiom and the resulting resume flow.
- Two integration guides under `docs/integrations/`. `WRAP_A_COMMAND.md` takes a command-line
  workload from an unsupervised run to a deliberately forced intervention without touching the
  workload itself, and `PYTHON_ADAPTER.md` shows a library author how to launch a workload under
  supervision, negotiate a cooperative checkpoint, and return a report a later run can resume from.

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
- `OwnedProcess::negotiate_checkpoint_endpoint` in the core library takes an inspected process
  identity rather than a bare PID. Neither Rust crate is published to a registry, so this is a note
  on the API shape rather than a migration anyone has to perform.

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
- The cooperative checkpoint endpoint is now bound to `(pid, start token)` and revalidated
  immediately before delivery, matching every other direct signal this supervisor sends; a PID
  recycled inside the owned group between negotiation and delivery is refused as an invalid
  checkpoint endpoint rather than signalled. An endpoint whose process had already exited was
  refused before this change too, since macOS reports no process group for an exited process and the
  membership check catches that. What is new is the narrow window between that check and delivery:
  an endpoint that exits inside it is now recorded as `process_missing`, where previously the request
  was signalled, which succeeds even for a process that has exited, and then waited out the full
  checkpoint timeout for an acknowledgement that could not arrive. A transient failure while
  inspecting the endpoint — the likeliest failure under exactly the memory pressure this supervisor
  exists to police — now also cancels the checkpoint request and escalates to termination, where
  previously nothing stood between the policy and the signal call. Permission denial can now
  originate from either the inspection or the signal itself.

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
