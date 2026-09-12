# Tutorial runs on the M1 Max 32 GB

This directory holds the recordings behind [TUTORIAL.md](../../../TUTORIAL.md): four runs of the
tutorial's document summarizer (`examples/tutorial/summarize_docs.py`) under `mlx-guard` 0.2.0 on
the reference host, one after another on 2026-09-12. Nothing in the tutorial's transcripts or
numbers comes from anywhere else. Sizes below are decimal gigabytes; the limit the runs used is
the command-line literal `6GiB`, which is 6.44 GB.

| Run | Command shape | Outcome | Report |
|---|---|---|---|
| Observe, 12 pages, bug on | `mlx-guard observe --sample-interval 50ms` | `child_exited`, 0; peak footprint 3.82 GB | [observe.json](reports/observe.json) |
| Whole corpus, 6GiB limit, bug on, no checkpoint | `mlx-guard run --max-footprint 6GiB --wall-time 10m` | `policy_intervention` at 40.0 s after page 40, TERM, exit 75, no progress file | [run-limit.json](reports/run-limit.json) |
| Same, checkpoint on, resumed | the same `run`, twice, through a resume loop | run 1: checkpoint acknowledged 578 ms after the request at page 40, then TERM, exit 75; run 2: 31 pages, exit 0 | [resume-1.json](reports/resume-1.json), [resume-2.json](reports/resume-2.json) |
| Whole corpus, 6GiB limit, bug fixed | the same `run` with `--no-keep-caches` | `child_exited`, 0; peak footprint 3.04 GB | [run-fixed.json](reports/run-fixed.json) |

`transcripts/` has the terminal output of each run with the working directory generalized to
`reports/`. `reports/summaries.json` is the finished work product of the resumed run: the 71
page summaries and the corpus fingerprint the script checks before resuming.
[provenance.json](provenance.json) records the commit, the example script's hash, the guard
wheel, the model revision, the `mlx-lm` and `mlx` versions, the corpus size and fingerprint, and
the macOS build. The checksummed journals that sat beside each report were present and unused,
and are not committed. Every file here passed `scripts/scan-evidence-bundle.sh`.

The host was a 10-core Apple M1 Max MacBook Pro with 32 GB of unified memory on macOS 26.6.2
(25G83), on AC power, with 73 % of memory free before the first run. These recordings show the
supervision paths behaving as their contracts describe on one ordinary workload on one machine.
They are not a timing or memory guarantee for any other host; the
[compatibility matrix](../../../docs/COMPATIBILITY.md) says which setups have evidence.
