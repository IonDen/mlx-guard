# Changelog

All notable changes to this project are documented here. The format follows Keep a Changelog, and
versions follow Semantic Versioning.

## [Unreleased]

## [0.4.0] - 2026-10-31

### Added

- When `run` or `observe` ends with a non-zero exit, `mlx-guard` now prints two more lines on
  stderr after everything it printed before: `mlx-guard: what happened: …` and
  `mlx-guard: what to do: … More: <link>`. They explain the outcome from the report and link to the
  matching section of the new `docs/TROUBLESHOOTING.md`. They never contain the command, environment
  values, paths, or command output, and there is no option to turn them off. A command's own exit
  code that `mlx-guard` does not also use prints nothing. Exit codes, the stdout summary line, and
  report schema v1 are unchanged. A contract parse error (exit 64) now ends its line with a newline
  before the new lines. Scripts that compare stderr exactly on a non-zero run need to relax the
  check or read the exit code or the report.
- `RunResult.explanation` (`str | None`, keyword-only, default `None`) holds the same text as the
  two lines without the `mlx-guard: ` prefix. It is `None` when no lines were printed. Outcomes with
  no report still raise their existing errors.

### Changed

- On a non-zero exit, stderr now ends with two more lines than before, including when the code is
  `128+n` for a signal or one of the command's own codes that `mlx-guard` also uses. A script that
  asserts an empty stderr, or an exact one, on a failing run will see them. Read the exit code or the
  report instead. Exit codes, the stdout summary line, and report schema v1 are unchanged.

### Documentation

- A new troubleshooting page (`docs/TROUBLESHOOTING.md`) explains each outcome and what to do next.
- The reference measurements stay the 0.3.1 bundle on macOS 27.0.1 (`evidence/v0.3.1/m1-max-32gb/`).
  This release changes how a finished run is described, after the last sample, and not sampling,
  policy, identity tracking, or intervention.

## [0.3.1] - 2026-10-28

### Fixed

- A command that has already ended is no longer reported as a supervisor failure. During an
  escalation, a TERM, KILL, or forwarded signal that reaches an owned group with no live member now
  counts as the command having ended: the intervention stands, `child_status` records the exit, and
  the exit code is `75`. Before, a worker that was still tearing down could turn a working
  intervention into `supervisor_failure` (exit `70`), because Darwin refuses a group signal with
  `EPERM` while every member has exited but none is reaped yet. A refusal against a group that still
  has a live member, or that cannot be scanned, stays a failure (`70`). Exit codes and their meanings
  are unchanged, and report schema v1 is unchanged. This applies to `run`: there, such a signal is
  recorded as `process_missing`, and `permission_denied` now means a live member remained. `observe`
  still records a group signal as delivered or refused, without the liveness check.
- `import mlx_guard` now succeeds when the package metadata is missing (vendored copies, broken
  installs, some frozen layouts). In that case `start()`, `supervisor_argv()`, and `load_report()`
  raise `SupervisorDiscoveryError`, and `start()` does so before it launches anything, so the
  documented fallback to a direct launch (`except SupervisorDiscoveryError`) covers it. Reading
  `mlx_guard.__version__` without the metadata raises `importlib.metadata.PackageNotFoundError`.
  Before, the import itself failed with that error. On a normal install `__version__` is unchanged.
- `scripts/calibrate-host.sh` now renders the core counts correctly on macOS 27, which reports
  them in a new four-part form. Before, the provenance line read "0 Performance and 8 Efficiency"
  on an M1 Max. The measurements were never affected.

### Changed

- `CheckpointWorker.poll()` raises `CheckpointCallbackError` with the callback's own exception as
  `__cause__`, so a traceback shows why the save failed. The error message is unchanged.
  `KeyboardInterrupt` and `SystemExit` raised inside the callback still send the failed
  acknowledgement, then propagate unchanged.
