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

## Wire format

Every frame starts with a four-byte big-endian body length. Bodies are at most 128 bytes and contain
the `MGCP` magic, protocol version `1`, and a message kind. The hello/ready exchange binds a random
32-byte per-run nonce. A request adds an independently random nonzero initial request ID and monotonic
deadline. An acknowledgement must repeat both values and carries one worker status: completed,
failed, or cancelled. Randomizing the first ID prevents a worker from pre-queuing a valid request-1
acknowledgement during negotiation.

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

The echoed request ID, the intervention reason behind the request, and the acknowledgement's
artifact facts now land in the report's `checkpoint` record alongside `status` and `at_ms`. The
wire is unchanged: this is the same bounded, path-free frame described above, projected into the
persisted copy rather than a new exchange. Paths and names have no wire representation, so they
have no report representation either — only `kind` (`file`, `directory`, or `opaque`) and an
optional byte count ever reach `checkpoint.artifact`. As with the live acknowledgement, the
persisted copy is the worker's report, not independent proof that artifact bytes are complete or
durable.
