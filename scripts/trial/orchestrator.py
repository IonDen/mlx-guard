"""Runs one arm per subprocess, records its report/result on disk, and skips arms already done.

Each arm lives under ``<trial_root>/arms/<arm_id>/``: one ``attempt-<n>`` directory per attempt,
holding ``report.json`` (or, for a Python-launched arm, ``reports/<arm_id>.json``), the captured
``stdout.log``/``stderr.log``, a redacted ``command.txt``, and ``result.json``. The arm as a whole
is "done" only once a bare ``status`` file exists next to the attempt directories — written by
``mark_done`` after ``result.json`` is on disk, never before, so a crash between validating and
recording never lands a done arm with no evidence, and a failed-shape attempt (which never reaches
``mark_done``) is retried into a fresh attempt directory rather than skipped forever.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any, Literal

import bands
import validators as v

Mode = Literal["observe", "run", "python", "probe"]


@dataclass(frozen=True, slots=True)
class ArmSpec:
    """One row of the trial matrix: how to launch an arm and how to judge its report."""

    arm_id: str
    mode: Mode
    argv: tuple[str, ...]
    sample_interval_ms: int
    limit_bytes: int | None
    wall_time_ms: int | None
    validator: Callable[[Mapping[str, Any]], None]
    notes: str


@dataclass(frozen=True, slots=True)
class ArmResult:
    """What one ``run_arm`` call learned, whether or not it actually launched anything."""

    arm_id: str
    attempt: Path | None
    dry_run: bool
    skipped: bool
    exit_code: int | None = None
    duration_s: float | None = None
    validator_ok: bool | None = None
    validator_error: str | None = None
    peak_bytes: int | None = None
    sample_count: int | None = None
    ring_wrapped: bool | None = None
    self_consistency_band: str | None = None
    observed_band: str | None = None
    report_path: str | None = None
    predicted_band: str | None = None
    band_mismatch: bool | None = None


class VersionPinError(RuntimeError):
    """A later arm's resolved tool version differs from what an earlier arm in the family pinned."""


class DerivationError(RuntimeError):
    """A ``--derive-from`` request cannot be met: an undone source arm, or a conflicting flag."""


def next_attempt_dir(arm_root: Path) -> Path:
    """Return ``arms/<id>/attempt-<n>``, one past the number of existing attempt directories."""
    existing = [p for p in arm_root.glob("attempt-*") if p.is_dir()]
    return arm_root / f"attempt-{len(existing) + 1}"


def mark_done(arm_root: Path, attempt: Path) -> None:
    """Record ``attempt`` as the arm's accepted evidence; refuse if ``result.json`` is missing."""
    if not (attempt / "result.json").exists():
        raise FileNotFoundError(f"cannot mark done: {attempt / 'result.json'} does not exist")
    arm_root.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(dir=arm_root, prefix=".status-")
    try:
        with os.fdopen(fd, "w") as handle:
            handle.write(attempt.name + "\n")
        os.replace(tmp_name, arm_root / "status")
    except BaseException:
        Path(tmp_name).unlink(missing_ok=True)
        raise


def is_done(arm_root: Path) -> bool:
    """Return True once ``status`` exists — the only marker of a validated, retained attempt."""
    return (arm_root / "status").exists()


def record_version(trial_root: Path, family: str, version: str) -> None:
    """Persist the tool-version triple ``family``'s earlier arm resolved.

    A later arm in the same family checks its own resolved version against this.
    """
    path = _version_pin_path(trial_root, family)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(version + "\n")


def check_version(trial_root: Path, family: str, version: str) -> None:
    """Raise ``VersionPinError`` when ``version`` differs from a pin ``family``'s earlier arm made.

    A no-op when no pin has been recorded yet — this call itself records the first one.
    """
    path = _version_pin_path(trial_root, family)
    if not path.exists():
        record_version(trial_root, family, version)
        return
    pinned = path.read_text().strip()
    if pinned != version:
        raise VersionPinError(
            f"{family}: an earlier arm calibrated against {pinned!r}, this arm resolved {version!r}"
        )


def _version_pin_path(trial_root: Path, family: str) -> Path:
    return trial_root / "arms" / f".{family}.version-pin"


def _duration_flag(ms: int) -> str:
    return f"{ms}ms"


def _byte_flag(n: int) -> str:
    return f"{n}B"