- The exit-`74` message for a report path that is already in use is now
  `journal target already exists; choose a new --report path`, so it names the fix.

### Documentation

- The reference measurements were re-captured on macOS 27.0.1 and every bound holds
  (`evidence/v0.3.1/m1-max-32gb/`). The bundle was captured at the commit before the
  version bump, so its binaries report 0.3.0; the supervisor code is identical to the release.
  Anonymous 64 MiB error is 32,816 B (0.049%), the 16-member
  sample-window p95 is 0.186 ms, and external TERM to final report is 56.9 ms at p95. The new
  calibration README records one capture-script defect: its provenance file misreads the core
  count, which this release fixes. The compatibility matrix, support matrix, and packaging notes
  now cite this bundle, and the policy document's escalation envelope carries its numbers. The
  0.2.0 bundle stays as the macOS 26.6.2 measurement.
- State a limit plainly: if the supervisor itself is killed with SIGKILL, the supervised command
  keeps running. It is reparented to launchd in its own process group, only the hidden
  `.<report-name>.journal` remains, and no final report is written. A launcher that stops a guarded
  run should send a catchable signal (TERM or INT), which the supervisor forwards.
- `docs/PYTHON_API.md` says to wait (bounded, for example `wait(timeout=10)`) after
  `GuardProcess.cancel()` before the calling process exits.

## [0.3.0] - 2026-09-23

### Added

- A tutorial (`TUTORIAL.md`) that follows one ordinary job, a local-LLM document summarizer
  with a realistic memory leak, through `observe`, a footprint limit, a cooperative checkpoint
  with a resume loop, and the fix, with every transcript and report recorded on the reference host
  (`evidence/v0.2.0/tutorial/`) and the script under `examples/tutorial/`.
- A README figure (`docs/images/limit-intervention.svg`) of the recorded limit run: sampled
  footprint, warning band, limit, emergency line, and the SIGTERM marker.
  `scripts/render_limit_figure.py` draws it from the committed report, and a test fails when the
  figure and the evidence drift apart.
- The `mlx-train-perf` integration proof, re-recorded on the published 0.2.0 wheel against
  `mlx-train-perf` 0.8.0, the first consumer release that ships the integration
  (`evidence/v0.2.0/mlx-train-perf/`): a checkpointed wall-limit intervention with the partial
  artifact and matching request ids, the failure paths each consumer test pins, and the four
  points of client friction the first consumer met. `docs/integrations/MLX_TRAIN_PERF.md` now
  cites this bundle; the 0.1.0-era one stays as the record of the first adapter.

### Changed

- A terminal on the supervisor's standard input no longer refuses the launch. The supervised
  command receives `/dev/null` instead, and the supervisor prints one stderr line:
  `mlx-guard: standard input is a terminal, so the command reads from /dev/null instead`. Any other
  standard input (a file, a pipe, an explicit `< /dev/null`) reaches the command unchanged and
  prints nothing. Through 0.2.0 the same launch exited `64` with
  `interactive terminal input is unsupported` before starting anything, which is what every README
  quick-start command and the Python example did when typed into Terminal; the CLI retry with the
  same report path then exited `74`. Shell job control is still unsupported: a command
  that must read the keyboard cannot run under `mlx-guard`. The `< /dev/null` advice is removed from
  the README, `docs/EXAMPLES.md`, and `docs/PYTHON_API.md`.
- Documentation only. `docs/PYTHON_API.md` now says which error permits a library to fall back to
  a direct launch (a discovery error raised by `start()`, never the same error type raised later
  by `GuardProcess.wait()` or `GuardProcess.poll()`), and what a worker should do when its
  checkpoint callback fails. `docs/integrations/MLX_TRAIN_PERF.md` states the same fallback rule.
  The README opens with a recorded intervention, turns the quick start into numbered steps, adds
  a "When a run stops" table (exit code, what happened, what to do), lists its limits as bullets,
  groups the documentation index by reader need, and moves the refreshed MetalGuard comparison
  below the development notes.

