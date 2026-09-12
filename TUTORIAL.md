# A first afternoon with mlx-guard

This is the story of one small job. It is told twice: first by arithmetic, for what the job would
have done to the Mac on its own, then by recording, for what happened under `mlx-guard`. The job
is ordinary on purpose. You have a folder of Markdown files and a local model, and you want a
two-sentence summary of every page. Nothing about it looks dangerous.

Everything below ran on a MacBook Pro with an M1 Max and 32 GB of unified memory, on macOS
26.6.2, with `mlx-guard` 0.2.0 from PyPI. The transcripts are verbatim; only the report directory
is shown as `reports/`. The reports themselves sit in
[`evidence/v0.2.0/tutorial/`](evidence/v0.2.0/tutorial/README.md), if you want the JSON open
while you read. Sizes in the text are decimal gigabytes. The one binary unit is the limit itself,
because that is how the flag is written: `6GiB` is 6.44 GB.

## The job

The script is [`examples/tutorial/summarize_docs.py`](examples/tutorial/summarize_docs.py). It
loads `mlx-community/Llama-3.2-3B-Instruct-4bit` through `mlx-lm`. It cuts the repository's own
`README.md` and `docs/` into pages of about 3,200 characters. For each page it asks the model for a
two-sentence summary, and at the end it writes the summaries to a JSON file. Seventy-one pages,
about a second each.

It also has a bug, the kind people write on a Tuesday afternoon. The loop keeps every page's
prompt cache in a dictionary. The next version of the script was going to ask follow-up questions,
and recomputing the prompts seemed wasteful:

```python
cache = make_prompt_cache(model)
for response in stream_generate(model, tokenizer, prompt, max_tokens=args.max_tokens, prompt_cache=cache):
    pieces.append(response.text)
summaries[index] = "".join(pieces).strip()
if args.keep_caches:
    caches[index] = cache
```

A prompt cache for this model costs about 115 KB per token: 28 layers, 8 key-value heads, 128
dimensions, keys and values both, in fp16. A 4-bit model still keeps a full-precision cache
unless you ask `mlx-lm` to quantize it. A 3,200-character page is roughly 800 tokens. The cache
grows in blocks of 256 tokens, so a page pins 88 MB or 117 MB rather than a smooth per-token
amount. Over this corpus it averages 90 MB per page, and none of it is ever released. The script
prints what MLX holds after each page, so the growth is visible if you look. Most people do not
look. The job is producing summaries and the fan is quiet.

Set up an environment the way the tutorial did, so the commands match:

```bash
uv venv .venv && source .venv/bin/activate
uv pip install mlx-lm==0.31.3 mlx-guard==0.2.0
install -d -m 700 reports
```

## What this job does to a Mac on its own

We did not run this part. There is no need to, and on a laptop you actually use there is no good
reason to. The arithmetic is enough. It is also the arithmetic you should do for your own jobs.

The recorded runs below put the process at roughly 2.5 GB once the model has loaded. Every page
then adds 90 MB. A Mac reports 32 GB, but macOS and whatever else is open need their share, so a
process can count on something like 25 GB before the system starts to defend itself. At 90 MB a
page, that is 250 pages, or about four minutes at this pace. A 16 GB laptop gets there in a
hundred pages. This tutorial's corpus is 71 pages, so this exact run would have finished. A real
document dump of two thousand pages wants 180 GB of caches.

What happens then is not a clean error. MLX's own limits protect the allocation, not the machine.
Its default memory limit sits above what the system can actually hand out, so it fails only after
macOS has already gone to swap. Unified memory means the GPU's allocations are the system's
memory; there is no separate pool to run out of first. macOS compresses pages, then swaps. Each
of those makes every allocation slower. The job that was doing a page a second now does a page
every ten seconds, then every minute, while the memory keeps growing. The pointer stops moving.
Applications stop redrawing. If you are lucky, `kernel_task` or a jetsam decision kills something:
possibly your terminal, possibly the browser with the form you were filling in. If you are
unlucky, the machine reboots. The [threat model](docs/THREAT_MODEL.md) adds one more case. On
macOS 26.4 and later, a driver bug can panic the kernel under Metal load with the footprint
nowhere near full. No tool in user space can prevent that one.

