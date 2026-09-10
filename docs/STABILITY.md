# Stability

This table says, for each surface a user or integrator can depend on, what may still change before
1.0 and what the 1.0 release freezes. "Additive" means a new optional flag, field, or value with a
CHANGELOG entry and no change to existing behaviour; anything else is a breaking change and is
called out in the CHANGELOG under Changed. Versions follow Semantic Versioning once 1.0 ships;
until then a minor release may carry the breaking changes the table allows.

| Surface | Before 1.0 | At 1.0 |
|---|---|---|
| CLI grammar and flags (`observe`, `run`, unit grammar, the `--` separator) | Additive only: new optional flags with a CHANGELOG entry. Existing flags keep their meaning and defaults, except where the defaults row says otherwise. | Frozen. |
| Exit codes (child status preserved, `128+n`, 64, 70, 74, 75, 126, 127) | Frozen. The 0.2 line changed which code wins when several apply (70 outranks 75; a parent exit under the default policy reports 75) and those precedence rules are now part of the contract. | Frozen. |
| Report schema v1 (`docs/REPORTS.md`) | New fields are additive, optional, and ignored by older readers. A new `outcome.kind` value is a breaking change for the Python reader, which rejects unknown kinds, and is announced as one. | Frozen; a breaking change becomes schema v2. |
| Journal (`.<report>.journal`) | Byte format versioned by its magic header; it may change between releases. Recovery is promised for a journal written by the same release. | Decided at 1.0: either frozen or kept explicitly release-scoped. |
| Checkpoint frame protocol (`docs/CHECKPOINT_PROTOCOL.md`) | Version 1 layout is documented to the byte and pinned by a golden test. Additive optional payloads only; any offset or kind change is a new protocol version negotiated by the version byte. | Frozen. |
| Python API (`mlx_guard`: `ObserveConfig`, `RunConfig`, `run`, `observe`, `start`, `load_report`, `CheckpointWorker`) | Configs are keyword-only from 0.2; new fields are optional with defaults. Field names are part of the API, field order is not. Removing or renaming a public name is breaking. | Frozen. |
| Rust crates (`mlx-guard-core`, `mlx-guard-cli`) | Internal. Neither crate is published to a registry (`publish = false`); their types may change in any release without notice. Build the CLI or use the Python package. | Internal. |
| Contract documents (`docs/*.md`) | Describe the shipped behaviour of the release they ship with; a behaviour change updates the matching document in the same change. | Same rule; the documents are the contract. |
| Defaults | Listed below with the release that set them. A default may change before 1.0 with a CHANGELOG entry and the flag that restores the old value. | Frozen. |
| Published measurement bounds (`evidence/`) | Not a contract. They describe the host and commit that produced them, and the release checklist refreshes them when supervision code changes. | Not a contract. |

## Defaults and the release that set them

| Default | Value | Since |
|---|---|---|
| `--sample-interval` | 50 ms (accepted range 10 ms to 10 s) | 0.1 |
| Consecutive unusable samples before failing closed | 3 | 0.1 |
| Consecutive over-limit samples before intervention | 2 | 0.1 |
| Emergency KILL band above the limit | 10 % of the limit | 0.1 |
| TERM to KILL grace | 1 s | 0.1 |
| `--checkpoint-timeout` | 1 s (accepted range 10 ms to 60 s); was 100 ms | 0.2 |
| `--on-parent-exit` | `terminate` | 0.2 |
| Sample collection window and age floors | 250 ms and 500 ms, independent of the interval | 0.2 |
| Report sample ring | 4,096 most recent windows | 0.1 |
| Retained escape evidence | 64 identities; the count keeps rising | 0.2 |
| `--wall-time` maximum | 30 days | 0.1 |