## [0.2.0] - 2026-09-12

### Added

- A compatibility matrix (`docs/COMPATIBILITY.md`) indexed by hardware setup (chip family and
  tier by unified memory size) in which every cell is verified by a committed bundle,
  community-measured, or untested, with the macOS build, commit, and date of each capture as
  provenance. It also names the failure class no cell covers: the IOGPU driver bug that panics
  macOS 26.4 and later under Metal workloads (unfixed as of late August 2026) and can fire with the
  footprint inside any limit, now listed by signature in the threat model as well.
- `scripts/calibrate-host.sh` produces the calibration bundle on any Apple Silicon Mac in one
  command: six measurement chunks, each written the moment it finishes, resumable after an
  interruption, with the profile label derived from the hardware record's language-independent
  fields and every published file scanned by `scripts/scan-evidence-bundle.sh` for the host's own
  identifiers, its name, its paths, and the usual identifier labels. A hardware evidence bundle
  issue template turns such a bundle into a community-measured cell. The reference-host script is
  now the M1 Max 32 GB guard around it.
- The 0.2.0 reference bundle for the M1 Max 32 GB (`evidence/v0.2.0/m1-max-32gb/`): footprint
  accuracy, sampling cost, intervention latency, lifecycle scenarios, the 30-minute endurance run,
  and a re-captured escalation envelope, with the shared-VM envelope captures pooled again from a
  fresh workflow run.
- A stability table (`docs/STABILITY.md`) states, for each public surface (CLI grammar and flags,
  exit codes, report schema v1, the journal, the checkpoint frame protocol, the Python API, the
  Rust crates, the contract documents, defaults, and published measurement bounds), what may still
  change before 1.0 and how, and what freezes at 1.0. The checkpoint protocol document gains the
  byte offsets of every frame, pinned by a golden-frame test.
- Observe reports now carry a `calibration` section — total, complete, and incomplete sample counts,
  the observed duration, the highest complete aggregate footprint, and the highest positive growth
  rate, always with `observation_only: true` and `safety_certified: false`. Its peak is an unbounded
  running maximum, so it holds a long run's early, highest footprint even after the 4,096-sample
  history ring has evicted the sample it came from — the number a limit is chosen from. The field is
  additive: enforcing runs and reports written before this release do not carry one, and reading
  those back still validates.
- `mlx-guard run` prints the emergency KILL threshold to stderr before the workload starts, so the
  ceiling a run authorizes — about 10 % above the limit — is visible up front rather than only in
  the final report.
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
- A reference-host soak gate. Two opt-in escaping-churn soak tests supervise a workload whose
  children `setsid` out of the owned group at tens of escapes per second for up to an hour, one
  through the in-process sampler and one through the real `mlx-guard` binary, and assert that the
  supervisor's own footprint growth, CPU share, retained escape evidence, and sample window stay
  inside measured ceilings. `scripts/soak-reference-host.sh` runs them together with the existing
  endurance and pid-churn tests as resumable 30-minute chunks and writes the release's soak
  evidence bundle; the release checklist now requires that run whenever supervision code changed.
  The pid-churn endurance test also writes a JSON artifact and accepts durations up to 1800 s.
- A new escalation-envelope instrument measures checkpoint acknowledgement, TERM, and KILL
  timing across real supervised runs. Reference measurements from the M1 Max 32 GB host are
  published in `evidence/v0.2.0/m1-max-32gb/` and summarized in `docs/POLICY.md`, with a
  dispatch-only workflow for shared-VM corroboration.
- An in-house trial exercises mlx-guard against two real, published command recipes: an `mlx-lm`
  LoRA fine-tune and an `mflux` image generation. Under enforced footprint limits it covers a limit
  with headroom that stays silent, a graceful footprint TERM on each recipe, and a wall-time
  cooperative checkpoint with a resume from it. The schema-v1 reports, checksummed journals, and
  provenance are published in `evidence/v0.2.0/in-house-trial/`.