None of that arrives with a stack trace. You come back to a rebooted Mac, no summaries file, and a
script that looks fine.

## Measure before you decide anything

`mlx-guard` has two modes. `observe` watches and reports. `run` watches and enforces a limit you
give it. The order matters: observe first, so that the limit you choose later is a number you
measured rather than one you guessed.

We watched the buggy script over its first 12 pages, sampling the process group's footprint every
50 ms:

```console
$ mlx-guard observe --sample-interval 50ms --report reports/observe.json -- python examples/tutorial/summarize_docs.py --max-pages 12 --progress reports/observe-progress.json
12 pages, 12 to do, keep_caches=True
page 1/12 in 1.2s; mlx active 1.79 GiB; caches held 1
page 2/12 in 1.0s; mlx active 1.90 GiB; caches held 2
page 3/12 in 0.6s; mlx active 1.96 GiB; caches held 3
page 4/12 in 1.2s; mlx active 2.07 GiB; caches held 4
page 5/12 in 0.4s; mlx active 2.09 GiB; caches held 5
page 6/12 in 1.0s; mlx active 2.18 GiB; caches held 6
page 7/12 in 1.0s; mlx active 2.26 GiB; caches held 7
page 8/12 in 0.9s; mlx active 2.34 GiB; caches held 8
page 9/12 in 1.1s; mlx active 2.45 GiB; caches held 9
page 10/12 in 1.0s; mlx active 2.56 GiB; caches held 10
page 11/12 in 1.0s; mlx active 2.64 GiB; caches held 11
page 12/12 in 1.1s; mlx active 2.75 GiB; caches held 12
done: 12 summaries in 11s -> reports/observe-progress.json
mlx-guard: child_exited at 14387ms; 252 samples, 0 signals
$ echo $?
0
```

The script's own lines already tell the story: a gigabyte in eleven pages, in a straight line. The
guard adds one line at the end. The child exited on its own, 252 samples were taken, and no signal
was sent. Observe mode never sends one.

The report has a `calibration` section written for exactly this moment:

```json
"calibration": {
  "observation_only": true,
  "safety_certified": false,
  "total_samples": 252,
  "complete_samples": 252,
  "observed_duration_ms": 14330,
  "peak_aggregate_footprint_bytes": { "status": "available", "value": 3823077872 },
  "peak_growth_bytes_per_second": { "status": "available", "value": 13164315419 },
  "automatic_limit_bytes": null,
  "guidance": "choose_explicit_limit_from_repeated_representative_runs"
}
```

Two things to read here. The peak is 3.82 GB. That is the process as the operating system charges
it, and it sits about a gigabyte above the script's own figure. The number MLX prints is its
active allocations. It leaves out the buffers MLX keeps in its own cache pool for reuse, the Metal
runtime, and Python itself. `mx.get_cache_memory()` shows the pool; the guard's number is what the
machine actually has to find. The peak growth rate looks alarming at 13 GB/s until you realize it
is the model loading in the first two seconds. The leak's own pace is in the samples, and it is the
90 MB per page you can also see above. The guard does not turn any of this into a limit for you.
`automatic_limit_bytes` is null and always will be. A number invented by the tool would be a number
nobody chose.

So we chose one. Picture the machine that will run this job for real: a 16 GB laptop with a
browser and an editor open. On that machine, `6GiB` is what this process may have. It sits
comfortably above the 3.82 GB peak of a 12-page run, and the rest of the laptop can live with it.

## A limit, and what it buys

Same script, whole corpus, bug still in it, now under `run` with that limit and a wall-clock cap
for good measure. Checkpoints are switched off for this run. We will come back to them.

```console
$ mlx-guard run --max-footprint 6GiB --wall-time 10m --report reports/run-limit.json -- python examples/tutorial/summarize_docs.py --no-checkpoint --progress reports/run-limit-progress.json
mlx-guard: enforcing a 6442450944-byte footprint limit; emergency KILL at 7086696038 bytes, about 10% above the limit
71 pages, 71 to do, keep_caches=True
page 1/71 in 1.2s; mlx active 1.79 GiB; caches held 1
...
page 38/71 in 0.9s; mlx active 4.91 GiB; caches held 38
page 39/71 in 1.0s; mlx active 4.99 GiB; caches held 39
page 40/71 in 0.8s; mlx active 5.07 GiB; caches held 40
mlx-guard: policy_intervention at 40016ms; 702 samples, 1 signal
$ echo $?
75
$ ls reports/run-limit-progress.json
ls: reports/run-limit-progress.json: No such file or directory
```

