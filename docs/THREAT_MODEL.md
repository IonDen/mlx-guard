# Threat model

## Protected operation

`mlx-guard` reduces the impact of an accidental runaway in one trusted same-user command. It owns a
new process group, periodically measures members visible through public macOS interfaces, applies an
explicit policy, records bounded evidence, and signals validated group members. Artifact handling
defends against symlink substitution, unsafe permissions, PID reuse, and partial writes.

## Trust assumptions

The invoking user, supervisor binary, Python environment, kernel, and macOS accounting interfaces
are trusted. The supervised program may fail, leak memory, hang, fork, or ignore TERM, but it is not
assumed to be actively hostile. Report paths point into an existing invoking-user-owned `0700`
directory on a trusted local filesystem.

Checkpoint acknowledgement trusts possession of the inherited descriptor plus the plaintext
per-run nonce. A pre-connect descendant or a fork-without-exec can retain that capability. The
protocol prevents accidental cross-run/replay confusion; it does not prove which trusted descendant
performed the callback.

## Out of scope

v0.1 does not defend against:

- a malicious same-user child that escapes its process group, races observation, tampers with the
  caller's files, or attacks the supervisor;
- root, kernel, hypervisor, driver, hardware, or system-wide failures;
- atomic enforcement of unified memory or immediate Metal-driver reclamation;
- availability loss between samples, before launch completes, or while durable storage fails;
- prompt, model, dataset, checkpoint, or command-output confidentiality outside the guard report.

One driver failure deserves its signature here because it can fire with the supervised process's
footprint well inside any limit, so a clean report is not evidence that the tool would have
prevented it: on macOS 26.4 and later, Metal workloads can panic the kernel inside Apple's IOGPU
extension (`IOGPUMemory.cpp:550 completeMemory() prepare count underflow`, also
`IOGPUGroupMemory.cpp:219`), reported against MLX inference
([mlx #3346](https://github.com/ml-explore/mlx/issues/3346),
[mlx #3186](https://github.com/ml-explore/mlx/issues/3186)), distributed inference
([exo #1972](https://github.com/exo-explore/exo/issues/1972)), and a serving stack
([oMLX #557](https://github.com/jundot/omlx/issues/557)); reporters state the only mitigation is
not using the GPU. No userland supervisor reaches it. The same panic line also appears under
sustained over-allocation, which a footprint limit does address, so the signature alone does not
say which case fired. The [compatibility matrix](COMPATIBILITY.md) carries the same row.

The report records escape and measurement-quality evidence where possible; it does not claim that a
missing observation proves containment. See `IDENTITY_AND_CONTAINMENT.md`, `SAMPLING.md`, and
`REPORTS.md` for the exact boundaries.
