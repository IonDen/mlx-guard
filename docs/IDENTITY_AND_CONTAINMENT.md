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

## Checkpoint delivery

The cooperative checkpoint endpoint follows the same discipline as every other direct signal:
negotiation binds the caller's already-established `(pid, process_start_abstime)` on trust, and
delivery revalidates that exact identity immediately before signalling. Inspection and signalling
remain two separate steps; macOS has no `pidfd`-equivalent primitive to bind them atomically, so a
PID recycled inside that sub-millisecond window is still not detectable. This narrows the reuse risk
rather than eliminating it. A mismatched identity is refused as an invalid endpoint before any signal
reaches the target. Permission denial can now be observed at either step — inspecting the endpoint or
signalling it — and both are reported the same way.

## Descendants and escapes

Each snapshot considers the bound root, current owned-group members, current parent chains, and
identities tracked in earlier frames. This lets the tracker record:

- a child that disappeared between enumerations;
- a PID whose start token changed;
- a known child whose parent changed;
- a same-group grandchild observed after its intermediate parent exited;
- a descendant that left the owned group with `setsid`.

A changed parent alone is reparent evidence, not an escape: a double-forked descendant that keeps the
owned process group is recorded only as `Reparented`, because escape requires being outside that
group, not merely under a different parent.

An escape remains evidence for the whole run once a sample observes it. Later group cleanup cannot
turn that run into a claim that an arbitrary daemon was contained. Escape detection happens only on
the sample that catches it, so detection latency is bounded by the configured `--sample-interval`,
and a descendant that escapes and exits again between two samples leaves no evidence at all. A child
that leaves both the owned process group and its parent link before observation first sees it — a
daemonizer that detaches faster than one sample interval — is never recognized as an escape at all.

An exited process reports no process group, so it is never recorded as an escape. Each distinct
escaped identity contributes at least one increment to the escape count for the run. Above the
retained-evidence cap the counted mark lives only on the tracked set, so an identity that a sample
misses and later re-observes can add a further increment. The count is bounded evidence of distinct
escapes, not an exact census. The report persists this count, never the escaped identities' pids.

## Aggregation

Only live observations with validated identities and the owned PGID enter the aggregate. If any
owned member lacks footprint data, the result is `incomplete` with known bytes and the missing
identities. A newly listed relevant PID may fail inspection before its start token is known; every
such failure except confirmed disappearance also makes the result incomplete, with the PID and typed
failure retained rather than an invented identity. Arithmetic overflow is a separate result. Missing
root evidence also makes the aggregate incomplete until root exit was actually observed.

Summing per-process physical footprint can double count pages shared by related processes. The
aggregate is a repeatable intervention input for the observed tree, not a unique-page total or a
claim about all machine memory.

## Cleanup evidence

Cleanup polling combines identity-bound snapshots with a direct nonzero PGID existence check. It
reports the bound survivors at the deadline. An owned group is empty only when no live validated
member is observed and the operating system reports no such group. An empty owned group is not
containment: an escaped descendant sits outside the group by definition, so neither the cleanup TERM
nor an escalated KILL, both scoped to the owned group, can reach it. Enumeration uncertainty prevents
a complete result. Any observed escape also keeps the overall cleanup result incomplete, even if the
owned group itself becomes empty. Because an exited child is never recorded as an escape, an ordinary
child that exits before cleanup polls no longer marks that result incomplete on its own.

Real-process tests exercise child churn, same-group double-fork reparenting, live `setsid` escape, a
daemonized grandchild counted as an escape, flooding past the evidence cap while still counting every
escape exactly, retained root zombies, TERM-resistant survivors, exact-start direct signalling, and
protection of an unrelated or reused PID.
