# The Python adapter pattern

This is for the author of a library that starts a workload on someone else's behalf: a trainer, a
generation pipeline, a batch runner, anything whose users call your function rather than your
command line. An adapter puts supervision inside that call. Your users keep writing
`your_library.train(...)`, and the adapter launches the real work under `mlx-guard`, negotiates a
checkpoint before escalation, and hands the report back. Anyone who already has the command line in
front of them wants [wrap a command](WRAP_A_COMMAND.md) instead; that page needs no code at all, and
nothing here is required to use it.

Two things separate an adapter from a wrapped command line. The workload connects the checkpoint
worker itself, so a request for a checkpoint before TERM reaches real code instead of being refused
by a process that never negotiated one. And the report that comes back carries the `request_id` the
workload tagged its own saved state with, which is what a later process needs to pick that state
back up.

## The pattern

Both halves live in one file below so it runs as written. In a library the launching half is your
entry point, and the supervised half is the script or module it starts. Save it as
`adapter_demo.py`:

```python
"""Cooperative supervision: the launcher an adapter exposes, and the workload it wraps."""

from __future__ import annotations

import json
import os
import sys
import time
from pathlib import Path

import mlx_guard

CHECKPOINTS = Path("checkpoints")
REPORTS = Path("reports")


def train() -> None:
    """The supervised side: a work loop that saves state when the supervisor asks."""
    state = {"step": 0}

    def save_checkpoint(request: mlx_guard.CheckpointRequest) -> mlx_guard.CheckpointResponse:
        CHECKPOINTS.mkdir(mode=0o700, exist_ok=True)
        directory = CHECKPOINTS / str(request.request_id)
        directory.mkdir(mode=0o700, parents=False, exist_ok=True)
        payload = directory / "step"
        with payload.open("w") as handle:
            handle.write(f"{state['step']}\n")
            handle.flush()
            os.fsync(handle.fileno())
        saved = sum(entry.stat().st_size for entry in directory.iterdir() if entry.is_file())
        return mlx_guard.CheckpointResponse.completed(
            mlx_guard.CheckpointArtifact(
                mlx_guard.CheckpointArtifactKind.DIRECTORY,
                size_bytes=saved,
            )
        )

    worker = mlx_guard.CheckpointWorker.connect(save_checkpoint)
    if worker is None:
        raise SystemExit("not launched by mlx-guard, so there is nothing to negotiate")
    with worker:
        while True:
            state["step"] += 1
            time.sleep(0.05)  # stands in for one training step
            worker.poll()


def supervise() -> None:
    """The launching side: what the adapter does on the library user's behalf."""
    REPORTS.mkdir(mode=0o700, parents=True, exist_ok=True)
    process = mlx_guard.start(
        mlx_guard.RunConfig(
            command=(sys.executable, __file__, "train"),
            report=REPORTS / f"adapter-demo-{os.getpid()}.json",
            max_footprint_bytes=2 * 1024**3,
            wall_time_ms=2_000,
            checkpoint_timeout_ms=2_000,
        )
    )
    result = process.wait()
    print(f"exit code: {result.returncode}")
    print(f"outcome: {result.report.outcome.kind.value}")
    print(json.dumps(result.report.payload["checkpoint"], indent=2, default=dict))


if __name__ == "__main__":
    if sys.argv[1:] == ["train"]:
        train()
    else:
        supervise()
```

On the launching side, `RunConfig` is frozen, its command is a literal argument tuple that never
sees a shell, and an enforcing run needs an explicit byte limit. Take that limit from your caller or
from their own `observe` run, never from a fraction of the machine's memory. Each run gets its own
report path here because a completed run keeps its journal beside the report, and a reused path is
refused rather than overwritten. `checkpoint_timeout_ms` sets how long the supervisor waits for the
worker's acknowledgement before it escalates anyway. `start()` returns a `GuardProcess`, so an
adapter that wants to stream output or cancel has a handle; `wait()` gives back the typed result
and the parsed report.

