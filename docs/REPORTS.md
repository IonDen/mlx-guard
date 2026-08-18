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
`error`. Missing or failed observations are never encoded as zero. A child that exits before its
identity can be inspected produces a valid report with zero samples, `unknown` final footprint,
and the child's actual terminal status — not a supervisor failure. Artifact errors use fixed codes;
they do not include a path or raw operating-system message. Checkpoint request delivery is
`requested_unverified`. A nonce- and request-matching worker response is
`acknowledged_unverified_durability`, which still does not prove durable bytes.

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

The successful journal is retained as durable recovery evidence. A report path is therefore
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
