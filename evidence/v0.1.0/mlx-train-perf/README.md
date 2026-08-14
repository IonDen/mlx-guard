# mlx-train-perf integration proof

This bundle records a bounded end-to-end run through the consumer's shipped runner boundary. The
consumer was at commit `78de3533c99fbec3a629af5a560831d21d3b99ea`; mlx-guard was built as the
0.1.0 wheel from commit `7d1103b145fc918d57a6277c655923f600317fbf`. The host was an Apple M1 Max
with 32 GB of unified memory, macOS 26.6.1, MLX 0.32.0, and mlx-lm 0.31.3.

The runner launched a 1,000-repetition real MLX loss-layer condition with a 512 MiB OS-accounted
footprint limit, 500 ms wall limit, and 10 ms sampling. At 505 ms, the supervisor requested a
checkpoint. The worker finished repetition 336, atomically wrote and synced the artifact normalized
as [`partial-result.json`](partial-result.json), and returned path-free FILE metadata. The supervisor
accepted the authenticated response at 527 ms, sent TERM to the owned process group, and finalized at
553 ms with zero final footprint. The complete [`report.json`](report.json) contains 37 samples, a
48,218,736-byte maximum aggregate footprint, and no artifact errors.
[`report-summary.json`](report-summary.json) preserves the policy, checkpoint, signals, transitions,
outcome, capabilities, privacy declaration, and hashes of the two source artifacts.

## Failure-path verification

- No acknowledgement: the native blocked-worker test reached the checkpoint deadline and delivered
  TERM without allowing the callback to extend policy time.
- Callback error: the Python helper sent a failed acknowledgement; the consumer adapter propagated a
  failed partial-result write instead of declaring completion.
- Client cancellation: SIGINT was forwarded through the supervisor and recorded as a child signal.
- Missing binary and version mismatch: discovery failed before launch; consumer tests permitted one
  recorded direct-launch fallback.
- Report or persistence failure: native ENOSPC and EIO tests kept enforcement active and returned the
  partial-artifact failure outcome; consumer tests prohibited a second worker launch.

## Integration friction

The first mlx-guard release is not yet available to dependency resolution, so the consumer uses a
lazy optional import instead of declaring a package extra. Users install mlx-guard separately. Native
reports require a private owner-controlled directory; the runner now creates `_mlx_guard/` with mode
0700. No consumer-specific exception, status, condition kind, or policy rule was added to mlx-guard
core. The consumer's existing wired limit and active-memory watchdog remain the direct-launch
fallback and also stay active inside supervised workers.