def _build_binary_argv(spec: ArmSpec, binary: Path, attempt: Path) -> tuple[list[str], Path]:
    """Build the `<binary> observe|run [flags] --report PATH -- <argv>` argv (docs/CLI.md)."""
    report_path = attempt / "report.json"
    argv: list[str] = [str(binary), spec.mode]
    if spec.mode == "run":
        limit_display = _byte_flag(spec.limit_bytes) if spec.limit_bytes is not None else "<limit>"
        argv += ["--max-footprint", limit_display]
        if spec.wall_time_ms is not None:
            argv += ["--wall-time", _duration_flag(spec.wall_time_ms)]
    argv += ["--sample-interval", _duration_flag(spec.sample_interval_ms)]
    argv += ["--report", str(report_path)]
    argv += ["--", *spec.argv]
    return argv, report_path


def _redact_argv(argv: Sequence[str], attempt: Path, binary: Path) -> list[str]:
    """Basenames for the binary, attempt-relative paths for anything under it — no home paths."""
    out: list[str] = []
    for token in argv:
        if token == str(binary):
            out.append(binary.name)
            continue
        if token.startswith("/"):
            candidate = Path(token)
            try:
                out.append(str(candidate.relative_to(attempt)))
                continue
            except ValueError:
                out.append(candidate.name)
                continue
        out.append(token)
    return out


def _derive_metrics(
    report: Mapping[str, Any], limit_bytes: int | None, predicted_band: str | None = None
) -> dict[str, Any]:
    """Derive result.json's report-shape metrics.

    ``self_consistency_band`` is a same-run sanity check only: ``bands.predict_band`` applied to
    this report's own samples against its own ``limit_bytes`` — it answers "does this run's own
    trajectory look like it would predict its own outcome", not "did the run match what an earlier
    arm's samples predicted". The latter (the real ahead-of-launch prediction used for L3/F3) is
    ``predicted_band``, supplied by the caller from ``prediction.json`` via ``--derive-from``; when
    given, ``band_mismatch`` records whether the observed band differs from it.
    """
    metrics: dict[str, Any] = {
        "peak_bytes": None,
        "sample_count": len(report.get("samples", [])),
        "ring_wrapped": v.ring_wrapped(report),
        "observed_band": v.observed_band(report),
        "self_consistency_band": None,
        "predicted_band": predicted_band,
        "band_mismatch": None,
    }
    with contextlib.suppress(v.ShapeError):
        metrics["peak_bytes"] = v.peak(report)
    if limit_bytes is not None:
        values = v.available_values(report)
        if values:
            metrics["self_consistency_band"] = bands.predict_band(values, limit_bytes)
    if predicted_band is not None:
        metrics["band_mismatch"] = metrics["observed_band"] != predicted_band
    return metrics


def _result_to_json(result: ArmResult, attempt: Path) -> dict[str, Any]:
    return {
        "arm_id": result.arm_id,
        "attempt": attempt.name,
        "report_path": result.report_path,
        "exit_code": result.exit_code,
        "duration_s": result.duration_s,
        "validator_ok": result.validator_ok,
        "validator_error": result.validator_error,
        "peak_bytes": result.peak_bytes,
        "sample_count": result.sample_count,
        "ring_wrapped": result.ring_wrapped,
        "self_consistency_band": result.self_consistency_band,
        "observed_band": result.observed_band,
        "predicted_band": result.predicted_band,
        "band_mismatch": result.band_mismatch,
    }


