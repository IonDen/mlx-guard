# GitHub shared-VM escalation-envelope captures

Five captures of the checkpoint- and signal-escalation timing envelope, taken on GitHub's shared
`macos-15` runner (Apple M1 (Virtual), 3 vCPU, 7 GB memory, uncontrolled co-tenancy) rather than
the dedicated reference host. They come from the dispatch-only `.github/workflows/envelope.yml`
workflow, run id 34591393999 against `main` at commit `9d42fc8`, dispatched
2026-09-11. Raw JSON is under `raw/escalation-envelope/run1` through `run5`. The runner's
hardware lines, CPU count, memory size, and per-repetition load are recorded in each capture.

This is the "verified, shared VM" cell of the [compatibility matrix](../../../docs/COMPATIBILITY.md):
the full macOS test suite passes on this runner on every push to `main`, and these captures corroborate
the reference host's envelope. They never gate a release and are never a timing reference.

## Pooled results

Each capture runs 20 repetitions per scenario. The table below pools every raw interval value
across all five captures per scenario and interval (100 values per row) and takes p95 over the
pooled set. p95 is the value at sorted index `ceil(n * 95 / 100) - 1` (ascending order), the same
convention `crates/mlx-guard-cli/tests/escalation_envelope.rs` and the M1 Max reference bundle use.

| Scenario.interval | pooled p95 (ms) | max (ms) | n | missing | timed_out |
|---|---:|---:|---:|---:|---:|
| `checkpoint_ack_idle.request_to_ack` | 93 | 103 | 100 | 0 | 0 |
| `checkpoint_ack_idle.term_to_quiet` | 90 | 93 | 100 | 0 | 0 |
| `checkpoint_ack_loaded.request_to_ack` | 96 | 131 | 100 | 0 | 0 |
| `checkpoint_ack_loaded.term_to_quiet` | 94 | 99 | 100 | 0 | 0 |
| `group_term.term_to_quiet` | 94 | 125 | 100 | 0 | 0 |
| `group_kill.kill_to_quiet` | 96 | 110 | 100 | 0 | 0 |

These numbers corroborate the reference-host envelope in [`../m1-max-32gb/`](../m1-max-32gb/README.md).
Reported system load at the start of a repetition ranged from about 0.93 to 20.16
(1-minute average) across the five runs, reflecting the shared runner's uncontrolled co-tenancy
rather than a controlled load profile. The previous fold, from run 33044176184 at commit
`ab13cb2` on 2026-08-27, is in this file's history.
