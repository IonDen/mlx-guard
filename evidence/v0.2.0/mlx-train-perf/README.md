# mlx-train-perf integration proof

This bundle records a bounded end-to-end run through the consumer's shipped runner boundary, on
the published mlx-guard 0.2.0 wheel. The consumer was mlx-train-perf 0.8.0 at commit
`349c2babd6e7093ce24fc20f1e73367c6bab409b`, the first release that ships the integration
(`run_conditions(..., guard=ExternalGuardConfig(...))`, installed with the `guard` extra, which
pins `mlx-guard==0.2.0`). The host was an Apple M1 Max with 32 GB of unified memory, macOS 27.0,
MLX 0.32.0, and mlx-lm 0.31.3. It replaces the [0.1.0-era bundle](../../v0.1.0/mlx-train-perf/README.md)
as the current proof; that one stays as the record of the first adapter.

The runner launched a 1,000-repetition real MLX loss-layer condition with a 512 MiB OS-accounted
footprint limit, a 500 ms wall limit, 10 ms sampling, and a 1 s checkpoint acknowledgement
timeout. At 501 ms the supervisor requested a checkpoint. The worker finished repetition 278,
atomically wrote and synced the artifact normalized as [`partial-result.json`](partial-result.json),
and returned path-free `file` metadata (636 bytes). The supervisor accepted the authenticated
response at 531 ms, sent TERM to the owned process group, and finalized at 554 ms with zero final
footprint. The complete [`report.json`](report.json) contains 43 samples, a 48,464,616-byte maximum
aggregate footprint, and no artifact errors. [`report-summary.json`](report-summary.json) preserves
the policy, checkpoint (with its `request_id`, `reason` and `artifact`), signals, transitions,
outcome (with `child_status`), capabilities, privacy declaration, and hashes of the source
artifacts. [`supervision-record.json`](supervision-record.json) is what the consumer's runner wrote
beside the report from the same run: the outcome kind, how the worker itself ended, and the
checkpoint status, request id, reason and artifact. The request id in the partial artifact matches
the one in the report.

Unlike the 0.1.0 run, this one was recorded under `on_parent_exit=terminate`, the default since
0.2.0, and with keyword-only configurations.

## Failure-path verification

Each path is pinned by a consumer test. Those marked real run the published supervisor and real
workers; the rest use a stand-in for the package.

- Normal completion (real): a supervised condition finishes `ok` and the report says
  `child_exited`, code 0.
- Policy intervention (real): the run above, as a test, asserting the acknowledged checkpoint, the
  `wall_time` reason, and matching request ids.
- Retry after an intervention (real): a second launch of the same condition is not blocked by the
  retained journal, because every launch gets its own report path.
- In-process protection under supervision (real): with no supervisor limit able to trip, the
  consumer's own wall backstop aborts the worker; the report records `child_exited` with code 70
  and the consumer classifies it as the worker's status, by outcome kind rather than exit code.
- Callback error (real): with the artifact directory read-only, the callback's write fails; the
  report shows `requested_unverified` with no artifact, no partial file exists, and the worker
  stays up to be stopped by TERM (`policy_intervention`, child signaled 15).
- Missing package, broken install, failed discovery at `start()`: one recorded direct-launch
  fallback, announced on stderr before the worker starts.
- Discovery error after the run, report or persistence failure, client failure: never a second
  launch; the failure is recorded beside whatever artifact the worker left.
- Runner interrupted while waiting (four hand-driven runs, not included in this bundle): the
  consumer cancels the supervisor and waits a bounded moment; all four ended `child_signaled`,
  signal 2, with nothing left running.

## Integration friction

Four things the first consumer had to learn from the 0.2.0 client, recorded here because the next
consumer will meet them too.

The direct-launch fallback has to be decided by call site, not by exception type. The client
re-verifies the binary while loading the final report, so `SupervisorDiscoveryError` can also
surface from `wait()`, after the command has run. Only a discovery error from `start()` is safe to
fall back on.

A report path cannot be reused. The supervisor retains `.<name>.journal` beside each report and
refuses a path whose journal exists, so a consumer that retries work needs a fresh report path per
attempt.

A broken install raises `PackageNotFoundError` at import, whose `name` is the distribution name
`mlx-guard`, not the import name. A consumer that checks only for `mlx_guard` re-raises it.

A worker should survive a failed checkpoint. `poll()` raises after it has sent the failed
acknowledgement; a slow-exiting MLX worker that died there was sometimes gone before the TERM
arrived, and that run was reported as `supervisor_failure`. Catching `CheckpointError` around
`poll()` and continuing until the signal avoids the race.

No consumer-specific exception, status, condition kind, or policy rule was added to mlx-guard core.
The consumer's wired limit, active-memory watchdog and wall backstop remain the direct-launch
fallback and stay active inside supervised workers. Native reports need a private owner-controlled
directory; the runner creates `_mlx_guard/` with mode 0700 and refuses one that is a symlink or
owned by another user.
