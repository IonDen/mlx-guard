# Report and privacy contract

Every v0.1 result uses JSON schema major `1`. The package version records which writer produced the
file; it does not change the schema meaning. A schema-v1 reader accepts unknown fields so newer v1
writers can add data. It rejects every other schema major instead of guessing. The committed golden
file at `crates/mlx-guard-core/tests/fixtures/report-v1.json` freezes names, enum values, structure,
and field order.

## Recorded data

The top-level report contains:

- a random run ID, executable basename, argument count, and optional correlation hash;
- pre-launch capability results and normalized non-secret policy configuration;
- bounded sample windows, advisory metrics, policy transitions, and signal attempts;
- checkpoint status, containment-escape evidence, and redacted artifact errors;
- a typed terminal outcome, final footprint availability, and privacy assertions.

Observed values use one of five tagged states: `available`, `unknown`, `unavailable`, `stale`, or
`error`. Missing or failed observations are never encoded as zero. A child that exits before
supervision begins — before its identity can be inspected, or before the checkpoint endpoint is
negotiated — produces a valid report with zero samples, `unknown` final footprint, and the child's
actual terminal status — not a supervisor failure. Artifact errors use fixed codes;
they do not include a path or raw operating-system message. Checkpoint request delivery is
`requested_unverified`. A nonce- and request-matching worker response is
`acknowledged_unverified_durability`, which still does not prove durable bytes.

`escape.escaped_count` is optional and present only when nonzero: at least one increment per
distinct escaped identity observed during the run, independent of the bounded in-memory evidence
list capped at 64 identities, so an over-cap identity keeps being counted rather than silently
dropped once the list is full. Above that cap the counted mark lives only on the currently tracked
set, so an identity a sample misses and later re-observes can add a further increment — read the
field as bounded evidence of distinct escapes, not an exact census. It is a count, never a list: the
escaped identities' pids are never persisted.

`checkpoint.request_id`, `checkpoint.reason`, and `checkpoint.artifact` are optional and let a
resuming consumer correlate the report with the worker's own saved state. `request_id` is the
nonzero id the supervisor sent and, on acknowledgement, the worker echoed — a worker tags its saved
state with this id so a later reader can find it. `reason` is the intervention cause behind the
checkpoint attempt: `footprint` or `wall_time`, the only causes that can request one. `artifact` is
the worker's own path-free claim about what it saved (`kind`: `file`, `directory`, or `opaque`, plus
an optional `size_bytes`) — the worker's report, not independent proof; mlx-guard never verifies it.
All three fields are absent from reports written before they existed.

Their presence follows the checkpoint status:

| status | `request_id` | `reason` | `artifact` |
|---|---|---|---|
| `not_negotiated` | absent | present only when a checkpoint was attempted toward a non-negotiated channel, otherwise absent | absent |
| `requested_unverified` / `timed_out` | present | present | absent |
| `acknowledged_unverified_durability` | present | present | present when the worker's acknowledgement supplied one |
| `cancelled` | absent (delivery failed; the worker was never signalled) | present | absent |

Validation only constrains fields that are present, never their absence, so every report written
before these fields existed keeps parsing: a present `request_id` must be nonzero and its status
must be one where a frame was actually delivered (`requested_unverified`,
`acknowledged_unverified_durability`, or `timed_out`); a present `artifact` requires
`acknowledged_unverified_durability`; a present `reason` must be `footprint` or `wall_time`.

`outcome.child_status` records the root command's own exit code or signal whenever it was observed,
under every outcome kind, so an intervention or supervisor failure never hides how the command
ended. `outcome.owned_group_survivors` distinguishes two situations and its encoding is not
symmetric between them. At a natural root exit, it is `true` when observe ended with owned-group
members still running (they are not signalled) and is absent — never an explicit `false` — when none
were left. At a parent-exit shutdown (below), this explicit encoding belongs to `observe`'s own
completion: `Some(true)` when the group was still alive at the KILL decision, `Some(false)` when the
TERM alone was enough. `run`'s terminal outcome leaves the field absent at a parent-exit shutdown just
as it does everywhere else. A reader must treat the absent case and an explicit `false` as different
facts. Each signal record carries `reason`:
`footprint`, `wall_time`, `external_signal`, `root_exit_cleanup`, `parent_exit`,
`observation_failure`, or `supervisor_fault`. All three fields are absent from reports written before
they existed.