def run_arm(
    spec: ArmSpec,
    trial_root: Path,
    binary: Path,
    *,
    dry_run: bool,
    predicted_band: str | None = None,
) -> ArmResult:
    """Launch one arm (or print its argv under ``dry_run``), then validate and record its result.

    ``predicted_band`` is the ahead-of-launch prediction from ``--derive-from`` (graceful mode
    only). When given, a validator failure caused solely by the observed band differing from the
    prediction is recorded as ``band_mismatch: true`` rather than a hard failure — the evidence is
    still accepted as long as the report matches *some* real band shape.
    """
    arm_root = trial_root / "arms" / spec.arm_id
    if is_done(arm_root):
        return ArmResult(spec.arm_id, attempt=None, dry_run=dry_run, skipped=True)

    attempt = next_attempt_dir(arm_root)

    if spec.mode == "probe":
        return _run_probe_arm(spec, arm_root, attempt, binary, dry_run=dry_run)
    if spec.mode == "python":
        argv = list(spec.argv)
        report_path = attempt / "reports" / f"{spec.arm_id.lower()}.json"
    else:
        argv, report_path = _build_binary_argv(spec, binary, attempt)

    display = _redact_argv(argv, attempt, binary)
    if dry_run:
        print(f"[{spec.arm_id}] {' '.join(display)}")
        return ArmResult(spec.arm_id, attempt=None, dry_run=True, skipped=False)

    if spec.mode == "run" and spec.limit_bytes is None:
        raise ValueError(f"{spec.arm_id}: mode 'run' requires a resolved limit_bytes before launch")

    attempt.mkdir(mode=0o700, parents=True)
    if spec.mode == "python":
        (attempt / "reports").mkdir(mode=0o700, exist_ok=True)
    (attempt / "command.txt").write_text(" ".join(display) + "\n")

    started = time.monotonic()
    with (
        (attempt / "stdout.log").open("wb") as out_log,
        (attempt / "stderr.log").open("wb") as err_log,
    ):
        proc = subprocess.run(  # noqa: S603 — argv is a literal tuple from arms.py, never a shell string
            argv,
            cwd=attempt,
            stdin=subprocess.DEVNULL,
            stdout=out_log,
            stderr=err_log,
            check=False,
        )
    duration_s = time.monotonic() - started

    report: Mapping[str, Any] | None = None
    validator_error: str | None = None
    if report_path.exists():
        try:
            report = json.loads(report_path.read_text())
            spec.validator(report)
        except (json.JSONDecodeError, v.ShapeError, KeyError, TypeError) as exc:
            validator_error = f"{type(exc).__name__}: {exc}"
    else:
        validator_error = f"no report at {report_path.relative_to(attempt)}"

    metrics: dict[str, Any] = {
        "peak_bytes": None,
        "sample_count": None,
        "ring_wrapped": None,
        "observed_band": None,
        "self_consistency_band": None,
        "predicted_band": predicted_band,
        "band_mismatch": None,
    }
    if report is not None:
        with contextlib.suppress(KeyError, TypeError):
            metrics = _derive_metrics(report, spec.limit_bytes, predicted_band)
        if validator_error is not None and metrics.get("band_mismatch"):
            # The validator was chosen from the PREDICTED band; a mismatch means it just
            # asserted the wrong shape, not that the shape itself is bad. Confirm the report is a
            # valid instance of whatever band actually happened before accepting it as evidence.
            observed = metrics.get("observed_band")
            fallback = (
                v.assert_graceful_footprint
                if observed == "graceful"
                else v.assert_emergency_footprint
                if observed == "emergency"
                else None
            )
            if fallback is not None:
                try:
                    fallback(report)
                    validator_error = None
                except v.ShapeError as exc:
                    validator_error = (
                        f"{type(exc).__name__}: {exc} (also failed the observed-band check)"
                    )

    result = ArmResult(
        arm_id=spec.arm_id,
        attempt=attempt,
        dry_run=False,
        skipped=False,
        exit_code=proc.returncode,
        duration_s=duration_s,
        validator_ok=validator_error is None,
        validator_error=validator_error,
        report_path=str(report_path.relative_to(attempt)),
        **metrics,
    )
    (attempt / "result.json").write_text(
        json.dumps(_result_to_json(result, attempt), indent=2) + "\n"
    )
    if validator_error is None:
        mark_done(arm_root, attempt)
    return result


def _run_probe_arm(
    spec: ArmSpec, arm_root: Path, attempt: Path, binary: Path, *, dry_run: bool
) -> ArmResult:
    """Mode ``probe``: delegate to ``tty_probe.run_probe`` and validate its dict shape."""
    import tty_probe  # deferred: built in Task 1 Step 5, after the orchestrator

    if dry_run:
        print(f"[{spec.arm_id}] tty_probe.run_probe({binary}, <attempt>, {spec.argv!r})")
        return ArmResult(spec.arm_id, attempt=None, dry_run=True, skipped=False)

    attempt.mkdir(mode=0o700, parents=True)
    started = time.monotonic()
    probe = tty_probe.run_probe(binary, attempt, spec.argv)
    duration_s = time.monotonic() - started

    validator_error: str | None = None
    try:
        spec.validator(probe)
    except v.ShapeError as exc:
        validator_error = f"{type(exc).__name__}: {exc}"

    (attempt / "tty").mkdir(mode=0o700, exist_ok=True)
    (attempt / "tty" / "t0.json").write_text(json.dumps(probe, indent=2) + "\n")

    remedy_exit = probe.get("remedy_exit")
    result = ArmResult(
        arm_id=spec.arm_id,
        attempt=attempt,
        dry_run=False,
        skipped=False,
        exit_code=remedy_exit if isinstance(remedy_exit, int) else None,
        duration_s=duration_s,
        validator_ok=validator_error is None,
        validator_error=validator_error,
    )
    (attempt / "result.json").write_text(
        json.dumps(_result_to_json(result, attempt), indent=2) + "\n"
    )
    if validator_error is None:
        mark_done(arm_root, attempt)
    return result


