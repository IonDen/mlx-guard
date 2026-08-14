# Footprint sampling contract

mlx-guard measures the owned command tree with the public Darwin `libproc` interfaces. Startup
probes `proc_pid_rusage(RUSAGE_INFO_V0)` instead of assuming that footprint measurement is available.
An unsupported host, denied process, vanished process, stale identity, malformed result, and failed
enumeration remain typed outcomes. None becomes a numeric zero.

## Sampling window

Each attempt records its monotonic start and finish, exact validated process identities, individual
OS-accounted physical footprints, containment events, and aggregate outcome. The supported interval
is 10 ms through 10 s. Policy receives a numeric aggregate only when all of these conditions hold:

- the aggregate is complete and did not overflow;
- the clock did not move backwards and the window has positive duration;
- the window stays within the configured maximum;
- processing occurs within the configured freshness limit.

A partial sample retains known bytes, missing identities, and observation failures for diagnostics,
but cannot trigger a memory threshold as though the known subtotal were complete. After a long sleep,
the loop schedules one new sample instead of replaying every missed interval. A reversed monotonic
timestamp becomes explicit clock-discontinuity evidence.

## Bounded operation

In-memory history uses a fixed-capacity ring. Capacity must be between 1 and 4,096 samples; inserting
a new sample evicts the oldest. A 50 ms runtime can therefore retain a useful recent window without
memory growing with run duration.

The journal writes the first 4,096 samples as crash-survivable progress. When a longer run reaches a
terminal outcome, it appends a synchronized history-reset marker followed by the ring's latest 4,096
windows; final report replay therefore exposes the recent window. A journal interrupted before its
terminal outcome remains recovery evidence, not a complete final report.

On macOS, the fast path enumerates the owned process group, previously tracked identities, and public
parent-child edges. It calls the more expensive rusage observer only for relevant candidates. This
preserves best-effort escape detection without inspecting the footprint of every process on the
machine. Sampling continues after TERM or KILL is requested until process exit is actually observed.

## Interpretation limits

The aggregate is a multi-call estimate, not an instantaneous machine-wide truth. Shared pages may be
counted in more than one process. A released Metal object can remain charged by the OS for more than
ten seconds, so release does not promise immediate headroom. The Linux adapter exercises topology,
clock, and history behavior in CI but reports footprint capability as unsupported.
