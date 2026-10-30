# Troubleshooting a run that stopped

Start here when a guarded run ended with a non-zero exit code. Find the code in the table, open
the section, and you will know what happened, why mlx-guard did it, what to do next, and which
report fields to read.

Since 0.4.0, mlx-guard prints the same explanation on stderr when a run ends with a non-zero exit.
The one exception is a command's own exit code that mlx-guard does not also use: that prints
nothing. The last two lines
start with `mlx-guard: what happened:` and `mlx-guard: what to do:`, and the second one ends with a
link to a section of this page. The wording of those lines may change between releases. The two
prefixes, where the lines appear, and the rule that they never contain your command, environment
values, paths, or command output are stable (see [STABILITY](STABILITY.md)). Scripts should read the
exit code or the [report](REPORTS.md), never these lines.

Below, each section quotes the sentences mlx-guard prints. Text in braces, such as `{limit}`, is
filled in from the run. A sentence that says "the error above" points at the one-line diagnostic
mlx-guard prints just before the two lines.

| Exit code | Sections |
|---|---|
| The command's own code (for example `1`) | Nothing is printed. See [a code mlx-guard also uses](#a-code-mlx-guard-also-uses) only if the code is 64, 70, 74, 75, 126, or 127 |
| `64` | [the command line](#64-the-command-line), [a launch setting](#64-a-launch-setting) |
| `70` | [before launch](#70-before-launch), [while starting the command](#70-while-starting-the-command), [footprint readings lost](#70-footprint-readings-lost), [a signal not delivered](#70-a-signal-not-delivered), [an internal failure](#70-an-internal-failure) |
| `74` | [report path already used](#74-report-path-already-used), [report directory](#74-report-directory), [before launch](#74-before-launch), [after launch](#74-after-launch) |
| `75` | [the footprint limit](#75-the-footprint-limit), [emergency kill](#75-emergency-kill), [the wall-time limit](#75-the-wall-time-limit), [the launching process exited](#75-the-launching-process-exited), [a signal the command ignored](#75-a-signal-the-command-ignored), [processes left behind](#75-processes-left-behind), [cause not recorded](#75-cause-not-recorded) |
| `126` | [not executable](#126-not-executable) |
| `127` | [not found](#127-not-found) |
| `128 + n` | [signal passed on by mlx-guard](#signal-passed-on-by-mlx-guard), [signal from somewhere else](#signal-from-somewhere-else) |

Which section you land in depends on the report, so the same exit code can have several. The
report's `outcome.kind` is the reliable way to tell what ended the run.

## A worked example

The recorded tutorial run was stopped by its footprint limit and ended with exit 75. The tutorial
was recorded before these lines existed. These are the two lines 0.4.0 prints for that run, rendered
from its report. The second continues with ` More:` and a link to
[the footprint limit](#75-the-footprint-limit):

```text
mlx-guard: what happened: the OS-accounted footprint reached 6.05 GiB, at or above the 6.00 GiB limit you set, on 2 samples in a row, so mlx-guard sent SIGTERM to the command's process group at 40.0 s.
mlx-guard: what to do: observe the command to see its peak, then fix the growth or raise --max-footprint.
```

The footprint is the OS-accounted number mlx-guard samples, 6.05 GiB. The limit
was 6.00 GiB and you set it. Two samples in a row were at or above it, which is the default
(`configuration.required_breach_samples`). So mlx-guard sent SIGTERM, 40.0 seconds into the run.
The report holds each of those facts: the `transitions` entry that crossed the limit, the `signals`
record for the TERM, and `checkpoint.status`.

## 64: the command line

**What happened.** mlx-guard did not like its own command line, so nothing was launched and no
report was written. It prints: `the command line was not accepted ({flag}), so nothing was launched
and no report was written.` The flag is named only for mlx-guard's own options, such as
`--max-footprint`. Two other forms exist: `a setting was rejected before launch (the error above
names it), so nothing was launched and no report was written.` and `the internal --client-ready-fd
descriptor could not be used, so nothing was launched and no report was written.`

**What to do.** `fix {flag} as the error above says, then run the command again.` Without a named
flag it says `fix the command line as the error above says, then run the command again.` For the
`--client-ready-fd` form: `leave --client-ready-fd out when you run mlx-guard yourself; it is for
the Python client. If the Python client produced this, open an issue.` Byte values need a binary
suffix such as `GiB`; see the [value grammar](CLI.md#value-grammar).

**Report fields.** There is no report to read. The line above the two plain lines names the problem.

## 64: a launch setting

**What happened.** The command line was fine, but a launch setting could not be used. It prints:
`mlx-guard refused a launch setting before starting the command, so nothing ran; the error line on
stderr names the setting.`

**Why.** mlx-guard checks `--cwd` and `--env` before it starts the command. A missing or non-directory
`--cwd`, or an `--env` that sets `MLX_GUARD_CHECKPOINT_FD`, is refused.

**What to do.** `--cwd must name an existing directory, and --env must not set
MLX_GUARD_CHECKPOINT_FD; fix the one named, and give the retry a new --report name, because this run
kept its report.`

**Report fields.** `outcome.kind` is `invalid_configuration`. The report exists, which is why the
retry needs a new `--report` name.

## 70: before launch

**What happened.** mlx-guard stopped before it launched anything and wrote no report. One of four
sentences, each ending `so it launched nothing and wrote no report.`:

- `mlx-guard needs macOS to read the OS-accounted footprint, so it launched nothing and wrote no
  report.` What to do: `run it on a Mac with Apple Silicon.`
- `mlx-guard could not read the OS-accounted footprint on this Mac, so it launched nothing and wrote
  no report.` What to do: `check the compatibility page for your macOS version, then open an issue
  with your macOS version and chip.` The [compatibility matrix](COMPATIBILITY.md) is that page.
- `mlx-guard could not set up the run's identity, so it launched nothing and wrote no report.` What
  to do: `make sure the command after -- names a file (an empty string or / does not); if it does,
  open an issue.`
- `mlx-guard could not set up its own supervision, so it launched nothing and wrote no report.` What
  to do: `run it again; if it repeats, open an issue with the error line above.`

**Report fields.** None; there is no report.

## 70: while starting the command

**What happened.** `mlx-guard could not finish starting the run, so it supervised nothing; the
error line on stderr names the step.`

**Why.** The report records a supervisor failure at time 0 with no samples, so the run never began
supervising.

**What to do.** `run it again; if it repeats, open an issue with that error line and the redacted
report.`

**Report fields.** `outcome.kind` is `supervisor_failure`, `outcome.at_ms` is 0, and `samples` is empty.

## 70: footprint readings lost

**What happened.** mlx-guard could not read the OS-accounted footprint for several samples in a
row. The default is 3 (`configuration.max_missing_samples`). What it did next depends on the mode.

`run` stops the command rather than let it run unwatched: `mlx-guard could not read the OS-accounted
footprint for 3 samples in a row (last reading: a process disappeared), so it stopped the command
rather than let it run unwatched: it sent SIGTERM to the command's process group at 5.2 s.` What to
do: `run it again on a less loaded machine; if it repeats, open an issue with the redacted report.`

`observe` never signals the command: `mlx-guard could not read the OS-accounted footprint for 3
samples in a row (last reading: permission denied), so observation ended at 12.4 s. Observe mode
does not stop the command, so it may still be running.` What to do: `check whether the command is
still running and stop it yourself if you need to, then observe again.` The command may be running
unobserved.

**Report fields.** `outcome.kind` is `supervisor_failure`; the last entries of `samples` show the
reading error; `signals` shows what, if anything, was sent.

## 70: a signal not delivered

**What happened.** `run` could not deliver a signal to the command's process group: `mlx-guard
could not deliver SIGKILL to the command's process group, which still had a live member or could not
be checked (result: permission denied).` The result can also read `failed`.

**What to do.** `check whether the command is still running and stop it yourself, then open an issue
with the redacted report.` The command may still be running.

**Report fields.** In `signals`, the record whose `target` is `owned_process_group` and whose
`result` is `permission_denied` or `failed`. A signal that reaches a group with no live member is
not a failure; see the [policy contract](POLICY.md).

## 70: an internal failure

**What happened.** `mlx-guard hit an internal failure while supervising, and it sent SIGTERM to the
command's process group at 120 ms; the error line on stderr names it.` The TERM clause appears only
when the report has a TERM record.

**What to do.** `check whether the command is still running, then run it again; if it repeats, open
an issue with that error line and the redacted report.`

**Report fields.** `outcome.kind` is `supervisor_failure`; `signals` and `transitions` show what
mlx-guard did before it failed.

## 74: report path already used

**What happened.** `an earlier run already used this report path, and its report and hidden journal
are kept, so nothing was launched.`

**Why.** mlx-guard never overwrites an earlier run's evidence.

**What to do.** `pass a new --report name, or move both the report and its hidden .<name>.journal
away before reusing the path.`

**Report fields.** None for this run. The old report belongs to the earlier run.

## 74: report directory

**What happened.** `the report directory is missing, not yours, or not owner-only (mode 0700), so
nothing was launched.`

**What to do.** `create the directory with mkdir -m 700, or fix its owner and mode, then run again.`

**Report fields.** None; no report was written.

## 74: before launch

**What happened.** `mlx-guard could not prepare its journal ({detail}), so nothing was launched.`
The detail is one of `the journal could not be created`, `a write failed`, `a sync to disk failed`,
`reading the journal back failed`, `a journal record was invalid`, `the report path is a link or
another kind of file`, or `the final report could not be written`. Some failures name no detail.

**Why.** The journal may already exist on disk when one of these failures happens after it was
created. A plain retry on the same report path would then stop on that leftover journal.

**What to do.** `check free disk space, then pass a new --report name or move the hidden
.<name>.journal file away, and run again.`

**Report fields.** None; nothing was launched.

## 74: after launch

**What happened.** Three cases share this section. The command ran, but the report could not be
completed: `the command was supervised, but mlx-guard could not write its complete report ({detail});
safety enforcement was not affected.` Or the command could not be launched and mlx-guard could not
record why: `the command could not be launched, and mlx-guard could not finish the report about
that, so the cause is not recorded.` Or the run did not finish starting and the report was lost
too: `mlx-guard could not finish starting the run, and could not write its report ({detail}), so it
supervised nothing.` The error lines above these two name the step that failed.

**What to do.** For the first: `check free disk space and that nothing else changed the report
directory during the run; the hidden .<name>.journal beside the report path keeps what was saved. Use
a new --report name next time.` For the second: `check free disk space and the report directory,
then run again with a new --report name to see the launch error.` For the third: `check free disk
space and the report directory, then run again with a new --report name; the error lines above name
the step.`

**Report fields.** Usually none. The hidden `.<name>.journal` beside the report path holds what was
saved; the [report reference](REPORTS.md#persistence-and-ownership) explains it.

## 75: the footprint limit

**What happened.** The OS-accounted footprint was at or above the `--max-footprint` limit you set on
enough samples in a row, so mlx-guard stopped the command. The recorded run in the example above is
the typical case. If a checkpoint was negotiated, the sentence goes on, for example: `mlx-guard asked
the command to save a checkpoint at 40.3 s and then sent SIGTERM to its process group. The command
acknowledged checkpoint request 5404420652406863310.` Other checkpoint outcomes are `The command did
not acknowledge checkpoint request 11 within 1.0 s; it may still have saved it.` and `The checkpoint
request could not be delivered.` A report with no breach reading says `the OS-accounted footprint
was at or above the 100 B limit you set on 2 samples in a row`, with no measured value.

**What to do.** `observe the command to see its peak, then fix the growth or raise --max-footprint.`
When a checkpoint was acknowledged, it adds `; resume from the state saved for checkpoint request
{id}`. When it timed out: `; if the command saved state for checkpoint request {id}, resume from it`.

**Report fields.** `configuration.max_footprint_bytes` and `configuration.required_breach_samples`;
the `transitions` entry that went from warning to checkpoint requested or terminating, with its
`aggregate_footprint_bytes`; `signals` for the TERM; `checkpoint.status` and `checkpoint.request_id`.

## 75: emergency kill

**What happened.** A single sample reached the emergency band: `one
sample of the OS-accounted footprint, 7.00 GiB, was at or above the emergency threshold of 6.60 GiB,
which is 614.40 MiB more than your 6.00 GiB limit, so mlx-guard sent SIGKILL to the command's
process group at once at 12.3 s, with no checkpoint and no grace period.` If the command had already
ended, the end reads `so mlx-guard found the command already ended when it sent SIGKILL at 12.3 s.`

**Why.** The emergency threshold is a sampled intervention threshold that sits 10 % of the limit
past the limit by default. At or above it, mlx-guard skips the checkpoint request and the TERM grace
period.

**What to do.** `find what allocates that much between samples, or raise --max-footprint if that peak
is expected; a shorter --sample-interval sees growth sooner.`

**Report fields.** `configuration.emergency_footprint_bytes` and `configuration.max_footprint_bytes`;
the `transitions` entry that went to `emergency`; the KILL record in `signals`.

## 75: the wall-time limit

**What happened.** `the run reached the {wall} wall-time limit you set, so mlx-guard sent SIGTERM to
the command's process group at {t}.` If the command was still running afterwards, it adds `, and
SIGKILL 1.0 s later because it was still running`; if a second terminal signal arrived, `, and
SIGKILL 206 ms later, after a second terminal signal arrived`. When the command had already ended,
the wording changes to match, for example `mlx-guard found the command already ended when it sent
SIGTERM at 1.0 s` or `mlx-guard asked the command to save a checkpoint at 1.0 s; the command ended
before the SIGTERM that followed could reach it`.

**What to do.** `raise --wall-time if the command needs longer, or find out why it ran longer than
expected.` With an acknowledged or timed-out checkpoint it adds the same resume advice as above.

**Report fields.** `configuration.wall_time_ms`; `signals` with `reason` `wall_time`;
`checkpoint.status`.

## 75: the launching process exited

**What happened.** `the process that started mlx-guard exited at 2.1 s, and the default
--on-parent-exit=terminate stops the command with it, so mlx-guard sent SIGTERM to the command's
process group at 2.1 s.`

**What to do.** `keep the launching process running until the run ends, or pass
--on-parent-exit=detach to let the command outlive it.` See [the CLI contract](CLI.md) for how the
parent watch works.

**Report fields.** `outcome.parent_exited_at_ms`; `signals` with `reason` `parent_exit`.

## 75: a signal the command ignored

**What happened.** `the command was still running after the SIGINT that mlx-guard passed on to it, so
mlx-guard sent SIGKILL to its process group at 1.4 s.`

**Why.** You pressed Ctrl-C, or something sent mlx-guard a terminal signal. mlx-guard passed it on,
waited the one-second grace, and escalated.

**What to do.** `nothing, if you meant to stop it; otherwise make the command exit when it receives
SIGINT or SIGTERM.`

**Report fields.** In `signals`, the forwarded record and the KILL record with `reason`
`external_signal`.

## 75: processes left behind

**What happened.** `the command exited, but processes it started were still running after SIGTERM, so
mlx-guard sent SIGKILL to them at 1.2 s; the command's own exit status is in the report under
outcome.child_status.` If they had ended by the time of the KILL, it reads `so mlx-guard found them
already ended when it sent SIGKILL at 1.2 s`.

**What to do.** `make the command wait for, or stop, the processes it starts before it exits.`

**Report fields.** `outcome.child_status`; `signals` with `reason` `root_exit_cleanup`.

## 75: cause not recorded

**What happened.** `mlx-guard sent SIGTERM to the command's process group at 13 ms, and this report
does not record which limit caused it.` A checkpoint sentence may follow, such as `A checkpoint was
requested but not acknowledged.`

**Why.** Reports written before the cause was recorded carry no `reason`, and mlx-guard does not
guess.

A report that does record a reason, on a signal or on the checkpoint, but fits none of the other
75 sections gets a shorter form that tells what mlx-guard did and says that no limit caused it, because
the reason recorded is not a limit: `mlx-guard sent SIGTERM to the command's process group at 400 ms.`

**What to do.** `read signals and transitions in the report to see which limit was crossed.` For the
shorter form: `read signals in the report to see what mlx-guard did; no limit was crossed.`

**Report fields.** `signals`, `transitions`, `checkpoint`.

## 126: not executable

**What happened.** `the file named after -- exists but could not be run, so nothing was launched.`

**What to do.** `make it executable (chmod +x) or put its interpreter first, as in -- python
script.py; give the retry a new --report name, because this run kept its report.`

**Report fields.** `outcome.kind` is `launch_not_executable`.

## 127: not found

**What happened.** `no executable was found for the command named after --, so nothing was
launched.`

**What to do.** `check the spelling, give the full path, or activate the environment that provides it;
give the retry a new --report name, because this run kept its report.`

**Report fields.** `outcome.kind` is `launch_not_found`.

## Signal passed on by mlx-guard

**What happened.** The exit code is 128 plus a signal number, and mlx-guard received that signal and
passed it on: `the command ended on SIGINT, which mlx-guard received and passed on to it.` For
SIGKILL: `the command ended on SIGKILL, which mlx-guard sent after a second terminal signal arrived.`

**What to do.** `nothing to do if you sent it.`

**Report fields.** `outcome.child_status.signal`, and the matching `external_signal` record in
`signals`.

## Signal from somewhere else

**What happened.** The command ended on a signal that mlx-guard did not send: `the command ended on
SIGSEGV, and it did not come from mlx-guard.` For SIGKILL it adds `; something else stopped it, and
macOS can do this when memory runs short.`

**What to do.** `the command's own output or its macOS crash report says why.` For SIGKILL: `check
the command's own output and the macOS logs; if memory ran short, set --max-footprint (with mlx-guard
run) below that point so mlx-guard stops the command first.`

**Report fields.** `outcome.child_status.signal`; `signals` has no delivered record for that signal.

## A code mlx-guard also uses

**What happened.** The command itself exited with 64, 70, 74, 75, 126, or 127, and mlx-guard did not
intervene: `the command itself exited with code 75, and mlx-guard did not intervene; 75 is also one
of mlx-guard's own exit codes.`

**What to do.** `read the command's own output; the report's outcome.kind is child_exited.`

**Report fields.** `outcome.kind` is `child_exited` and `outcome.code` is the command's code. Any
other command exit code, including 0, prints nothing.