def assemble(trial_root: Path, bundle_dir: Path) -> list[Path]:
    """Collect every done arm's report, journal, and small markers into ``bundle_dir``.

    Copies exactly: each done arm's report (found via its own ``result.json``'s ``report_path`` —
    ``report.json`` for observe/run arms, ``reports/<arm>.json`` for a Python-launched arm like
    C1/C2 — one source of truth, not a hardcoded guess) to ``reports/<arm>.json``, its retained
    journal (if any) to ``reports/.<arm>.json.journal``, every ``result.json`` row into one
    ``arms.json``, ``tty/t0.json``, any ``cooperative/*.json`` markers, and a ``prediction.json``
    (if the arm used ``--derive-from``) to ``predictions/<arm>.json``. Never logs, adapters,
    images, weights.
    """
    arms_root = trial_root / "arms"
    written: list[Path] = []
    index: list[dict[str, Any]] = []
    if not arms_root.is_dir():
        return written

    for arm_root in sorted(p for p in arms_root.iterdir() if p.is_dir()):
        if not is_done(arm_root):
            continue
        arm_id = arm_root.name
        attempt = arm_root / (arm_root / "status").read_text().strip()

        result_path = attempt / "result.json"
        result_data: dict[str, Any] = {}
        if result_path.exists():
            result_data = json.loads(result_path.read_text())
            index.append(result_data)

        report_rel = result_data.get("report_path") or "report.json"
        report_path = attempt / report_rel
        if report_path.exists():
            reports_dir = bundle_dir / "reports"
            reports_dir.mkdir(parents=True, exist_ok=True)
            dest = reports_dir / f"{arm_id}.json"
            dest.write_text(report_path.read_text())
            written.append(dest)
            journal_path = report_path.with_name(f".{report_path.name}.journal")
            if journal_path.exists():
                journal_dest = reports_dir / f".{arm_id}.json.journal"
                journal_dest.write_text(journal_path.read_text())
                written.append(journal_dest)

        prediction_path = arm_root / "prediction.json"
        if prediction_path.exists():
            predictions_dir = bundle_dir / "predictions"
            predictions_dir.mkdir(parents=True, exist_ok=True)
            dest = predictions_dir / f"{arm_id}.json"
            dest.write_text(prediction_path.read_text())
            written.append(dest)

        for subdir_name in ("tty", "cooperative"):
            src_dir = attempt / subdir_name
            if not src_dir.is_dir():
                continue
            dest_dir = bundle_dir / subdir_name
            dest_dir.mkdir(parents=True, exist_ok=True)
            for src_file in sorted(src_dir.glob("*.json")):
                dest_file = dest_dir / src_file.name
                dest_file.write_text(src_file.read_text())
                written.append(dest_file)

    if index:
        arms_json = bundle_dir / "arms.json"
        bundle_dir.mkdir(parents=True, exist_ok=True)
        arms_json.write_text(json.dumps(index, indent=2) + "\n")
        written.append(arms_json)
    return written


def resolved_report_path(arm_root: Path) -> Path:
    """Return the done arm's report path, from its own ``result.json`` (single source of truth).

    Refuses (``DerivationError``) when the arm is not done — a derivation must never read a source
    arm's report before that arm has finished and been accepted as evidence.
    """
    if not is_done(arm_root):
        raise DerivationError(f"{arm_root.name}: not done yet, refusing to read its report")
    attempt = arm_root / (arm_root / "status").read_text().strip()
    result = json.loads((attempt / "result.json").read_text())
    report_rel = result.get("report_path") or "report.json"
    return attempt / report_rel