`configuration.on_parent_exit` records the requested behavior when the process that launched
`mlx-guard` exits: `terminate` or `detach`. `configuration.parent_watch` records whether, and how,
the run watched for that exit — `active` means watching and enforcing, `detach` means watching for
evidence only, and `parent_is_launchd`, `hangup_ignored`, and `parent_unobservable` all mean the
watch was never checked (the launch-time parent was already `launchd`, SIGHUP was already disposed
to `SIG_IGN` the `nohup` way, or the parent's identity could not be established, respectively).
`outcome.parent_exited_at_ms` is the time the launching parent was first confirmed gone; it is
present only when `parent_watch` was `active` or `detach`. The launching parent's exact `(pid,
start_abstime)` identity is never persisted — only the watch state and this timestamp are. All three
fields are absent from reports written before they existed.

Validation enforces this pairing in both directions. A `detach` watch requires the `detach` option,
and a `detach` option requires a watch of `detach` or `parent_unobservable` — the only two watch
states a `detach` run can record. A `detach` option paired with any other watch, including an
absent one, is rejected as an invalid configuration, reducing the risk of a report whose
parent-exit fields disagree with each other.

Observe reports can now carry outcome `policy_intervention`. A forwarded terminal signal (SIGHUP,
SIGINT, SIGTERM) reaches the owned group unchanged, with no grace timer; observe keeps sampling and
the run ends when the root exits on its own, reporting the root's own signaled status (`128+n`), not
a policy intervention. Under the default `terminate` behavior, a launching parent's exit ends
observation a different way: TERM to the owned group, a one-second grace, KILL if it is still alive,
and the run is reported as an intervention (`policy_intervention`, exit 75) rather than a root exit.
During that gated shutdown window observe's own measurement-quality policy machine keeps running
independently, and a `SupervisorError` transition record and a final `policy_intervention` outcome
can both appear in the same report — both facts are true; the in-flight parent-exit intervention owns
the outcome, and the transition is only evidence of what the sampler saw while it was in flight.

An observe report also carries a `calibration` section: total, complete, and incomplete sample
counts; the observed duration; the highest complete aggregate footprint; and the highest positive
growth rate. It always records `observation_only: true`, `safety_certified: false`, and no automatic
limit. Its peak is an unbounded running maximum, so it holds the whole run's highest footprint even
after the 4,096-sample history ring has evicted the sample it came from — the number a limit is
chosen from. The section is present only on observe reports; it is absent from enforcing runs and
from reports written before this release, and validation accepts a report without it. When present,
it must be an observe report and the artifact's own invariants must hold.

Advisory values retain their original schema-v1 fields. New writers may also add `pressure_level`
and per-field `metadata` with the metric scope, public API source, observation timestamp, and
freshness. Readers remain compatible with earlier schema-v1 reports where those additive fields are
absent. Validation rejects metadata whose source, scope, freshness, or timestamp contradicts the
value it describes.

## Default redaction

Redaction happens in memory before JSON reaches the persistence layer. Schema v1 has no fields for
environment values, raw argv, absolute paths, prompts, model IDs, tokens, or child output. The
executable basename and argument count are the only default command identity. A non-UTF-8 basename
becomes the fixed string `<non-utf8>`; its original bytes are not copied.

Seeded property tests drive the argument-to-basename projection against a committed adversarial
corpus, and real-process tests plant a marker in every launch channel — arguments, environment,
working directory, executable path, and process output — then scan the persisted report and
journal for survival.

Raw sensitive capture is not available, even as an opt-in, in v0.1. The only explicit capture
option is a correlation hash with the form `sha256:` followed by 64 lowercase hexadecimal digits.
Hash a random, non-secret correlation identifier. Do not hash a path, token, prompt, model ID, or
other low-entropy secret because a digest does not make such input safely anonymous.

## Persistence and ownership

The native writer requires an invoking-user-owned `0700` directory. Before worker launch, it checks
any existing report and exclusively creates `.<report-name>.journal` with mode `0600`. Directory and
file lookups stay anchored to the validated directory descriptor. The writer rejects symlinks,
unexpected file types, foreign ownership, and broader permissions without changing those targets.

The journal starts with the version-1 magic header. Each record has a bounded length, sequence
number, JSON payload, and CRC-32 checksum. Records contain only schema-v1 types, so redaction occurs
before the first write. Headers, policy transitions, signal attempts, checkpoint states, and
terminal outcomes require a file sync. This requirement makes the transition durable before the
runtime can record its related signal. Initialization syncs the new journal before syncing its
directory.

Recovery returns the valid prefix and labels the stream `complete`, `truncated`, or `corrupt`.
Only a complete stream with one first header, one final outcome, and the required report components
can become a final report. Parsers treat all journal and JSON fields as data. They never pass content
to a shell or executable.

Finalization replays the journal into schema-v1 JSON, writes `.<report-name>.tmp` as `0600`, syncs
it, renames it over a safe report target, then syncs the directory. A failed write or rename removes
only the temporary file created by that attempt. The caller receives no success result until this
sequence finishes.

The successful journal is retained as durable recovery evidence. Its byte format is versioned by
its magic header and may change between releases; recovery is promised only for a journal written
by the same release, while the JSON report it produces follows schema v1. A report path is therefore
single-use while `.<report-name>.journal` exists. Choose a unique report name for each run, or
explicitly archive/remove both files after reviewing them. The Python client raises
`ReportPathInUseError` before launch when it sees the retained journal; the native CLI rejects the
same target during exclusive journal creation. Neither path interprets an older report as the new
run's result.

Secure journal initialization is mandatory. A later write or sync failure disables further
persistence but does not stop TERM, KILL, cleanup, or observation work. The runtime prints a
path-free error, suppresses any complete-report claim, and returns the contracted partial artifact
failure status (exit `74`).

Retention is user-managed. mlx-guard does not upload reports, contact a telemetry service, or delete
old reports or journals automatically. A future upload or retention feature requires a new explicit
contract;
schema-v1 defaults always record `upload: disabled` and `retention: user_managed`.

## Validation

`ReportV1::validate` rejects weakened privacy assertions, malformed identities or hashes, invalid
configuration, invalid signal numbers, reversed sample clocks, unordered records, impossible
checkpoint fields, inconsistent advisory metadata, and an outcome timestamp earlier than recorded
activity. `to_json_pretty` validates before serialization. `from_json` validates after parsing and
strips unknown fields when the typed report is serialized again.
