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

The report records escape and measurement-quality evidence where possible; it does not claim that a
missing observation proves containment. See `IDENTITY_AND_CONTAINMENT.md`, `SAMPLING.md`, and
`REPORTS.md` for the exact boundaries.