def compute_derivation(
    trial_root: Path, source_arm_ids: Sequence[str], derivation: Literal["headroom", "graceful"]
) -> dict[str, Any]:
    """Derive a limit (and, for ``graceful``, a band prediction) from named done arms' reports.

    ``headroom``: the limit is 125% of the highest peak among the named arms' own peaks — no band
    prediction. ``graceful``: the named arm with the higher ``bands.late_peak`` is chosen; the
    limit is ``bands.derive_limit`` of that peak, and the predicted band comes from
    ``bands.predict_band`` on that arm's own samples against the derived limit. Every named arm
    must already be done (``resolved_report_path`` refuses otherwise).
    """
    reports = [
        (arm_id, json.loads(resolved_report_path(trial_root / "arms" / arm_id).read_text()))
        for arm_id in source_arm_ids
    ]

    if derivation == "headroom":
        peak = max(v.peak(report) for _, report in reports)
        return {
            "derived_from": list(source_arm_ids),
            "derivation": "headroom",
            "limit_bytes": bands.headroom_limit(peak, 125),
            "predicted_band": None,
            "largest_step_near_bytes": None,
            "emergency_threshold_bytes": None,
            "source_peak_bytes": peak,
            "source_late_peak_bytes": None,
        }

    late_peaks = [
        (arm_id, report, bands.late_peak(v.available_values(report))) for arm_id, report in reports
    ]
    _chosen_id, chosen_report, chosen_late_peak = max(late_peaks, key=lambda row: row[2])
    values = v.available_values(chosen_report)
    limit = bands.derive_limit(chosen_late_peak)
    return {
        "derived_from": list(source_arm_ids),
        "derivation": "graceful",
        "limit_bytes": limit,
        "predicted_band": bands.predict_band(values, limit),
        "largest_step_near_bytes": bands.largest_step_near(values, limit),
        "emergency_threshold_bytes": bands.emergency_threshold(limit),
        "source_peak_bytes": None,
        "source_late_peak_bytes": chosen_late_peak,
    }


def write_prediction(arm_root: Path, payload: Mapping[str, Any]) -> Path:
    """Atomically write ``arms/<id>/prediction.json`` before the arm it describes ever launches."""
    arm_root.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(dir=arm_root, prefix=".prediction-")
    try:
        with os.fdopen(fd, "w") as handle:
            handle.write(json.dumps(payload, indent=2) + "\n")
        dest = arm_root / "prediction.json"
        os.replace(tmp_name, dest)
    except BaseException:
        Path(tmp_name).unlink(missing_ok=True)
        raise
    return dest


def _predicted_band_validator(predicted_band: str) -> Callable[[Mapping[str, Any]], None]:
    """Return the strict predicted-band validator a ``graceful`` derivation picks for its arm."""
    return (
        v.assert_graceful_footprint
        if predicted_band == "graceful"
        else v.assert_emergency_footprint
    )


def run_derived_arm(
    spec: ArmSpec,
    trial_root: Path,
    binary: Path,
    *,
    source_arm_ids: Sequence[str],
    derivation: Literal["headroom", "graceful"],
    dry_run: bool,
) -> ArmResult:
    """Derive ``spec``'s limit from ``source_arm_ids``' own reports, then run it via ``run_arm``.

    Writes ``arms/<spec.arm_id>/prediction.json`` before launching (never under ``dry_run``, which
    only prints the derivation and the argv). For ``graceful``, the spec's validator is replaced
    with the strict predicted-band assertion — ``run_arm`` records a predicted/observed mismatch as
    ``band_mismatch`` rather than a hard failure.
    """
    derived = compute_derivation(trial_root, source_arm_ids, derivation)
    predicted_band = derived["predicted_band"]
    if predicted_band is not None:
        final_spec = replace(
            spec,
            limit_bytes=derived["limit_bytes"],
            validator=_predicted_band_validator(predicted_band),
        )
    else:
        final_spec = replace(spec, limit_bytes=derived["limit_bytes"])

    if dry_run:
        print(f"[{spec.arm_id}] derive-from {source_arm_ids} ({derivation}): {derived}")
        return run_arm(final_spec, trial_root, binary, dry_run=True)

    write_prediction(trial_root / "arms" / spec.arm_id, derived)
    return run_arm(final_spec, trial_root, binary, dry_run=False, predicted_band=predicted_band)


