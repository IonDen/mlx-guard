# Measuring a macOS process tree honestly

*A methods companion from `mlx-guard`: what it takes to turn "how much memory is that command
using?" into a number a supervisor may act on*

> 📄 [Read on the website](https://ineshin.space/papers/measuring-a-macos-process-tree-honestly/) — same paper, formatted for reading.

"How much memory is that command using?" sounds like a question with a syscall-shaped answer.
It is not, once the answer is allowed to kill something. A companion memo,
[Why the memory limit must live outside the process](https://github.com/IonDen/mlx-guard/blob/main/docs/papers/why-the-memory-limit-must-live-outside-the-process.md),
argues that the last line of defense against a runaway MLX workload is an external supervisor
watching the number the operating system charges. This memo covers the half that memo takes for
granted: how that number is actually produced on macOS, where the obvious way of producing it
lies, and what guard closes each hole. The subject is the failure catalogue, not a tour of
[`mlx-guard`](https://github.com/IonDen/mlx-guard).

The memo is documentary. Every measurement comes from the committed evidence bundle in the
repository at v0.1.0 — a bounded calibration run on a 10-core Apple M1 Max with 32 GB of unified
memory, macOS 26.6.1
([reference-host bundle](https://github.com/IonDen/mlx-guard/blob/v0.1.0/evidence/v0.1.0/m1-max-32gb/README.md),
which records the exact commit and tool versions). Nothing was rerun for this write-up. Links
into `mlx-guard` contracts and evidence are pinned to the public v0.1.0 tag; companion memos are
linked at their public homes. The measurements describe this host and commit. They are
source-reported, not universal bounds.

## 1. The signal: what the OS charges, when it can be read at all

macOS has no cgroups. It does have per-process memory limits in the kernel (memorystatus, the
machinery behind jetsam) and a shipped operator wrapper for them:
[`taskpolicy -m <MiB>`](https://github.com/apple-oss-distributions/system_cmds/blob/408bba7453608006b89772db185defbac8fe2fd0/taskpolicy/taskpolicy.8)
launches a program under a fatal per-process limit, which Apple's source sets through a spawn
attribute. The callable interface underneath is private, and what `taskpolicy` supplies is a
ceiling on one process; what it does not supply is a budget over a whole tree, typed
observation outcomes, or a report of what happened. What that kernel machinery enforces on,
though, is a ledger any supervisor can read:
[Apple's memorystatus documentation](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/doc/vm/memorystatus_kills.md)
describes a per-process limit as a limit on the `phys_footprint` ledger field. The signal to
act on is that same accounting: the `ri_phys_footprint` field of
`proc_pid_rusage(RUSAGE_INFO_V0)`, which bills the process for its dirty, compressed, and
driver-mapped memory. The same structure carries a resident-set size beside that field, and the
two are not interchangeable. The companion memo covers why a framework's own counter is not
this number either.

The point here is narrower: even the OS-accounted signal cannot be assumed to be available.
`mlx-guard` probes the capability at startup instead of assuming it, and the
[sampling contract](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/SAMPLING.md) makes
every way the read can fail a typed outcome: unsupported host, denied process, vanished process,
stale identity, malformed result, failed enumeration. None of them becomes a numeric zero.

The zero is the trap. A supervisor compares a number against a limit; a failure encoded as `0`
reads as "this process uses no memory," which silently disables the limit for exactly the
process that may be running away. A typed failure, by contrast, can be handled as what it is:
missing evidence. The same discipline holds across platforms. The Linux adapter exercises the
topology, clock, and history rules in CI but reports the footprint capability as unsupported
rather than inventing a compatible-looking number.

## 2. A PID is not an identity

The number must be attributed to a process, and the obvious attribution, the PID, is not
stable. PIDs are recycled. A supervisor that samples PID 4242, watches it cross the limit, and
signals PID 4242 two hundred milliseconds later may kill a process that did not exist when the
sample was taken. The race is old and documented; Linux eventually answered it with
[pidfd-based signalling](https://lwn.net/Articles/773459/), and binding identity to the pair
of PID and start time is established supervisor practice. The part worth writing down is the
gap that remains.

The [identity contract](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/IDENTITY_AND_CONTAINMENT.md)
therefore binds every observed process to the pair `(pid, process_start_abstime)`. A changed
start token is a typed `stale` outcome, distinct from disappearance, permission denial,
malformed metadata, unsupported access, and enumeration failure. The Darwin adapter reads the
start and exit tokens together with the footprint from `proc_pid_rusage(RUSAGE_INFO_V0)`, reads
parent and process-group metadata with `proc_pidinfo(PROC_PIDTBSDINFO)`, then samples the start
token a second time to detect reuse inside the observation window itself. One edge case shows
how far the requirement goes: an exited root that has not been reaped can still expose rusage
data after its BSD metadata is gone. The adapter records the validated identity and the exit,
and refuses to invent parent or group values for a process that no longer has them.

The token closes less than the contract's wording suggests, because the v0.1.0 runtime has
three signalling paths. The identity-aware direct path inspects a process and refuses to signal
a `stale` identity. TERM and KILL are broadcast to the owned process group as a whole, an
authority whose reuse hazard lives in the group ID rather than the PID. And the
cooperative-checkpoint request takes a third route: its endpoint is recorded as a PID and a
group, delivery rechecks only that the PID still belongs to the group, and the signal goes to
the numeric PID
([source at v0.1.0](https://github.com/IonDen/mlx-guard/blob/v0.1.0/crates/mlx-guard-core/src/process_control.rs)).
That recheck narrows the reuse window, as its own comment says; it does not bind the start
token, so a PID reused inside the same group would pass it. Even the identity-aware path
inspects and then signals as two separate steps, and macOS has no `pidfd` equivalent to make
them one. Binding identity narrows the race to that final window. Nothing available to an
unentitled process on macOS closes it, and a memo about measurement should say so about its
own tool.

## 3. Discovery is a race you can only record

A command tree is not enumerated atomically. Children fork, exit, reparent, and call `setsid`
while the supervisor walks the process table, so any snapshot is already slightly wrong by the
time it completes. Darwin gives an ordinary, unentitled command-line tool no better primitive.
A [2019 write-up by Julio Merino](https://jmmv.dev/2019/11/wait-for-process-group-darwin.html)
walks the same wall: for such a tool, scanning the process table is the only discovery
mechanism available, and a descendant that starts a new process group escapes it. Apple's
[Endpoint Security](https://developer.apple.com/documentation/endpointsecurity) framework does
deliver fork events, but behind a restricted entitlement and a deployment model built for
security agents rather than for a supervisor a user installs from a package index.

The design response is to make the disagreements between snapshots themselves evidence. Each
snapshot considers the bound root, the current owned
process group, current parent chains, and every identity tracked in earlier frames, which lets
the tracker record: a child that disappeared between enumerations, a PID whose start token
changed, a known child whose parent changed, a same-group grandchild observed after its
intermediate parent exited, and a descendant that left the owned group with `setsid`.

An escape, once observed, remains evidence for the whole run. Later cleanup of the owned group
cannot convert that run into a claim that an arbitrary daemon was contained. The limit is
stated with the same bluntness: discovery can still miss a process that forks, escapes, and
exits entirely between snapshots. Cleanup follows the same rule in reverse: an owned group is
declared empty only when no live validated member is observed *and* the operating system
reports no such group, and any observed escape keeps the overall cleanup result incomplete even
when the group itself is gone.

## 4. The incomplete-aggregate rule

The number a policy acts on is an aggregate over the owned process group, and the aggregate
inherits every failure mode above. The rule that keeps it honest: only live observations with
validated identities and the owned process group enter the sum. If any owned member lacks
footprint data, the aggregate is `incomplete`: the known bytes and the missing identities are
retained for diagnostics, but the subtotal can never trigger a memory threshold as though it
were the whole. Arithmetic overflow is its own typed result rather than a wrapped number.

`Complete` has a precise and narrow meaning here: complete for the currently owned group. A
descendant that has escaped the group is excluded from the sum, and its escape does not make
the aggregate partial. Policy can receive a numeric, `complete` aggregate while a known
workload process sits outside both measurement and enforcement. The escape lives in the run's
evidence and blocks a clean cleanup claim; it does not lower or suppress the number. An
operator reading `complete` as complete coverage of everything the command started would be
reading more than the word promises.

The sampling window adds its own validity conditions. Policy receives a numeric aggregate only
when the aggregate is complete and did not overflow, the monotonic clock did not move backwards
and the window has positive duration, the window stays within the configured maximum, and
processing happens within the configured freshness limit. After a long sleep the loop schedules
one new sample instead of replaying every missed interval, and a reversed monotonic timestamp
becomes explicit clock-discontinuity evidence rather than a negative-duration window.

![Decision flow for one sampling window: the supervisor enumerates the owned process group, tracked identities, and parent-child edges, then inspects each relevant PID with proc_pid_rusage, proc_pidinfo, and a second proc_pid_rusage start-token check; each observation is classified as an owned member whose footprint enters the sum, an escape recorded as whole-run evidence and excluded from the sum without making the aggregate partial, a confirmed disappearance recorded as an event with the sum unaffected, or a typed failure (stale, denied, malformed, unsupported, enumeration) that makes the aggregate incomplete while retaining the PID and failure kind; the aggregate over owned members then passes a gate requiring completeness, no overflow, a monotonic clock with positive window duration, the maximum window, and the freshness limit, and only if every condition holds does a numeric aggregate reach policy for a threshold comparison, otherwise the window is diagnostics only with known bytes and missing identities kept and no threshold decision](https://raw.githubusercontent.com/IonDen/mlx-guard/main/docs/papers/diagrams/sampling-window-decision.svg)

*Figure 1. One sampling window at v0.1.0: how each observation is classified, and the gate a
numeric aggregate must pass before policy may compare it against a limit. The figure shows the
control structure from the sampling and identity contracts; it reports no measurement. An
escape is drawn as excluded rather than as partial because that is what the tracker does, which
is the point of this section. The editable
[PlantUML source](https://github.com/IonDen/mlx-guard/blob/main/docs/papers/diagrams/sampling-window-decision.puml)
is published with the SVG.*

Even a complete aggregate carries a stated interpretation limit: summing per-process physical
footprint can count pages shared between related processes more than once, the same overcount
that proportional set size (PSS) exists to correct on Linux. The aggregate is a repeatable
intervention input for the observed tree. It is neither a unique-page total nor a claim about
all machine memory. These caveats are not hedging around the measurement; they are the
measurement's specification.

## 5. What honest measurement costs, on the record

The committed bundle answers the practical question: does all this typing and revalidating
produce a signal that is accurate enough to enforce with and cheap enough to run at 50 ms?
The calibration table, reproduced from the
[bundle](https://github.com/IonDen/mlx-guard/blob/v0.1.0/evidence/v0.1.0/m1-max-32gb/README.md):

| Measure | Target | Observed | Result |
|---|---:|---:|---:|
| Anonymous 64 MiB maximum error | 1% or less | 49,224 B (0.073%) | Pass |
| Live anonymous-shared 64 MiB maximum error | 1% or less | 16,432 B (0.024%) | Pass |
| Metal 64 MiB maximum visible-delta error | 10% or less | 1,294,408 B (1.929%) | Pass |
| `proc_pid_rusage` call p95 | 1 ms or less | 1.250 µs | Pass |
| 16-member sample-window p95 | 10 ms or less | 0.183 ms | Pass |
| 50 ms sampler CPU, 16 members | 2% of one core or less | 1.6868% | Pass |
| Supervisor maximum RSS | 20 MiB or less | 2.98 MiB | Pass |
| 30-minute footprint growth | 2 MiB or less | 80 KiB | Pass |
| Decision to first signal p95 | 10 ms or less | 8.209 µs | Pass |
| External TERM to final report p95 | 100 ms or less | 60.811 ms | Pass |
| External INT to final report p95 | 100 ms or less | 56.186 ms | Pass |
| 128 MiB/s ramp overshoot p95 | 16 MiB or less | 13,386,112 B (12.77 MiB) | Pass |

One label in that table needs correcting. The 1.250 µs row is labelled a
single `proc_pid_rusage` call, in the bundle and therefore here, but the
[calibration harness](https://github.com/IonDen/mlx-guard/blob/v0.1.0/crates/mlx-guard-test-support/tests/reference_calibration.rs)
times the full per-PID inspection: two `proc_pid_rusage` reads with a `proc_pidinfo` call between
them, the second read being the reuse check from section 2. The number is the cost of one
validated identity-plus-footprint observation, roughly three system calls, not one.

Three readings matter. Accuracy: the anonymous-memory error is under a tenth of a percent, and
even the Metal visible delta (the hard case, allocated outside the CPU allocator) stays under
two percent against a ten-percent target. Cost: a full per-PID inspection lands at 1.25 µs p95,
a 16-member window at 0.183 ms, and the whole 50 ms sampling loop at under 2% of one core, with
the supervisor itself at 2.98 MiB RSS and 80 KiB of growth over a 30-minute run that retained
exactly its bounded 256-sample window. And latency, which is the price of sampling: at a
128 MiB/s allocation ramp the p95 overshoot past the limit is 12.77 MiB, because an allocation
can always land between two samples. The companion memo names this as something external
supervision cannot promise away; the table above is where the number lives.

The cost reading carries a scope limit of its own. That endurance result belongs to its
scenario. At v0.1.0 the
[tracker](https://github.com/IonDen/mlx-guard/blob/v0.1.0/crates/mlx-guard-core/src/identity.rs)
keeps every distinct escaped identity for the life of the run and copies the full set into each
retained sample, so a workload producing a stream of distinct escapes would grow every sample
with it: the fixed ring bounds the sample count, not the sample size. The committed number
shows the supervisor staying small under the recorded workload. It does not certify bounded
growth under escape churn, and a stronger claim needs a release whose contract and evidence
bundle both contain the cap.

The fixtures behind these rows were deliberately bounded: 128 MiB aggregate and 10 seconds for
the synthetic workloads, a fixed 64 MiB buffer under a 5-second watchdog for the Metal
calibration, no real MLX allocation requested. From a clean checkout of the recorded commit,
the run reproduces with:

```bash
./scripts/calibrate-reference-host.sh /private/tmp/mlx-guard-v0.1.0-evidence
```

The [script](https://github.com/IonDen/mlx-guard/blob/v0.1.0/scripts/calibrate-reference-host.sh)
refuses a dirty worktree, an existing output directory, and any host that is not an M1 Max with
32 GB. That is the full extent of what it enforces: the recorded commit is the caller's
responsibility, and the macOS build and tool versions are written into the bundle's provenance
rather than checked against it.

## 6. The negative result: pages that do not come back

Two observations in the bundle support no comfortable conclusion. Reporting them correctly, and
naming what the bundle never observed at all, is the part of measurement honesty that editing
pressure works hardest against.

First, the released Metal buffer. In each of the ten Metal calibration runs the harness took
one complete footprint sample immediately after the fixture reported the release, then told the
fixture to exit. Every one of those samples still showed the allocation charged, with a maximum
residual of 68,403,272 bytes. The five-second watchdog bounded the fixture's lifetime, not an
observation window; there was no watching beyond that single sample. Second, the signalled
ramps: none of the 20 ramp runs produced a lower numeric footprint sample between the
intervention signal and the group's finalization, 54 to 61 ms later. Third, and by omission:
neither experiment measured what happens to the pages after the process is gone. Sampling a
process tree cannot observe reclamation after exit, and nothing in the bundle claims to.

The first two observations are right-censored: each ended while the condition still held. The
supported statements are narrow. One immediate post-release sample still showed the charge; no
pre-exit sample showed a decrease. The
[bundle](https://github.com/IonDen/mlx-guard/blob/v0.1.0/evidence/v0.1.0/m1-max-32gb/README.md)
draws exactly that line: no prompt-reclamation claim is supported. The unsupported statements
are one careless prose pass away: "Metal never reclaims released buffers," or "killing a worker
does not return its pages." The
[sampling contract](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/SAMPLING.md) carries
its own, separate interpretation limit: a released Metal object can remain charged by the OS
for more than ten seconds, so release does not promise immediate headroom. That figure concerns
a release inside a live process, is the contract's statement rather than a measurement from
these runs, and says nothing about reclamation after a kill. For a supervisor, the supported
operational statement is the modest one: these bounded observations give no basis for assuming
immediate headroom after a release or after a signal, so a limit chosen with zero headroom
rests on an assumption nothing here supports. A sibling memo, [How to measure what quantization actually costs](https://github.com/IonDen/mlx-quant-fidelity/blob/main/docs/papers/how-to-measure-what-quantization-actually-costs.md),
makes the same argument for a different instrument: the boundary of what was observed is part
of the result, and a report that omits it is wrong even when every number in it is right.

## The shape of an honest number

The same restraint shapes how the tool hands numbers back to an operator. The
[observe mode](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/OBSERVE_AND_CALIBRATION.md)
reads from the same sampler but never enforces, and its calibration artifact records
`observation_only: true` and `safety_certified: false` alongside the observed peak. The
measurement can reduce the guesswork in choosing a limit, but the choice, and the headroom for
what sampling cannot see, stay with the operator.

A number a supervisor may act on turns out to be a compound object: an OS-accounted signal
whose availability was probed, attributed to an identity that was revalidated, aggregated only
over members that were actually observed, inside a window whose clock behaved, with every
deviation recorded as typed evidence instead of a plausible-looking integer. Everything short
of that is diagnostics. On this host, at v0.1.0, that standard cost less than two percent of
one core.

---
*Denis Ineshin · 2026-08-23 · [ineshin.space](https://ineshin.space)*