On the supervised side, `CheckpointWorker.connect()` installs a SIGUSR1 handler and completes the
inherited-descriptor handshake, so it belongs on the main thread before the work loop starts. It
returns `None` when the process was not launched by `mlx-guard`, which is what keeps the same
script runnable on its own. The callback runs on whichever thread calls `poll()`, and it owns the
durability decision: it writes, it flushes, it fsyncs, and only then does it return
`CheckpointResponse.completed()`. Naming the directory after `request.request_id` is the whole
resume mechanism, since the id is the only thing the report and the worker's own filesystem have in
common. The checkpoints root is created on its own line, and the per-request directory with
`parents=False`, because `mkdir(parents=True)` applies its mode to the leaf alone and leaves any
parent it had to create world-listable. Artifact metadata is deliberately path-free, carrying a kind
and an optional size and nothing that could leak a name; the size reported here sums the files under
the directory, so the same callback still says something true once a real checkpoint is more than
one file. The acknowledgement budget is fixed for the run and cannot be extended by a callback that
is making progress, so a real loop polls at safe boundaries and finalizes an incrementally written
checkpoint rather than starting a full model save from scratch. The
[Python API](../PYTHON_API.md#cooperative-checkpoints) covers the threading and deadline rules in
full.

An MLX workload asks two more things of that loop. The first is that the callback not allocate. Put
an `mx.eval(model.parameters(), optimizer.state)` at the same boundary you poll on, so the callback
serializes arrays that are already materialized instead of forcing a lazy graph to evaluate, and
allocate, at the moment memory is already what went wrong; `mx.clear_cache()` before the save hands
MLX's retained buffers back and leaves the save some room. The second is that the budget match a
real step. Size `checkpoint_timeout_ms` against your longest step plus the time that save takes
rather than leaving it at the one-second default: the SIGUSR1 handler only sets a flag, `poll()`
does the work, and a main thread inside a long `mx.eval()` reaches neither until that call returns.
A budget shorter than one step is a budget the worker cannot meet, and the supervisor escalates when
it expires.

The limit above is set at 2 GiB so that it never fires, and the intervention is forced with a
two-second wall-time budget instead. That is deliberate: a footprint limit set far below what a
process actually uses lands in the emergency band and produces an immediate KILL with no checkpoint
request at all, which is the wrong shape to demonstrate. A wall-time expiry always takes the
graceful route. A real footprint breach takes that same route only while it stays inside the band:
two consecutive samples at or above the limit but under `1.1 ×` the limit. A sample that clears
`1.1 ×` is an emergency, so the limit has to leave enough margin that one MLX allocation cannot
carry the process past that multiple between two samples. Inside the band the report reads the same
as below, with `footprint` in place of `wall_time`. If you want to force one on purpose, the
[wrapped-command page](WRAP_A_COMMAND.md#optional-forcing-a-footprint-intervention-instead)
explains the band arithmetic.

## What one real run produced

The blocks below come from a single run of exactly the file above, against a development build of
this repository. Timings and the request id differ from run to run; the shape does not.

```console
$ python adapter_demo.py
mlx-guard: policy_intervention at 2176ms; 40 samples, 2 signals
exit code: 75
outcome: policy_intervention
{
  "status": "acknowledged_unverified_durability",
  "at_ms": 2109,
  "request_id": 16447506383078896645,
  "reason": "wall_time",
  "artifact": {
    "kind": "directory",
    "size_bytes": 3
  }
}
```

The two signals that run recorded are the whole negotiation:

```console
$ jq '.signals' reports/adapter-demo-51961.json
[
  {
    "at_ms": 2039,
    "signal": 30,
    "target": "cooperative_endpoint",
    "result": "delivered",
    "reason": "wall_time"
  },
  {
    "at_ms": 2109,
    "signal": 15,
    "target": "owned_process_group",
    "result": "delivered",
    "reason": "wall_time"
  }
]
```

At 2039 ms the wall-time budget expired and SIGUSR1 (30) went to the cooperative endpoint alone,
never to the group. The worker's next `poll()` wrote its file, fsynced it, and acknowledged. That
acknowledgement landed at 2109 ms, and TERM went to the owned process group in the same
millisecond. The run ended with exit code 75, the policy-intervention code, and the workload's own
state was on disk under the id the report names:

```console
$ cat checkpoints/16447506383078896645/step
37
```

## Reading the report

`acknowledged_unverified_durability` says precisely as much as is known. The worker declared that it
finished, the supervisor recorded the declaration, and nothing in that record is independent proof
that the bytes survived. Your callback made the durability decision; the report repeats it. The
`request_id` is the correlation key to match against, and `artifact` is what the worker chose to
declare about what it wrote.

A later process joins the interrupted run back to that saved state from the persisted report alone.
The [resume walkthrough](../PYTHON_API.md#resuming-after-an-intervention) shows the match to write,
including why `timed_out` deserves the same treatment as an acknowledgement: the supervisor giving
up on the wait does not mean the worker gave up on the save. Give the machine a moment before
launching that resume, though: a released Metal object can stay charged by the OS for more than ten
seconds after the process holding it is gone (see [sampling](../SAMPLING.md)), so a resume started
the instant the first run exits can measure the old run's footprint alongside its own and trip its
limit on memory nothing is using any more.

## What an adapter must and must not claim

An adapter built this way can honestly tell its users what it does for them. It launches their
workload under an external, sampled intervention threshold that reduces risk. It requests a
cooperative checkpoint before escalation, because the workload it wraps negotiates one. It surfaces
the report, so what happened is something they can read rather than infer.

What it must not promise takes longer to say, and matters more. Not a hard memory boundary: the
supervisor samples, so a spike that rises and falls inside one interval is never seen, and one that
stays is acted on at the samples that follow it rather than at the instant it happened. Not panic
prevention, which nothing in user space can offer. Not checkpoint durability, which belongs to your
callback and to the filesystem under it, and which the report only ever repeats back from the
worker. Not protection against TERM or KILL losing work, because whatever was in flight when the
signal arrived is gone. The words "hard boundary" and "guarantees" have no place in an adapter's
documentation, and the [threat model](../THREAT_MODEL.md) is what to point a user at when they ask
what the supervision is worth.