- Reports record `checkpoint.request_id`, `checkpoint.reason`, and `checkpoint.artifact`
  (path-free `kind`/`size_bytes` facts echoed from a completed acknowledgement), so a later
  process can join an interrupted run back to whatever the worker actually saved.
  `docs/PYTHON_API.md` documents the id-tagging idiom and the resulting resume flow.
- Two integration guides under `docs/integrations/`. `WRAP_A_COMMAND.md` takes a command-line
  workload from an unsupervised run to a deliberately forced intervention without touching the
  workload itself, and `PYTHON_ADAPTER.md` shows a library author how to launch a workload under
  supervision, negotiate a cooperative checkpoint, and return a report a later run can resume from.

### Changed

- The README no longer says wheels "cover macOS 11 or newer": the `macosx_11_0_arm64` tag is
  the build's deployment target, and runtime evidence is what the compatibility matrix lists.
- `ObserveConfig` and `RunConfig` are keyword-only. Positional construction, which the 0.1 classes
  accepted, now raises `TypeError`; pass every field by name. Field names are part of the stable
  Python API and field order is not, which is what the stability table promises.
- CI skips the Rust and Python jobs and the dependency audit on pull requests that change only
  documentation (README, CHANGELOG, release notes, `docs/`, `evidence/`). The release-literals
  guard still runs on every pull request, since it reads those files for version strings, and
  every push to `main` still runs the full pipeline.
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
  identity rather than a bare PID. Neither the core nor the CLI crate is published to a registry, so
  this is a note on the API shape rather than a migration anyone has to perform.

### Fixed

- A command that retires child processes quickly no longer trips the fail-closed path. A tracked
  child's exit between two samples was counted as an observation failure, so a workload that ended
  at least one child per sample interval for three intervals (at the default 50 ms, a shell loop of
  50 ms tools) ended `observe` with exit 70 and made `run` terminate a healthy command. A confirmed
  exit is now a containment event and the sample stays complete over the members still alive; the
  root's disappearance and every other failure kind still make the sample unusable. The core
  library's cleanup poll reports complete evidence for an owned group whose members exited before
  the final poll for the same reason.
- A healthy but slow-to-sample workload is no longer at risk of a spurious TERM. A reading was
  previously judged usable only if its process-tree enumeration finished within one
  `--sample-interval`, so on a large process tree, a busy machine, or a short interval an ordinary
  reading could exceed that; three in a row counted as observation failure and terminated a run that
  was well under its limit. The freshness bounds now have a floor independent of the interval — a
  250 ms collection window and a 500 ms sample age — so only a genuine sustained inability to read
  memory fails closed. On the reference M1 Max, real `mlx_lm.lora` fine-tunes held a 1 ms window p95
  against these bounds.
- A command that finished in the instant between the supervisor's first identity inspection and
  its checkpoint endpoint negotiation no longer turns the run into a supervisor failure (exit 70).
  It keeps its own exit status, exactly as a command that finishes before the inspection always
  has; the only difference is the diagnostic naming which step it beat. On a busy machine that
  window is wide enough for `/usr/bin/true` to fall into.
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
- The wrap-a-command LoRA recipe now installs mlx-lm's `train` extra, which carries the `datasets`
  package the Hub dataset path imports; the recipe as printed before this release loaded the model
  and then stopped on that import.
- The wrap-a-command page now states that `mlx-guard` refuses an interactive terminal on standard
  input, and its transcripts include the `< /dev/null` redirect that avoids it. The README and
  support matrix describe the same boundary instead of `job control` alone.

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

[Unreleased]: https://github.com/IonDen/mlx-guard/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/IonDen/mlx-guard/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/IonDen/mlx-guard/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/IonDen/mlx-guard/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/IonDen/mlx-guard/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/IonDen/mlx-guard/releases/tag/v0.1.0
