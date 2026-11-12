# mlx-guard 0.5.1

`mlx-guard` 0.5.1 is a patch release of the alpha external runtime safety supervisor for MLX
workloads on Apple Silicon. It launches one command in an owned process group, samples the group's
OS-accounted footprint, and applies an explicit memory limit and optional wall-time limit from
outside the process. This release fixes one bug in how a sample that finds no processes is read at
the end of a run: it printed a false "warning cleared" line, and it changed the count of missed
readings.

## What is fixed

### A false "warning cleared" line

In 0.5.0, a `run` that crossed the warning threshold and then ended normally could print
`warning cleared: the OS-accounted footprint fell to 0 B` just before the exit line. Its report
also recorded a matching return to normal at 0 B. This happened when the command's root process had
exited but its exit status had not been collected yet. The last sample then counted no processes
at all, and the policy read that as a recovery.

Now the policy gets no reading from a sample with no processes in the group. The sample is still
recorded in the report at 0 B, and the warning state stays as it was. The false line and the false
return to normal never caused a stop or a signal.

The same change means an empty sample now neither counts as a missed reading nor resets the count
of missed readings. This matters only when readings were already being missed as the run ended, and
then the exit code can differ from 0.5.0 in either direction. In 0.5.0 a slow or late empty sample
at the exit could be the third missed reading in a row and end a finished run with exit 70; 0.5.1
no longer does that. Also in 0.5.0 an empty sample reset the count, so misses on either side of it
could not add up; in 0.5.1 they can, and the missed-reading limit applies as it does at any other
time in the run. A sample that is incomplete because the process listing failed still counts as a
missed reading, as before.

The report's final footprint can still read 0 B after such a run. This release does not change that.

## What did not change

Apart from the fix above, a supervised `run` or `observe` behaves as it did in 0.5.0. Exit codes
and their meanings, report schema v1, the command-line flags, and the defaults are unchanged.

## Evidence

There are no new reference measurements. The measurement code and the policy state machine are
unchanged. The fix is one check in the run and observe loops, made before the policy sees the
sample, so the reference numbers do not move.

A real-process test keeps an exited root unreaped while a child stays alive. While the child lives,
every sample finds it as a member. Once the child is gone, the samples are empty and complete. A
unit test pins that the skipped sample is not counted as a missed reading.

The 0.5.0 reference bundle under `evidence/v0.5.0/m1-max-32gb/` still applies. Its numbers
describe one host and the commits and build profiles recorded with them; they are not a promise for
other Macs. The examples were captured from the 0.5.1 build. The release workflow runs on GitHub's
`macos-15` arm64 runner. Intel Macs and other operating systems are not release targets.

## Upgrading

Most users will see no change. A run that crossed the warning threshold and ended normally no
longer prints a false "warning cleared" line. If readings were already being missed as a run ended,
its exit code can differ from 0.5.0; see What is fixed.

The limits from earlier releases still hold. Sampling is periodic rather than atomic, a descendant
can leave the process group, same-user hostile workloads are outside the threat model, and Metal
allocations may remain charged after termination. The IOGPU driver bug that panics macOS 26.4 and
later under Metal workloads can fire with the footprint well inside any limit, and no external
supervisor can reach it. Warnings reduce risk by giving you time to react; they do not stop a
failure. Use `observe` across repeated representative runs before choosing a destructive limit.

Start with the [tutorial](https://github.com/IonDen/mlx-guard/blob/main/TUTORIAL.md) or the
[examples](https://github.com/IonDen/mlx-guard/blob/main/docs/EXAMPLES.md), then read the
[changelog](https://github.com/IonDen/mlx-guard/blob/main/CHANGELOG.md) and the
[stability table](https://github.com/IonDen/mlx-guard/blob/main/docs/STABILITY.md).
