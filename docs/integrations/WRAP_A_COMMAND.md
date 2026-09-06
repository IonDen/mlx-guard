# Wrap a command

This is for anyone with a command line that loads a model (a training script, an inference server,
a batch job, a shell script that calls `python` a dozen times) who wants a sampled, external
intervention threshold around it without writing an adapter or importing anything into that
workload. Five minutes gets you a report and a footprint number. Wiring `mlx-guard` into the
workload's own process, so it can negotiate a cooperative checkpoint and give you a resume path, is
a separate step covered by the [Python API](../PYTHON_API.md), not this page.

Wrapping a command this way gives you an external, sampled intervention threshold that reduces
risk, a cooperative checkpoint request before escalation when the workload negotiates one, and a
redacted report of what actually ran. It does not give you an unconditional memory ceiling, panic
prevention, proof that a checkpoint saved anything durable, or protection against TERM or KILL
losing work in progress. A plain command line that never negotiates a checkpoint reports exactly
that: see the ladder below for what it looks like.

## Install

```bash
pip install mlx-guard
```

This installs only on Apple Silicon macOS (macOS 11 or newer, arm64); other platforms have no
matching wheel, and `pip` fails by design rather than falling back to a source build.

The CLI also works without a Python project: `uvx mlx-guard …` runs it on demand, and
`pipx install mlx-guard` keeps it on your PATH. The other prerequisites for the commands on this
page are `python3`, used below to build the demo workload, and `jq` for the report-reading commands
(or use the Python `load_report` route shown at the end).

`mlx-guard` refuses to start if its standard input is an interactive terminal: it exits 64 with
`mlx-guard: interactive terminal input is unsupported` before launching anything. Run the commands
on this page from a script, or add `< /dev/null` to the command line, as the transcripts below do.

## The ladder

Every recipe on this page starts the same way: run the command bare, watch it under `observe`,
enforce a limit that never fires, then force an intervention on purpose so you can see the shape of
the report before you need to read one for real. The same demo command runs at every rung,
`python3 -c "import time; time.sleep(3)"`, a process that does nothing for three seconds and exits
cleanly.

Bare, no supervision:

```console
$ python3 -c "import time; time.sleep(3)"
$ echo $?
0
```

`observe` samples the same OS-accounted footprint enforcement would use, but never intervenes:

```console
$ mkdir -m 700 reports
$ mlx-guard observe --report reports/observe.json -- python3 -c "import time; time.sleep(3)" < /dev/null
mlx-guard: child_exited at 3047ms; 57 samples, 0 signals
$ echo $?
0
```

```console
$ jq '[.samples[] | select(.aggregate_footprint_bytes.status == "available") | .aggregate_footprint_bytes.value] | max' reports/observe.json
6930888
$ jq '[.samples[] | select(.aggregate_footprint_bytes.status != "available")] | length' reports/observe.json
0
$ jq '.outcome.kind, .signals' reports/observe.json
"child_exited"
[]
```

A footprint in a report is a tagged value, not a bare number: `available` means the whole owned
group was measured, and the other states record that it was not. The first command therefore filters
on the status before taking a maximum, and the second counts the samples it skipped. A nonzero count
means part of the run went unmeasured, and the peak you just read is the peak of what was seen.
[Reports and privacy](../REPORTS.md) lists the states.

A bare Python interpreter sleeping costs under 7 MiB here. `run` requires an explicit limit; set one
well above that observed peak and nothing changes:

```console
$ mlx-guard run --max-footprint 64MiB --report reports/run.json -- python3 -c "import time; time.sleep(3)" < /dev/null
mlx-guard: child_exited at 3068ms; 57 samples, 0 signals
$ echo $?
0
```

```console
$ jq '.outcome.kind, .signals' reports/run.json
"child_exited"
[]
```

Now force an intervention on purpose, with `--wall-time` rather than a tight footprint limit. A
wall-time expiry is always graceful: the supervisor still attempts a checkpoint request first, finds
no cooperative endpoint to answer it since the command never connected a worker, and only then sends
TERM.