The first line is the guard stating the deal before the workload starts: the limit, and the
ceiling about ten percent above it where it stops asking and kills. Then 40 pages of ordinary
work. Then one line, and exit code 75. That code means "a policy intervened" and nothing else;
the child's own status never gets mixed into it.

The report says what happened at a resolution the transcript cannot. The process first touched
the warning band at 32.2 s. It dropped back out twice, as pages finished and their working memory
was freed, and stayed in the band from 34.26 s. At 39.96 s two consecutive samples were at or
above the limit, and the guard sent `SIGTERM` to the whole process group. At 40.02 s the group was
gone:

```json
"outcome": {
  "at_ms": 40016,
  "kind": "policy_intervention",
  "final_footprint_bytes": { "status": "available", "value": 6499453256 },
  "child_status": { "status": "signaled", "signal": 15 }
},
"signals": [
  { "at_ms": 39964, "signal": 15, "target": "owned_process_group", "result": "delivered", "reason": "footprint" }
],
"checkpoint": { "status": "not_negotiated", "reason": "footprint" }
```

What that bought is easy to state. The Mac did not notice. The other applications kept their
memory, the pointer kept moving, and the run ended forty seconds in with a file that says exactly
why. Compare the arithmetic section: a rebooted machine and a guess.

There is also a bill. Forty summaries were computed and none were saved. The script writes its
output only at the end, and `SIGTERM` arrived before the end. The `ls` at the bottom of the
transcript is the whole problem in one line. A limit turns a catastrophe into a loss. Turning a
loss into progress takes one more step.

## Make the job resumable

The guard can ask before it terminates. A workload that says yes gets a request, a moment to save,
and only then the signal. The script already had the code for that. It is a dozen lines, shown
here with the type hints trimmed, and it needs no dependency beyond `mlx-guard` itself:

```python
def connect_checkpoint(progress, summaries, fingerprint):
    try:
        import mlx_guard
    except ImportError:
        return None

    def save_checkpoint(request):
        size = save_progress(progress, summaries, fingerprint)
        print(f"checkpoint {request.request_id}: saved {len(summaries)} summaries", flush=True)
        return mlx_guard.CheckpointResponse.completed(
            mlx_guard.CheckpointArtifact(mlx_guard.CheckpointArtifactKind.FILE, size)
        )

    return mlx_guard.CheckpointWorker.connect(save_checkpoint)
```

`connect` returns `None` when `mlx-guard` did not start the process, so the same script runs
unchanged on its own. The request is delivered wherever the script calls `worker.poll()`. The
script calls it after every generated token, not once per page, so during generation the callback
runs within a few milliseconds of the signal. The callback writes the whole progress file, a dozen
kilobytes, flushes it to disk with `fsync`, and renames it into place. That takes milliseconds. The
guard allows one second for all of it by default. The only stretch where the script cannot answer
is a page's prompt prefill, where `mlx-lm` yields nothing until the 800 prompt tokens are
processed. The recorded request landed exactly there, and the acknowledgement came 578 ms after
it. On a slower Mac, or with longer pages, pass `--checkpoint-timeout 5s` and stop thinking about
it.

The progress file carries a fingerprint of the corpus and the page size. `--resume` checks it
first, and refuses a file that belongs to different text rather than trusting its page numbers.
Now put the two together in a loop. It restarts only on exit 75, and it is capped, so a run that
can never finish does not loop forever. The batch finishes itself:

```console
$ n=1
$ while :; do
    mlx-guard run --max-footprint 6GiB --wall-time 10m --report reports/resume-$n.json -- \
      python examples/tutorial/summarize_docs.py --resume --progress reports/progress.json
    rc=$?
    [ $rc -eq 75 ] && [ $n -lt 10 ] || break
    n=$((n+1))
  done
mlx-guard: enforcing a 6442450944-byte footprint limit; emergency KILL at 7086696038 bytes, about 10% above the limit
71 pages, 71 to do, keep_caches=True
page 1/71 in 1.2s; mlx active 1.79 GiB; caches held 1
...
page 40/71 in 0.8s; mlx active 5.07 GiB; caches held 40
checkpoint 5404420652406863310: saved 40 summaries
mlx-guard: policy_intervention at 40935ms; 718 samples, 2 signals
mlx-guard: enforcing a 6442450944-byte footprint limit; emergency KILL at 7086696038 bytes, about 10% above the limit
71 pages, 31 to do, keep_caches=True
page 41/71 in 1.1s; mlx active 1.79 GiB; caches held 1
...
page 71/71 in 1.1s; mlx active 4.69 GiB; caches held 31
done: 71 summaries in 32s -> reports/progress.json
mlx-guard: child_exited at 34809ms; 610 samples, 0 signals
$ echo "finished after $n runs, last exit $rc"
finished after 2 runs, last exit 0
```

Two signals this time. `SIGUSR1` carried the checkpoint request, the script saved forty
summaries, and `SIGTERM` followed. The second run started with thirty-one pages to do and
finished them. The first report records the whole exchange, including the request id the script
printed, so the report and the script's output can be matched line for line:

```json
"checkpoint": {
  "status": "acknowledged_unverified_durability",
  "at_ms": 40865,
  "request_id": 5404420652406863310,
  "reason": "footprint",
  "artifact": { "kind": "file", "size_bytes": 11936 }
}
```

The status is honest about what the guard knows. The script said it completed the save. The guard
did not open the file to check. Durability is the worker's promise, which is why the callback
bothers with `fsync`, and the report says so.

The leak is still there. The job still cannot hold more than about forty pages of caches inside
the limit. But all seventy-one summaries are in `reports/progress.json`, the machine was never in
trouble, and nobody had to watch it.

## Fix the bug and prove it

The fix is one flag on this script, because the script was written to show it. In the version you
would actually ship it is one deleted line: stop keeping the caches. Same limit, same corpus:

```console
$ mlx-guard run --max-footprint 6GiB --wall-time 10m --report reports/run-fixed.json -- python examples/tutorial/summarize_docs.py --no-keep-caches --progress reports/fixed-progress.json
mlx-guard: enforcing a 6442450944-byte footprint limit; emergency KILL at 7086696038 bytes, about 10% above the limit
71 pages, 71 to do, keep_caches=False
page 1/71 in 1.2s; mlx active 1.79 GiB; caches held 0
...
page 35/71 in 1.0s; mlx active 1.77 GiB; caches held 0
...
page 71/71 in 1.1s; mlx active 1.79 GiB; caches held 0
done: 71 summaries in 70s -> reports/fixed-progress.json
mlx-guard: child_exited at 72606ms; 1334 samples, 0 signals
$ echo $?
0
```

Flat at 1.8 GiB of active memory from page one to page seventy-one. The report's highest
footprint is 3.04 GB, under half the limit, and the exit code is the script's own. Keep the guard
on for this run anyway. A limit that never fires costs a few percent of one CPU core; the
[soak measurements](evidence/v0.2.0/m1-max-32gb/soak/README.md) put it between 0.6 and 3 % on
this host. In return you get a report that says the job stayed where you expected. That is the
evidence you will want the day the next Tuesday-afternoon change lands.

## What to take with you

Measure with `observe` before you pick a number. Pick the number for the machine the job will run
on, not for the job. Give `run` that number and a wall-clock cap. Read exit 75 as "the guard
acted", then open the report for the rest. If the job can save its state, spend the dozen lines on
a checkpoint callback and a resume flag, and poll often; a capped loop on exit 75 then finishes
the work without you. When the report shows a flat line, keep the guard on. The report is the
proof.

The [examples](docs/EXAMPLES.md) page has the shortest working commands. The
[calibration guide](docs/OBSERVE_AND_CALIBRATION.md) explains every field in that `calibration`
section. [Wrap a command](docs/integrations/WRAP_A_COMMAND.md) does all of this for a job you
cannot edit. The [Python adapter](docs/integrations/PYTHON_ADAPTER.md) shows the same checkpoint
handshake driven from a library instead of a shell loop.
