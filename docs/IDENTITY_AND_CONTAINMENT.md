# Identity and containment contract

Process discovery is sampled and non-atomic. mlx-guard binds every observed process to
`(pid, process_start_abstime)` on macOS and revalidates both values before a direct signal. A PID
alone is never accepted as continuing identity. A changed start token is `stale`; disappearance,
permission denial, malformed metadata, unsupported access, and enumeration failure remain distinct
results.

The Darwin adapter obtains the start and exit tokens plus OS-accounted physical footprint from
`proc_pid_rusage(RUSAGE_INFO_V0)`. It reads parent and process-group metadata with
`proc_pidinfo(PROC_PIDTBSDINFO)`, then samples the start token again to detect reuse during the
window. An exited, unreaped root may retain rusage data after BSD metadata is gone; in that case the
adapter records the validated identity and exit without inventing parent or PGID values.

A Linux `/proc` adapter exercises the same identity and topology rules in CI. It does not provide
Darwin physical footprint and does not expand the supported runtime claim beyond Apple Silicon.

## Descendants and escapes

Each snapshot considers the bound root, current owned-group members, current parent chains, and
identities tracked in earlier frames. This lets the tracker record:

- a child that disappeared between enumerations;
- a PID whose start token changed;
- a known child whose parent changed;
- a same-group grandchild observed after its intermediate parent exited;
- a descendant that left the owned group with `setsid`.

An escape remains evidence for the whole run. Later group cleanup cannot turn that run into a claim
that an arbitrary daemon was contained. Discovery can still miss a process that forks, escapes, and
exits entirely between snapshots.

## Aggregation

Only live observations with validated identities and the owned PGID enter the aggregate. If any
owned member lacks footprint data, the result is `incomplete` with known bytes and the missing
identities. Arithmetic overflow is a separate result. Missing root evidence also makes the aggregate
incomplete until root exit was actually observed.

Summing per-process physical footprint can double count pages shared by related processes. The
aggregate is a repeatable intervention input for the observed tree, not a unique-page total or a
claim about all machine memory.

## Cleanup evidence

Cleanup polling combines identity-bound snapshots with a direct nonzero PGID existence check. It
reports the bound survivors at the deadline. An owned group is empty only when no live validated
member is observed and the operating system reports no such group. Enumeration uncertainty prevents
a complete result. Any observed escape also keeps the overall cleanup result incomplete, even if the
owned group itself becomes empty.

Real-process tests exercise child churn, same-group double-fork reparenting, live `setsid` escape,
retained root zombies, TERM-resistant survivors, exact-start direct signalling, and protection of an
unrelated or reused PID.
