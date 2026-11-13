# Why the memory limit must live outside the process

*A design memo from `mlx-guard`: what an in-process memory guard cannot promise on Apple
unified memory, and the external supervisor built for the outer ring*

> 📄 [Read on the website](https://ineshin.space/papers/why-the-memory-limit-must-live-outside-the-process/) — same paper, formatted for reading.

## Update, 2026-11-13: what changed in mlx-guard 0.2 to 0.5

The conclusion holds at `mlx-guard` v0.5.0 (published 2026-11-12): the limit must live outside the
process. The body describes v0.1.0; later versions are not covered here.

Three behaviors changed in 0.2.0
([changelog](https://github.com/IonDen/mlx-guard/blob/v0.5.0/CHANGELOG.md)). The
cooperative-checkpoint acknowledgement timeout rose from 100 ms to 1 s. Exit 70 still means the
supervisor failed; a child that exits before the first identity inspection now keeps its own
status. The checkpoint signal now revalidates pid and start token just before delivery; TERM
and KILL still go to the numeric process group
([process control](https://github.com/IonDen/mlx-guard/blob/v0.5.0/docs/PROCESS_CONTROL.md)).

On the same M1 Max, the Metal 64 MiB visible-delta error was 1.929% at v0.1.0, 4.028% at 0.3.1
and 3.272% at 0.5.0
([bundle](https://github.com/IonDen/mlx-guard/blob/v0.5.0/evidence/v0.5.0/m1-max-32gb/README.md)),
against a 10% bound. The 50 ms sampler read 2.58% to 2.89% of one core in six 30-minute debug
runs on macOS 27.2. The four earliest failed the 2% bound then in force; one was a 0.3.1 build, so
the rise is not in the 0.5.0 code alone. The cause is not identified; the figure now has no
target. The bundle's other rows differ by amounts that are small against their targets.

The README names one failure the body does not: the IOGPU driver bug that panics macOS 26.4 and
later under Metal workloads (unfixed as of late August 2026, per the README). It can fire with the
footprint well inside any limit, and no external supervisor can reach it
([limits](https://github.com/IonDen/mlx-guard/blob/v0.5.0/README.md#limits-and-safety-boundary)).

---

An MLX workload that runs away with memory does not fail politely. Unified memory means the
process, the window server, and the kernel's GPU driver are all drawing from the same pool, and
a run that reaches the bottom of that pool can end in a paging storm, a frozen machine, or a
kernel panic rather than an allocation error the workload could catch. This memo argues one
design claim from that starting point: the last line of defense against a runaway workload cannot live inside the workload's own
process. It has to be a separate supervisor that watches the number the operating system charges,
owns the process group, and survives to write down what happened.
[`mlx-guard`](https://github.com/IonDen/mlx-guard) is the tool built around that claim; this memo
is the design argument, including the parts of the problem the design deliberately does not claim
to solve.

The memo is documentary. Every `mlx-guard` measurement in the body comes from a committed, linkable
evidence bundle in the repository at v0.1.0 — a bounded calibration run on a 10-core Apple M1 Max
with 32 GB of unified memory, macOS 26.6.1
([reference-host bundle](https://github.com/IonDen/mlx-guard/blob/v0.1.0/evidence/v0.1.0/m1-max-32gb/README.md)),
and a bounded consumer-integration run on the same recorded configuration with MLX 0.32.0 and
mlx-lm 0.31.3
([integration bundle](https://github.com/IonDen/mlx-guard/blob/v0.1.0/evidence/v0.1.0/mlx-train-perf/README.md)).
Nothing was rerun for this write-up. The incident figures in section 1 are the upstream
reporters' own, not measurements taken here. In the body, links into `mlx-guard` source,
contracts, and evidence are pinned to the public v0.1.0 tag, except the supervisor's SIGKILL limit
in section 4, which is linked at v0.5.0, as are the update's sources. Figure 1 and its PlantUML
source are read from `main`, and companion memos and upstream issues are linked at their public
homes.

## 1. Two public incidents, one failure shape

Two independent reports against the MLX stack bound the problem better than any synthetic
demonstration could.

In [MLX issue #3896](https://github.com/ml-explore/mlx/issues/3896), a streaming saliency
workload over a 198-billion-parameter mixture-of-experts model tracked its memory with the
framework's own counter. `mx.get_peak_memory()` reported roughly **46 GB** while the macOS
`footprint` tool showed roughly **109 GB** of IOAccelerator (GPU) allocations and a
phys_footprint — the operating system's own accounting of what the process holds — of roughly
**110 GB**: about a **2.4×** undercount. Trusting that counter as the memory-pressure signal let
the process exceed the machine's safe working set twice — once ending in a Metal command-buffer
GPU watchdog timeout, once in a full hard reboot.

That issue is closed, and the explanation is not a mystery. `get_peak_memory()` tracks only the
allocator's active bytes, so buffers MLX retains in its cache pool — still resident, still
GPU-dirty — never enter the number. An MLX maintainer closed the report by noting that the
counter is meant for measuring a single model, where the cache hit rate is near 100%, and that
long-running or parallel serving should read `get_active_memory() + get_cache_memory()` instead;
a contributor's churn test found that pair tracking the OS figure to within 0.14 GB on a run
where peak memory reported 1.00 GB against a 60.19 GB footprint. The fix is real, and it is a fix
to the *reading*: a corrected counter still describes one process's allocator, and it still runs
inside the process it is meant to police. The operational lesson survives it: any decision keyed
to the counter this workload was actually reading would have been made more than 60 GB below the
number the operating system was charging the process.

In [mlx-lm issue #883](https://github.com/ml-explore/mlx-lm/issues/883), an `mlx_lm.server`
instance serving a 30B model grew its KV cache without bound across a long agentic session. At
crash time the process footprint was **83.23 GB** on a 96 GB machine, and **80.14 GB** of memory
was wired — pinned so the OS cannot page it out. The machine kernel-panicked in `IOGPUMemory.cpp:550`
(`completeMemory() prepare count underflow`) and force-rebooted.

Neither incident is a controlled experiment for any guard tool, and this memo does not claim a
supervisor would have saved either machine. The incidents are evidence about *signals* and
*boundaries*: the first shows the framework's counter and the OS's accounting can diverge by tens
of gigabytes on exactly the workloads that need a limit most; the second shows what fills the gap
when no boundary exists outside the process that is failing.

## 2. The cap inside the process was already tried

The predecessor paper,
[When an MLX Memory Cap Is Not a Safety Boundary](https://github.com/IonDen/mlx-train-perf/blob/main/docs/papers/when-an-mlx-memory-cap-is-not-a-safety-boundary.md),
reports a training-benchmark machine that kernel-panicked with MLX's memory caps installed, and
the in-process watchdog the project added afterwards, which that paper calls best-effort. It
concludes that a cap or a watchdog inside the failing process is not a safety boundary. An abort
path that must be more reliable than the failing workload cannot be a thread inside it; it has to
be a different process.

## 3. The counter you watch is not the bill you pay

An in-process guard almost always watches the framework's counters, because those are the numbers
the process can see cheaply. Which counter it watches matters — #3896 is the cautionary case, and
reading `get_active_memory() + get_cache_memory()` closes most of that particular gap. But even
a correctly read allocator total is the allocator's view of one process. The operating system
keeps its own charge per process, phys_footprint, and that charge is not limited to what the
framework allocated: the calibration below sees both a plain anonymous allocation and a Metal
buffer in it. It can also be read for processes the framework does not manage at all: a
dataloader, a tokenizer service, a spawned conversion step.

The
[mlx-train-perf integration contract](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/integrations/MLX_TRAIN_PERF.md)
says the same of that project's own in-process fallback: it samples
`mx.get_active_memory()` only, MLX cache memory is a separate retained pool the ceiling can miss,
and the external supervisor's OS-accounted footprint is what covers the broader process charge.

`mlx-guard` therefore reads memory the way the OS does:
[`proc_pid_rusage(RUSAGE_INFO_V0)`](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/SAMPLING.md)
phys_footprint, per process, aggregated over the process group it owns. In v0.1 the framework's
counters are not read at all, and system-wide signals — pressure, swap, compressor, wired totals —
are recorded as advisory context rather than enforcement inputs, because none of them is
attributable to the supervised command. The threshold is compared against the phys_footprint of
each owned-group member, summed; that sum can double count pages shared inside the tree, and the
contracts present it as a repeatable intervention input, not a unique-page total.

The committed v0.1.0 calibration bounds that choice on the reference host under macOS 26.6.1:
the largest observed error was **0.073%** on a 64 MiB anonymous allocation and, across ten runs,
**1.929%** on a 64 MiB Metal buffer's visible footprint delta. Those are maxima from a bounded run
on one machine, not statistical or cross-host bounds. The signal is not perfect — section 8 is
about its limits — but it is the right quantity, measured against known allocations.

## 4. The guard shares the fate of the guarded

Put the enforcement loop inside the process and it inherits the process's worst moments. It runs
on a thread of the same address space, scheduled by the same starved machine, killed by the same
SIGKILL. Concretely, an in-process guard:

- **cannot outlive the process to say what happened.** After the OOM kill, the crash, or the
  panic, there is no thread left to write the report. The forensic record — what grew, how fast,
  what the guard decided, whether termination was attempted — dies with the process, which is
  exactly when it was needed.
- **is scheduled by the machine it is supposed to protect.** During a paging storm the watchdog
  is one more thread of the faulting process, with no priority over the workload it watches. No
  published run shows a watchdog starved out of its abort, but nothing in the design prevents it.
- **guards one process.** A benchmark runner, a dataloader pool, or a spawned conversion step is
  a process *tree*; a limit installed inside one member sees none of the others.
- **dies before it can clean up.** If the guarded process is the parent of the tree, its death
  orphans the children it was supposed to bound.

The external supervisor inverts the first, third, and fourth item, and its own death is the
remaining exception: if it is killed with SIGKILL, the command keeps running and no final report
is written ([README at v0.5.0](https://github.com/IonDen/mlx-guard/blob/v0.5.0/README.md#limits-and-safety-boundary)).
The second item it only narrows: a small separate process shares neither the workload's address
space nor its locks, though no user-space tool survives a machine-wide stall. Section 8 returns to
that limit.

![Boundary diagram: the supervisor runs as a native parent outside the workload's failure domain, sampling OS-accounted phys_footprint per validated member, running the policy state machine, and writing the journal and final report; inside the failure domain, which is starved, killed, or panicking together, sit the owned process group with the supervised root command and its descendants, plus an in-process guard that reads framework counters from inside and ends when the process ends; the supervisor sends sampling and TERM/KILL to the group and a checkpoint request only to the negotiated endpoint, while a descendant that calls setsid leaves the owned group and is recorded as evidence rather than contained](https://raw.githubusercontent.com/IonDen/mlx-guard/main/docs/papers/diagrams/supervision-boundary.svg)

*Figure 1. Which components share a failure domain, and which do not. The figure shows the v0.1
control structure and the signal targets each path uses; it reports no measurement. The escape
path is drawn because the supervisor records it, not because the owned group contains it. The
editable
[PlantUML source](https://github.com/IonDen/mlx-guard/blob/main/docs/papers/diagrams/supervision-boundary.puml)
is published with the SVG.*
[The process-control contract](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/PROCESS_CONTROL.md)
launches the command as the leader of a new process group the supervisor owns; TERM and KILL
target the validated group, not one PID.
[The report contract](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/REPORTS.md) writes a
checksummed, synced journal *from the parent* as events happen, so a workload that dies leaves a
recoverable prefix on storage that survives and stays readable.

A supervisor whose own storage fails is a different case, and the contract promises no report
for it. Persistence stops, the complete-report claim is suppressed, and the run exits with the
partial-artifact code. That is what the committed storage-loss scenario records: no report file
at all. Where the supervisor and its storage do survive, the artifact arrives quickly. On the
reference host at v0.1.0, an external TERM to the supervisor still produced a final report with a
p95 of **60.8 ms**. The report is redacted before it is persisted: schema v1 has no fields for raw
argv, environment values, paths, prompts, or model identifiers — the executable basename and
argument count are the only command identity it keeps, alongside an optional correlation hash.
That removes the categories most likely to leak, but it does not make the file automatically safe
to publish. Read it before sharing; the
[support page](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/SUPPORT.md) asks for a
redacted report when filing an issue.

The v0.1.0 reference bundle also measures the "survives the worker" claim in its cheapest form:
across lifecycle scenarios including TERM-resistant workers, fast root exits, storage loss
mid-run, and a checkpoint timeout, the supervisor finalized a schema-valid report in every scenario where it
and its storage survived — and four safe workloads ran to completion with **zero** false
interventions.

## 5. macOS hands you a process group, not a cgroup

On Linux, this tool would be small: place the tree in a cgroup, set `memory.max`, and the kernel
enforces the boundary at charge time, no sampling loop required — for CPU-side memory, at least;
device-driver allocations are their own story there too. macOS has no equivalent for a tree. Its
kernel does keep per-process memory limits (memorystatus, the machinery behind jetsam), and
`taskpolicy -m` launches one program under such a limit, as the companion methods memo
[Measuring a macOS process tree honestly](https://github.com/IonDen/mlx-guard/blob/main/docs/papers/measuring-a-macos-process-tree-honestly.md)
shows. That is a fatal ceiling on one process, set at launch through a private interface, not a
budget over a whole tree.

What macOS does hand a supervisor is the POSIX toolbox: process groups, signals, `libproc`
sampling. Every piece is leaky on its own. A PID becomes reusable as soon as the exited process
is reaped, and a supervisor does not control reaping for processes it did not spawn. A
descendant can leave the group with one `setsid` call, starting a new session outside the owned
group. Enumeration of a process tree is non-atomic: the tree can change while you walk it.
macOS also has no pidfd-style stable process handle; the companion memo covers the cost.

The design consequence is not "give up"; it is "hold the boundary you can hold, and record the
rest as evidence."
[The identity contract](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/IDENTITY_AND_CONTAINMENT.md)
binds every observed process to `(pid, process_start_abstime)`, so a member whose start token
changed is recorded as stale rather than counted or trusted.

The signal paths deserve a precise statement, because their guarantees differ. In the v0.1.0
runtime, TERM and KILL went to the retained, launch-validated numeric process group; a checkpoint
request was rechecked for group membership immediately before delivery
([source at v0.1.0](https://github.com/IonDen/mlx-guard/blob/v0.1.0/crates/mlx-guard-core/src/process_control.rs)).
Neither re-proved a start token at the instant of the system call, so a reused PID inside a live
group, or a reused group number after the owned group emptied, was a real if narrow window on a
platform with no stable process handle.

Descendants are discovered on every sample. One observed outside the owned group is recorded as
an escape, and that flag stands for the whole run, so a later clean group-empty check can never
quietly upgrade the run into a containment claim. Reparenting and identity changes are recorded
too, as containment events rather than escapes. A descendant that detaches before it is ever
observed can be missed entirely.

The aggregate follows the same honesty rule as the sampler: if any owned member could not be
measured, the total is `incomplete`, kept for diagnostics, and never compared against the
threshold as if it were complete.

This is the structural difference between an enforced boundary and an *evidenced* one. macOS
permits the second. A tool that claims the first for a process tree on this platform is
describing a mechanism the platform does not have.

## 6. The design that follows

**A native parent, not a library.** The supervisor is a compiled executable that imports nothing
from the workload — no Python, no MLX, no framework version coupling. The enforcement loop never
runs inside the supervised process. The Python package is a typed client that launches the
binary; it contains no enforcement.

**An explicit limit, never an invented one.** No number is universally safe across machines and
workloads, so
[observe mode](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/OBSERVE_AND_CALIBRATION.md)
measures a workload without ever intervening, records the highest complete aggregate and growth
rate in a calibration artifact stamped `safety_certified: false`, and the operator chooses the
enforcement limit from observed peaks plus deliberate headroom. The tool refuses to guess.

**A deterministic, inspectable policy.**
[The policy contract](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/POLICY.md) is a pure
state machine: sampled events in, actions out. Every transition is recorded with its timestamp,
its state pair, and the aggregate observed at the time; the thresholds it was judged against
appear once per report. The v0.1 bands all derive from the single limit the operator configured,
with warning a band-step below it, emergency a step above, and two consecutive breaches required
before anything happens. Escalation then runs warn → optional cooperative checkpoint (**100 ms**
acknowledgement timeout at v0.1.0) → TERM (**1 s** grace) → KILL, and a sample in the emergency
band goes straight to KILL. Samples the supervisor cannot trust never count as zero: three
consecutive unusable ones, whether missing, stale, clock-reversed, or over-wide, make enforcement fail
*closed*, sending TERM, recording a typed supervisor error, and escalating to KILL if the process
is still alive. The timelines are executable, committed as
[golden tests](https://github.com/IonDen/mlx-guard/blob/v0.1.0/crates/mlx-guard-core/tests/policy_timelines.rs).

**Preserve work before destroying it.** A worker that opts in to the
[checkpoint protocol](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/CHECKPOINT_PROTOCOL.md)
gets asked to save before TERM arrives. The committed integration run shows the cooperative rungs
end to end: a 1,000-repetition MLX loss-layer benchmark under a deliberately tiny **500 ms**
wall limit — its **512 MiB** footprint limit was never approached; the peak aggregate was about
**46 MiB** — was asked to checkpoint at **505 ms**, finished repetition **336**, wrote and
synced a partial result, was acknowledged at **527 ms**, and was terminated with a final report
at **553 ms**, with 336 completed repetitions in the checkpointed artifact. No control run
establishes what an unsupervised abort would have salvaged (the consumer's own watchdog writes
an aborted result too), so the demonstrated claim is the preserved artifact, not a rescue from
zero. The protocol's evidence vocabulary stays honest: delivery alone records
`requested_unverified`, and even a matching acknowledgement records
`acknowledged_unverified_durability` — the worker's claim, not proof of durable bytes.

**A report that survives.** Section 4 covered the journal; the outcome side is typed exit codes
(the child's own code when nothing happened, **75** for a policy intervention, distinct codes for
supervisor and artifact failures), so scripts and CI can branch on what actually occurred. One
known exception was open at v0.1.0: a child that exited before the supervisor's first identity
inspection could surface the supervisor-failure code instead of its own status.

## 7. The supervisor must cost less than the problem it watches

An outer ring that competes with the workload for the resource it guards is part of the problem.
The v0.1.0 reference bundle prices the supervisor on the M1 Max host against a stated target for
every measure, and every measure passed: maximum supervisor RSS **2.98 MiB**; sampler CPU at a
50 ms interval over a 16-member group **1.69%** of one core; a single `proc_pid_rusage` call
**1.25 µs** at p95; a 16-member sample window **0.183 ms** at p95; supervisor footprint growth
over 30 minutes **80 KiB**; decision-to-first-signal latency **8.2 µs** at p95. At that release
the default 50 ms sampling interval bought roughly twenty observations per second for about a
sixtieth of one core and three megabytes of memory on this host, cheap enough to leave in front
of a heavy run, which is the only place a circuit breaker helps. Other Apple Silicon generations
are uncalibrated, and the support contract keeps performance expectations host- and
workload-specific.

## 8. What an external supervisor still cannot promise

The external position removes the fate-sharing failure and fixes the accounting. It does not
make enforcement atomic, and the same evidence bundles that price the supervisor also measure
its gaps.

**Sampling has a window, and allocations do not wait for it.** Under a controlled synthetic ramp
of 128 MiB/s, the p95 threshold overshoot was **12.77 MiB**, with a worst observed **13.19 MiB**
across the twenty runs. That number scales with the ramp: an MLX workload can materialize
gigabytes in a single lazy-evaluation step, and a single allocation can cross the threshold
entirely between two samples. The policy records the maximum observed overshoot per run.

**Signalling the process does not promptly return the memory — as far as could be observed.**
In all **ten** Metal calibration runs, the 64 MiB Metal buffer remained charged to the process
immediately after release, with a maximum residual of **65.2 MiB** (68,403,272 bytes), and in
none of the twenty ramp runs did a numerically lower footprint sample appear after the
intervention signal. These observations are right-censored — the runs ended before reclamation was observed, which is not evidence that
reclamation never happens, and the bundle says exactly that. The design consequence: the
supervisor keeps sampling after TERM and KILL until exit is actually observed, and no part of
the report treats signal delivery as reclamation.

**The group is owned, not sealed.** A `setsid` descendant can leave; discovery can miss a
process that forks, escapes, and exits between snapshots. The report carries escape evidence
rather than a containment guarantee, and the
[threat model](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/THREAT_MODEL.md) scopes the
whole tool to one trusted, accidentally-failing command — not a malicious one.

**System-wide failure outranks everything.** A supervisor is a user-space process. Through a
kernel panic, driver failure, or machine-wide stall, it may not be scheduled, and its last
journal record may be the only artifact. That is the honest ceiling of *any* user-space answer
on this platform, external or not.

This is why the project's own vocabulary is "OS-accounted footprint," "sampled intervention
threshold," and "reduces risk" — and never "hard boundary." The value of the external position
is not that it makes the boundary hard. It is that the gaps above are *measured where they can
be observed and written down* by a process that outlives the workload, rather than vanishing
with it.

## 9. Layers, not a replacement

Nothing in this argument retires the inner guards. The wired limit still bounds non-pageable
residency — the memory class whose exhaustion is one documented panic path. The advisory
memory limit still shapes allocator behavior. The in-process watchdog still aborts with an honest artifact in the common case, still works when no supervisor
is installed, and inside a supervised worker it keeps running — the integration contract keeps
the consumer's watchdog active as the fallback, and the supervisor adds process-group ownership
and OS-accounted footprint around it. In-application MLX safety layers such as
[MetalGuard](https://github.com/Harperbot/metal-guard) occupy the same inner ring with much
richer framework awareness; the external supervisor is deliberately application-neutral and
framework-blind, which is what lets it wrap an unmodified command. The rings measure different
quantities and fail independently — which is the whole point of rings.

## 10. Lessons

**A limit that lives inside the process it limits is advice.** The predecessor paper showed the
framework caps did not stop that machine's panic; this memo's addition is that *any* in-process
mechanism, cap or watchdog, shares the process's scheduler, address space, and lifetime, and
therefore its fate.

**Watch the number the OS charges you.** Framework counters are scoped to what the allocator
tracks, and the counter a workload reaches for first may be scoped more narrowly still: the
public 2.4× divergence came from a peak-memory counter that omits the retained cache pool.
Reading the right pair narrows that gap, but phys_footprint is the bill, and summed over the owned
group it covers every member of that group, including memory the framework never allocated.

**The abort path must survive the thing it aborts.** A separate native parent can act while the
worker is wedged, clean up the whole group, and — the part an in-process guard can never do —
write the report *after* the worker is gone.

**On macOS, containment is evidence, not a primitive.** No cgroup, no pidfd: identity must be
revalidated, escapes must be recorded, and an incomplete measurement must refuse to pretend it
is complete. A tool that cannot inherit a boundary from the kernel must prove the one it holds.

**Name the gaps, then measure them.** Overshoot has a p95. Non-reclamation has a residual and a
censoring caveat. The false-intervention rate on safe workloads has a number (zero, on four
workloads).

## References and source notes

- `mlx-guard` at the v0.1.0 tag:
  [sampling](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/SAMPLING.md),
  [identity and containment](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/IDENTITY_AND_CONTAINMENT.md),
  [process control](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/PROCESS_CONTROL.md),
  [policy](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/POLICY.md),
  [intervention](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/INTERVENTION.md),
  [observe and calibration](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/OBSERVE_AND_CALIBRATION.md),
  [checkpoint protocol](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/CHECKPOINT_PROTOCOL.md),
  [reports and privacy](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/REPORTS.md),
  [threat model](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/THREAT_MODEL.md),
  [support](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/SUPPORT.md),
  [mlx-train-perf integration contract](https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/integrations/MLX_TRAIN_PERF.md),
  [process-control source](https://github.com/IonDen/mlx-guard/blob/v0.1.0/crates/mlx-guard-core/src/process_control.rs),
  [policy timeline goldens](https://github.com/IonDen/mlx-guard/blob/v0.1.0/crates/mlx-guard-core/tests/policy_timelines.rs).
- Committed evidence at v0.1.0:
  [M1 Max 32 GB reference bundle](https://github.com/IonDen/mlx-guard/blob/v0.1.0/evidence/v0.1.0/m1-max-32gb/README.md)
  (accuracy, latency, overhead, overshoot, endurance, lifecycle scenarios, safe-workload corpus);
  [mlx-train-perf integration bundle](https://github.com/IonDen/mlx-guard/blob/v0.1.0/evidence/v0.1.0/mlx-train-perf/README.md)
  (checkpoint timeline, failure paths).
- `mlx-guard` at the v0.5.0 tag, for the update and the SIGKILL limit in section 4:
  [README](https://github.com/IonDen/mlx-guard/blob/v0.5.0/README.md),
  [changelog](https://github.com/IonDen/mlx-guard/blob/v0.5.0/CHANGELOG.md),
  [process control](https://github.com/IonDen/mlx-guard/blob/v0.5.0/docs/PROCESS_CONTROL.md),
  [M1 Max 32 GB reference bundle](https://github.com/IonDen/mlx-guard/blob/v0.5.0/evidence/v0.5.0/m1-max-32gb/README.md)
  (macOS 27.2, with the 0.3.1 figures alongside).
- Public incident reports: [MLX #3896](https://github.com/ml-explore/mlx/issues/3896)
  (peak-memory counter vs phys_footprint divergence; two exceedances, one GPU watchdog timeout
  and one hard reboot; closed 2026-08-08 with upstream guidance to read active plus cache
  memory);
  [mlx-lm #883](https://github.com/ml-explore/mlx-lm/issues/883)
  (unbounded KV cache, 83.23 GB footprint, kernel panic).
- Companion memos:
  [When an MLX Memory Cap Is Not a Safety Boundary](https://github.com/IonDen/mlx-train-perf/blob/main/docs/papers/when-an-mlx-memory-cap-is-not-a-safety-boundary.md)
  (the predecessor, section 2);
  [Measuring a macOS process tree honestly](https://github.com/IonDen/mlx-guard/blob/main/docs/papers/measuring-a-macos-process-tree-honestly.md)
  (the methods memo, section 5).
- Adjacent tooling: [MetalGuard](https://github.com/Harperbot/metal-guard) (in-application MLX
  safety layer; complementary boundary).

---

*Prepared 2026-08-14. Last updated 2026-11-13. Denis Ineshin.*