```console
$ mlx-guard run --max-footprint 64MiB --wall-time 1s --report reports/run-wall-time.json -- python3 -c "import time; time.sleep(3)" < /dev/null
mlx-guard: policy_intervention at 1095ms; 20 samples, 1 signal
$ echo $?
75
```

```console
$ jq '.outcome.kind, .checkpoint, .signals' reports/run-wall-time.json
"policy_intervention"
{
  "status": "not_negotiated",
  "reason": "wall_time"
}
[
  {
    "at_ms": 1024,
    "signal": 15,
    "target": "owned_process_group",
    "result": "delivered",
    "reason": "wall_time"
  }
]
```

A plain CLI workload negotiates no checkpoint, so the record says exactly that: `not_negotiated`
with a `reason` recording the attempted request, and no `request_id`, since there was never a
cooperative endpoint to acknowledge one.

### Optional: forcing a footprint intervention instead

A tight `--max-footprint` is a worse way to force this demo than `--wall-time`, and worth
understanding before you pick a real limit. `mlx-guard` treats a sample at or above `1.1 ×` the
limit as an emergency: it sends `SIGKILL` immediately, with no TERM record and no negotiated
checkpoint attempt at all. Setting the limit far below what a command actually uses jumps straight
into that band and shows the wrong shape. Forcing the graceful path instead (the same TERM, grace,
checkpoint-request sequence as the wall-time run above, just triggered by footprint) means picking a
limit close to a measured peak rather than an arbitrarily low one, and using a workload that holds a
stable allocation rather than one that keeps growing:

```console
$ mlx-guard observe --report reports/observe-footprint.json -- python3 -c "import time; b = bytearray(200 << 20); time.sleep(30)" < /dev/null
mlx-guard: child_exited at 30115ms; 558 samples, 0 signals
$ echo $?
0
$ jq '[.samples[] | select(.aggregate_footprint_bytes.status == "available") | .aggregate_footprint_bytes.value] | max' reports/observe-footprint.json
216810072
```

That workload holds a steady ~206.8 MiB (216,810,072 bytes) once its one allocation lands. The
emergency band sits at `1.1 ×` the limit, so a limit of `200MiB` (209,715,200 bytes) keeps the
observed peak below it while still sitting under the peak itself. The run breaches the limit
without ever entering the emergency band:

```console
$ mlx-guard run --max-footprint 200MiB --report reports/run-footprint.json -- python3 -c "import time; b = bytearray(200 << 20); time.sleep(30)" < /dev/null
mlx-guard: policy_intervention at 165ms; 3 samples, 1 signal
$ echo $?
75
```

```console
$ jq '.outcome.kind, .checkpoint, .signals' reports/run-footprint.json
"policy_intervention"
{
  "status": "not_negotiated",
  "reason": "footprint"
}
[
  {
    "at_ms": 110,
    "signal": 15,
    "target": "owned_process_group",
    "result": "delivered",
    "reason": "footprint"
  }
]
```

Same graceful shape as the wall-time run, this time with `reason: footprint` throughout. The
[policy contract](../POLICY.md) covers the full band and timing behavior; the point here is only
that a demo limit meant to force a clean TERM has to sit close to a real peak, not arbitrarily low.

## Two real recipes

The ladder above uses a throwaway command so every number in it could be captured fresh for this
page. Real workloads look the same from `mlx-guard`'s side, a literal argument vector and a byte
limit, just with a limit chosen from your own measurements instead of a three-second sleep. Neither
recipe below prints a limit, because the limit is the one part nobody can write for you: measure
your own command and substitute the number.

A small LoRA fine-tune with `mlx-lm`:

```bash
mlx-guard run --max-footprint <observed peak + headroom> --report reports/lora-finetune.json -- \
  uvx --from 'mlx-lm[train]' mlx_lm.lora \
    --model mlx-community/Qwen2.5-0.5B-Instruct-4bit \
    --train --data mlx-community/wikisql --iters 200 --adapter-path adapters
```

Loading the dataset from the Hub this way pulls in the `datasets` package, which `mlx-lm` ships
only through its `train` extra; without it, the command loads the model and then stops on that
import.

An `mflux` image generation, quantized to 8 bits:

```bash
mlx-guard run --max-footprint <observed peak + headroom> --report reports/mflux-generate.json -- \
  mflux-generate --model schnell --quantize 8 --prompt "a lighthouse at dusk" \
    --steps 4 --seed 42 --output out.png
```

That `--quantize 8` is load-bearing. `mflux-generate` quantizes nothing by default, so leaving it
off loads FLUX.1-schnell at the precision it ships in, which is a different workload with a
different footprint from the one an 8-bit run measures. Whichever you choose, measure the
configuration you actually intend to enforce.

### Choosing the limit

Run the real command under `observe` with representative arguments, read the peak out of the report
the way the ladder above does, and set `--max-footprint` above it with headroom for run-to-run
variation, never as a fraction of total machine memory. Repeat the observe run rather than trusting
one of them: the number you want is the highest repeatable peak, not whichever peak the first run
happened to produce. Whatever limit you set, enforcement authorizes a little more: the emergency
KILL band sits about 10 % above it (`limit + max(1 byte, limit / 10)`), so the real ceiling is the
limit plus that margin.

Two details decide whether the peak you read is the peak that matters.

The first is how long the report remembers. Sample history is a ring of the most recent 4,096
windows, and each new sample evicts the oldest, so at the default 50 ms interval a report holds
about three and a half minutes. An MLX workload spends its highest footprint early, while weights
load and the first step compiles, so observe a longer run at the default and that peak is gone
before the command exits. You would be calibrating from the steady-state tail, and the enforcing run
would trip during load. Size the interval so `4096 × interval` covers the whole run:
`--sample-interval 1s` reaches about 68 minutes, and 10s is the maximum the CLI accepts. Sampling
less often also means a spike that rises and falls between two samples is one the supervisor never
sees, so widen the interval deliberately rather than by default.

The second is that MLX holds memory you did not ask it to hold. OS-accounted footprint includes
MLX's retained buffer cache, and both tools above ship knobs that move it: `mflux-generate` has
`--low-ram` and `--mlx-cache-limit-gb`, and `mlx_lm.lora` has `--clear-cache-threshold`. Each of
them changes the number `observe` reports. Measure with the same cache settings the enforced run
will use, or the limit you derive belongs to a workload you are not running.

Recalibrate after a change in the workload, in MLX, in macOS, or in the machine.
[Observe and calibration](../OBSERVE_AND_CALIBRATION.md) covers the report's `calibration` section
and the partial-sample rules in full.

Fine-tunes and generations of this shape are what the maintainers' in-house trial exercises end to
end on the reference host (M1 Max, 32 GB). Their output is real workload output, not something this
page can fabricate, so none is pasted here, and the limit is left as a placeholder for the same
reason: the peak belongs to your machine. The report shapes captured from the throwaway demo command
above are what an actual run of either one produces.

One distinction matters between the two halves of this page. Every intervention above was *forced*:
a wall-time limit shorter than the sleep, or a footprint limit set deliberately close to the
observed peak. A limit chosen for real work sits above the observed peak with headroom and should
never fire. If `--max-footprint` on a production run does trip, that is the workload using more
memory than expected, not the demo behavior above repeating itself.

## Reading the report after an intervention

`jq` is enough for a quick check:

```bash
jq '.outcome.kind, .checkpoint' reports/lora-finetune.json
```

From Python, `load_report` gives you the same data as a typed object:

```python
from pathlib import Path

import mlx_guard

report = mlx_guard.load_report(Path("reports/lora-finetune.json"))
print(report.outcome.kind, report.payload["checkpoint"]["status"])
```

A command wrapped this way, never having connected `mlx_guard.CheckpointWorker`, always reports
`checkpoint.status: not_negotiated`, so there is no `request_id` or `artifact` to resume from; the
process simply stopped where TERM or KILL caught it. Resuming from a saved checkpoint is only
possible for a workload that connects the worker helper itself, which is the
[adapter pattern](PYTHON_ADAPTER.md), not this page's wrapped-CLI recipe; see its
[resume walkthrough](../PYTHON_API.md#resuming-after-an-intervention) for the match to write.
[Reports and privacy](../REPORTS.md) defines the full schema; every field name and status value used
above comes from there.