def _parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run or inspect the mlx-guard in-house trial arms."
    )
    parser.add_argument("--trial-root", required=True, type=Path)
    parser.add_argument("--binary", type=Path, help="path to the mlx-guard binary under test")
    parser.add_argument(
        "--dry-run", action="store_true", help="print every arm's argv and launch nothing"
    )
    parser.add_argument("--only", help="run only this arm id (default: every not-yet-done arm)")
    parser.add_argument(
        "--mflux-model", default=None, help="override arms.MFLUX_MODEL for the F arms"
    )
    parser.add_argument(
        "--limit-bytes",
        type=int,
        default=None,
        help="resolved limit for --only's arm (L2/L3/F2/F3/C1/C2)",
    )
    parser.add_argument(
        "--wall-time-ms", type=int, default=None, help="resolved wall time for --only's arm (C1)"
    )
    parser.add_argument(
        "--checkpoint-timeout-ms",
        type=int,
        default=None,
        help="resolved checkpoint timeout for --only's arm (C1)",
    )
    parser.add_argument(
        "--resume-report",
        default=None,
        help="C2 only: path to C1's report.json, passed through to the launcher unchanged",
    )
    parser.add_argument(
        "--c1-checkpoints",
        default=None,
        help="C2 only: path to C1's checkpoint root, passed through to the launcher unchanged",
    )
    parser.add_argument(
        "--derive-from",
        default=None,
        metavar="ARM_A[,ARM_B]",
        help="derive --only's limit (and, for --derivation graceful, its band prediction) "
        "from these done arms' own reports, instead of a hand-computed --limit-bytes",
    )
    parser.add_argument(
        "--derivation",
        choices=("headroom", "graceful"),
        default=None,
        help="how to derive the limit from --derive-from's arms",
    )
    parser.add_argument("--assemble", type=Path, default=None, metavar="BUNDLE_DIR")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    """CLI entry point.

    ``--dry-run`` prints every arm's argv; ``--assemble`` collects done arms' evidence.
    """
    args = _parse_args(argv if argv is not None else sys.argv[1:])

    if (args.resume_report is not None or args.c1_checkpoints is not None) and args.only is None:
        print("--resume-report/--c1-checkpoints require --only", file=sys.stderr)
        return 2

    if args.assemble is not None:
        written = assemble(args.trial_root, args.assemble)
        for path in written:
            print(path)
        return 0

    if args.binary is None:
        print("--binary is required unless --assemble is given", file=sys.stderr)
        return 2

    if args.derive_from is not None:
        if args.only is None:
            print("--derive-from requires --only", file=sys.stderr)
            return 2
        if args.limit_bytes is not None:
            print("--derive-from and --limit-bytes are mutually exclusive", file=sys.stderr)
            return 2
        if args.derivation is None:
            print("--derive-from requires --derivation headroom|graceful", file=sys.stderr)
            return 2

    import arms as arm_table  # deferred: arms.py imports ArmSpec back from this module

    build_kwargs: dict[str, Any] = {}
    if args.mflux_model is not None:
        build_kwargs["mflux_model"] = args.mflux_model
    if args.only is not None and args.derive_from is None:
        # A limit/wall-time/checkpoint-timeout override only ever targets --only's one arm — its
        # value is derived from an earlier arm's own report, one arm at a time.
        if args.limit_bytes is not None:
            build_kwargs["limit_bytes"] = {args.only: args.limit_bytes}
        if args.wall_time_ms is not None:
            build_kwargs["wall_time_ms"] = {args.only: args.wall_time_ms}
        if args.checkpoint_timeout_ms is not None:
            build_kwargs["checkpoint_timeout_ms"] = {args.only: args.checkpoint_timeout_ms}
        if args.resume_report is not None:
            build_kwargs["resume_report"] = {args.only: args.resume_report}
        if args.c1_checkpoints is not None:
            build_kwargs["c1_checkpoints"] = {args.only: args.c1_checkpoints}
    specs = arm_table.build_arms(**build_kwargs)

    if args.only is not None:
        specs = [s for s in specs if s.arm_id == args.only]
        if not specs:
            print(f"unknown arm id {args.only!r}", file=sys.stderr)
            return 2

    if args.derive_from is not None:
        spec = specs[0]
        source_arm_ids = [a.strip() for a in args.derive_from.split(",") if a.strip()]
        try:
            run_derived_arm(
                spec,
                args.trial_root,
                args.binary,
                source_arm_ids=source_arm_ids,
                derivation=args.derivation,
                dry_run=args.dry_run,
            )
        except DerivationError as exc:
            print(f"derivation refused: {exc}", file=sys.stderr)
            return 2
        return 0

    for spec in specs:
        if not args.dry_run and is_done(args.trial_root / "arms" / spec.arm_id):
            print(f"[{spec.arm_id}] already done, skipping")
            continue
        run_arm(spec, args.trial_root, args.binary, dry_run=args.dry_run)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
