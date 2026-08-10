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
`error`. Missing or failed observations are never encoded as zero. Artifact errors use fixed codes;
they do not include a path or raw operating-system message. Checkpoint request delivery is
`requested_unverified`. An authenticated worker response is
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

The native writer must create report and journal files with mode `0600` and must not weaken an
existing file's permissions. The chosen parent directory must be owned by the invoking user, must
not be a symlink, and should use mode `0700`. Persistence work must reject unsafe ownership or link
conditions before command launch.

Retention is user-managed. mlx-guard does not upload reports, contact a telemetry service, or delete
old reports automatically. A future upload or retention feature requires a new explicit contract;
schema-v1 defaults always record `upload: disabled` and `retention: user_managed`.

## Validation

`ReportV1::validate` rejects weakened privacy assertions, malformed identities or hashes, invalid
configuration, invalid signal numbers, reversed sample clocks, unordered records, impossible
checkpoint fields, inconsistent advisory metadata, and an outcome timestamp earlier than recorded
activity. `to_json_pretty` validates before serialization. `from_json` validates after parsing and
strips unknown fields when the typed report is serialized again.
