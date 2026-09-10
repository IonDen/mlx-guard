# Checkpoint protocol

Checkpointing is optional and cooperative. The supervisor creates a Unix socketpair and explicitly
inherits one endpoint into the root command. The child receives only the reserved descriptor number
in `MLX_GUARD_CHECKPOINT_FD`; callers cannot override that key. Standard output and error remain
application data and are never parsed as control messages.

## Negotiation and signal safety

Before enforcement begins, the supervisor sends a versioned hello on the descriptor. The worker
installs its checkpoint handler and returns the matching ready frame on the same descriptor. A
checkpoint request is refused until this FD-only handshake completes. This matters because the
default action for `SIGUSR1` and `SIGUSR2` is process termination on supported hosts.

Only `SIGUSR1` or `SIGUSR2` can be configured for checkpoint delivery. `SIGINT`, `SIGTERM`, `SIGKILL`,
job-control signals, and zero are rejected as collisions. The signal is sent only to the negotiated
live root or cooperative endpoint; TERM and KILL continue to target the validated owned group.

Delivery revalidates the endpoint's exact `(pid, start token)` immediately before signalling, so a
PID recycled inside the owned group between negotiation and delivery is refused as an invalid
checkpoint endpoint instead of receiving the signal — the same identity discipline every other direct
signal in this supervisor already followed. Through the CLI the checkpoint endpoint is always the
root process, held unreaped by the supervisor for the whole run, so its PID cannot be recycled there.
This is a contract-conformance fix, not a live PID-reuse hole in the CLI: it matters to the core
library API, where an embedder can negotiate an endpoint for a non-root member of the owned group.
Inspection and signalling remain two separate steps, and macOS offers no `pidfd`-equivalent primitive
to bind them atomically, so a PID reused inside that sub-millisecond window is still not detectable —
see [identity and containment](IDENTITY_AND_CONTAINMENT.md).

An endpoint whose process has already exited is refused earlier still, at the group membership
check, because macOS reports no process group for an exited process. The identity check narrows the
remaining window to the interval between inspection and the system call: an endpoint that exits
before that inspection is reported as `process_missing` rather than signalled, since delivery to an
exited process can otherwise succeed at the system call and wait out the whole checkpoint timeout
for an acknowledgement that cannot arrive. A failure in the identity check itself — unreadable or
unsupported process metadata, most likely under exactly the memory pressure this supervisor exists
to police — now also refuses the request rather than falling through to a plain signal call. All
three cases — refusal at the membership check, `process_missing` inside the narrower window, and a
failed identity check — escalate straight to termination. Permission denial can now surface from
either the inspection or the signal itself, and both are reported the same way.

## Wire format

Every frame starts with a four-byte big-endian body length. Bodies are at most 128 bytes and contain
the `MGCP` magic, protocol version `1`, and a message kind. The hello/ready exchange binds a random
32-byte per-run nonce. A request adds an independently random nonzero initial request ID and monotonic
deadline. An acknowledgement must repeat both values and carries one worker status: completed,
failed, or cancelled. Randomizing the first ID prevents a worker from pre-queuing a valid request-1
acknowledgement during negotiation.

Byte offsets within a body, all integers big-endian:

| Body | Length | Offsets |
|---|---:|---|
| hello (kind 3) and ready (kind 4) | 38 | magic `MGCP` 0–3, version `1` at 4, kind at 5, nonce 6–37 |
| request (kind 1) | 54 | as above, then request id 38–45, deadline in nanoseconds since the run epoch 46–53 |
| acknowledgement (kind 2) | 57 | as above through the request id, then status at 46 (1 completed, 2 failed, 3 cancelled), artifact kind at 47 (0 none, 1 file, 2 directory, 3 opaque), has-size at 48, size 49–56 |

A zero request id or deadline is malformed. The layout is pinned by a golden-frame test in the
core crate; a change to any offset or kind byte is a new protocol version.

Optional artifact metadata is deliberately path-free: only `file`, `directory`, or `opaque`, plus an
optional byte count. Names, paths, argv, environment values, model IDs, prompts, and worker output
have no wire representation.

## Validation and deadlines

The supervisor side is nonblocking and performs one bounded descriptor read per poll. Partial frames
remain bounded until another poll, endpoint exit, cancellation, or deadline. Wrong nonces, replayed
request IDs, duplicates, malformed or oversized frames, partial EOF, acknowledgements at or after the
deadline, and post-exit data never authenticate success.
Data received after explicit cancellation is classified separately as `post_cancel` diagnostic
input rather than malformed protocol data.

Signal delivery records `requested_unverified`. Only a matching completed acknowledgement records
`acknowledged_unverified_durability`; it is the worker's report, not independent proof that artifact
bytes are complete or durable. Failed, cancelled, missing, or late responses cannot extend the
checkpoint deadline.

This is possession-bound cooperation, not authentication against hostile supervised code. The
nonce is plaintext on the inherited channel. A descendant that inherits the descriptor before the
worker connects can observe or answer the exchange; a fork without exec can retain a connected
endpoint. Normal exec closes the descriptor after the Python helper re-arms close-on-exec.

## Persisted acknowledgement facts

The acknowledgement's echoed request ID and artifact facts now land in the report's `checkpoint`
record alongside `status` and `at_ms`. The wire is unchanged: this is the same bounded, path-free
frame described above, projected into the persisted copy rather than a new exchange. Paths and
names have no wire representation, so they have no report representation either — only `kind`
(`file`, `directory`, or `opaque`) and an optional byte count ever reach `checkpoint.artifact`. As
with the live acknowledgement, the persisted `request_id` and `artifact` are the worker's report,
not independent proof that artifact bytes are complete or durable.

`checkpoint.reason` is a different kind of fact: it is the supervisor's own record of why it
attempted a checkpoint (`footprint` or `wall_time`), latched locally whenever a checkpoint
actuation was executed. It is never echoed by the worker and never carried on the wire, so it can
be present even when no frame was exchanged at all — for example under `not_negotiated`, when the
attempt targeted a channel whose handshake never completed.
