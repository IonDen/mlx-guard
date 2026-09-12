# mlx-guard 0.2.0

`mlx-guard` 0.2.0 is the second alpha of the external runtime safety supervisor for MLX workloads
on Apple Silicon. It launches one command in an owned process group, samples the group's
macOS-accounted footprint, and applies an explicit memory limit and optional wall-time limit from
outside the process. This release is about hardening what 0.1.0 shipped: the fail-closed paths that
could terminate a healthy run are fixed, the awkward exits (root gone, parent gone, terminal hung
up) have defined behavior, and every public surface now has a written stability status and a
committed measurement behind its published bounds.

Three changes need attention when upgrading. `ObserveConfig` and `RunConfig` are keyword-only;
positional construction raises `TypeError`. When the process that launched `mlx-guard` exits, the
owned group is now terminated by default and the run reports a policy intervention (exit 75); pass
`--on-parent-exit=detach` or `on_parent_exit="detach"` to keep the old behavior. The cooperative
checkpoint acknowledgement timeout rose from 100 ms to 1 s, and `run` accepts
`--checkpoint-timeout` (10ms to 60s) to set it.

Two bugs in 0.1.0 could stop a workload that was under its limit. A tracked child exiting between
samples counted as a missing sample, so a command that retired short-lived children quickly tripped
the fail-closed path in three intervals. A reading whose process-tree walk took longer than one
sample interval was judged unusable, so a busy machine or a large tree could do the same. A
confirmed child exit is now a containment event, and the freshness bounds have a floor independent
of the interval. Both fixes are pinned by real-process tests and by the release's soak run.

Exit codes moved in two places. A root command that exits while other group members are still
running gets a cleanup TERM, a one-second grace, and KILL if needed; the root's own status is
reported unless KILL was required. A run that loses measurement while enforcement is active now
exits 70 with its signals recorded, not 75. SIGHUP is captured and forwarded like SIGINT and
SIGTERM, so closing the terminal no longer kills the supervisor without a report.

Reports gained the fields a later process needs: the root's own `child_status`, survivors at root
exit, a `reason` on every signal, `escaped_count`, the parent-watch state, and on a completed
checkpoint the `request_id`, `reason`, and path-free `artifact` facts, so a resume can join an
interrupted run back to what the worker actually saved. Observe reports carry a `calibration`
section with the highest complete footprint seen over the whole run, which is the number a limit
should be chosen from; it is always marked `safety_certified: false`.

The documentation set grew a stability table (`docs/STABILITY.md`) that says what may still change
before 1.0 and how, a hardware compatibility matrix (`docs/COMPATIBILITY.md`) in which every cell
is either backed by a committed bundle or marked untested, and two integration guides under
`docs/integrations/` for wrapping a command without an adapter and for a Python library that wants
a cooperative checkpoint and a resume key. `scripts/calibrate-host.sh` produces the calibration
bundle for any Apple Silicon Mac in one command, and an issue template turns such a bundle into a
community-measured row.

Evidence for this release lives under `evidence/v0.2.0/`: the M1 Max 32 GB reference bundle
(footprint accuracy, sampling cost, intervention latency, lifecycle scenarios, a 30-minute endurance
run, and the escalation envelope, pooled with captures from GitHub's shared `macos-15` runner), a
two-hour soak against escaping process churn, and an in-house trial that ran an `mlx-lm` LoRA
fine-tune and an `mflux` image generation under real limits. The runtime evidence is from macOS
26.6.2; the release workflow must also pass on the `macos-15` arm64 runner before publication.
Intel Macs and other operating systems are not release targets.

The limits from 0.1.0 still hold. Sampling is periodic rather than atomic, a descendant can leave
the process group, same-user hostile workloads are outside the threat model, and Metal allocations
may remain charged after termination. One failure now has a name in the threat model: the IOGPU
driver bug that panics macOS 26.4 and later under Metal workloads can fire with the footprint well
inside any limit, and no external supervisor can reach it. Use `observe` across repeated
representative runs before choosing a destructive limit.

Start with the [examples](https://github.com/IonDen/mlx-guard/blob/main/docs/EXAMPLES.md), then
read the [changelog](https://github.com/IonDen/mlx-guard/blob/main/CHANGELOG.md),
the [stability table](https://github.com/IonDen/mlx-guard/blob/main/docs/STABILITY.md), and the
[compatibility matrix](https://github.com/IonDen/mlx-guard/blob/main/docs/COMPATIBILITY.md).
