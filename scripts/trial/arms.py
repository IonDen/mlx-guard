"""The trial matrix (rev 2): one ``ArmSpec`` per row, in run order.

Limits and wall times for arms whose value depends on an earlier arm's own report (L2, L3, F2, F3,
C1, C2) start out ``None`` here; ``build_arms``'s ``limit_bytes``/``wall_time_ms``/
``checkpoint_timeout_ms`` keyword arguments (each a mapping of arm id to resolved value) fill them
in once that earlier arm's peak is known. ``orchestrator.py --dry-run`` renders an unresolved value
as ``<limit>`` / ``<walltime>`` rather than guessing one.
"""

import sys
from collections.abc import Mapping
from pathlib import Path

import validators as v
from orchestrator import ArmSpec

LORA_MODEL = "mlx-community/Qwen2.5-0.5B-Instruct-4bit"
LORA_DATA = "mlx-community/wikisql"
LORA_ITERS = "200"

MFLUX_MODEL = "dev"  # ruling A default (open point A); override with --mflux-model
MFLUX_PROMPT = "a lighthouse at dusk"
MFLUX_STEPS = "4"
MFLUX_SEED = "42"

TTY_PROBE_COMMAND = ("python3", "-c", "import time; time.sleep(3)")

RESUMABLE_LORA = str(Path(__file__).resolve().parent / "resumable_lora.py")


def _lora_argv(*, from_spec: str) -> tuple[str, ...]:
    """Build the wrap-a-command LoRA recipe argv, verbatim apart from the ``uvx --from`` spec."""
    return (
        "uvx",
        "--from",
        from_spec,
        "mlx_lm.lora",
        "--model",
        LORA_MODEL,
        "--train",
        "--data",
        LORA_DATA,
        "--iters",
        LORA_ITERS,
        "--adapter-path",
        "adapters",
    )


def _mflux_argv(*, model: str) -> tuple[str, ...]:
    """Build the wrap-a-command mflux recipe argv, verbatim apart from the model (open point A)."""
    return (
        "mflux-generate",
        "--model",
        model,
        "--quantize",
        "8",
        "--prompt",
        MFLUX_PROMPT,
        "--steps",
        MFLUX_STEPS,
        "--seed",
        MFLUX_SEED,
        "--output",
        "out.png",
    )


def _lookup(overrides: Mapping[str, int] | None, arm_id: str) -> int | None:
    return None if overrides is None else overrides.get(arm_id)


def _required_flag(flag: str, value: int | None, placeholder: str) -> tuple[str, str]:
    """Render a mandatory numeric ``launch`` flag, or a dry-run placeholder when unresolved."""
    return (flag, str(value) if value is not None else placeholder)


def _assert_cooperative_wall_time(report: Mapping[str, object]) -> None:
    """Adapt ``assert_cooperative`` (which returns the checkpoint record) to a bare validator."""
    v.assert_cooperative(report, reason="wall_time")


def build_arms(
    *,
    mflux_model: str = MFLUX_MODEL,
    limit_bytes: Mapping[str, int] | None = None,
    wall_time_ms: Mapping[str, int] | None = None,
    checkpoint_timeout_ms: Mapping[str, int] | None = None,
) -> list[ArmSpec]:
    """Build the full trial matrix, in run order: T0, L0, L1a/b, L2, L3, F0, F1a/b, F2, F3, C0-2."""
    lora_l0 = _lora_argv(from_spec="mlx-lm")
    lora_l1plus = _lora_argv(from_spec="mlx-lm[train]")
    mflux_argv = _mflux_argv(model=mflux_model)

    return [
        ArmSpec(
            arm_id="T0",
            mode="probe",
            argv=TTY_PROBE_COMMAND,
            sample_interval_ms=50,
            limit_bytes=None,
            wall_time_ms=None,
            validator=v.assert_tty_probe,
            notes="ladder's first command under a pty: bare stdin refused 64, `< /dev/null` runs 0",
        ),
        ArmSpec(
            arm_id="L0",
            mode="observe",
            argv=lora_l0,
            sample_interval_ms=50,
            limit_bytes=None,
            wall_time_ms=None,
            validator=lambda report: v.assert_child_exit(report, 1),
            notes=(
                "as printed (bare 'mlx-lm'): expected ModuleNotFoundError: "
                "No module named 'datasets'"
            ),
        ),
        ArmSpec(
            arm_id="L1a",
            mode="observe",
            argv=lora_l1plus,
            sample_interval_ms=50,
            limit_bytes=None,
            wall_time_ms=None,
            validator=v.assert_uneventful,
            notes="mlx-lm[train]; first of two calibration runs for L2/L3's derived limits",
        ),
        ArmSpec(
            arm_id="L1b",
            mode="observe",
            argv=lora_l1plus,
            sample_interval_ms=50,
            limit_bytes=None,
            wall_time_ms=None,
            validator=v.assert_uneventful,
            notes="mlx-lm[train]; second calibration run, for peak spread",
        ),
        ArmSpec(
            arm_id="L2",
            mode="run",
            argv=lora_l1plus,
            sample_interval_ms=50,
            limit_bytes=_lookup(limit_bytes, "L2"),
            wall_time_ms=None,
            validator=v.assert_uneventful,
            notes="headroom_limit(max(L1a.peak, L1b.peak)); must not fire",
        ),
        ArmSpec(
            arm_id="L3",
            mode="run",
            argv=lora_l1plus,
            sample_interval_ms=50,
            limit_bytes=_lookup(limit_bytes, "L3"),
            wall_time_ms=None,
            validator=lambda report: (
                v.assert_graceful_footprint(report)
                if v.observed_band(report) == "graceful"
                else v.assert_emergency_footprint(report)
            ),
            notes=(
                "derive_limit(late_peak(L1 samples)); band predicted before launch, "
                "checked against observed_band"
            ),
        ),
        ArmSpec(
            arm_id="F0",
            mode="run",
            argv=mflux_argv,
            sample_interval_ms=250,
            limit_bytes=25 * 1024**3,
            wall_time_ms=None,
            validator=v.assert_uneventful,
            notes=(
                "ceiling pass (review C4): 25GiB, no cache knob — "
                "argv-identical to F1 apart from the guard"
            ),
        ),
        ArmSpec(
            arm_id="F1a",
            mode="observe",
            argv=mflux_argv,
            sample_interval_ms=250,
            limit_bytes=None,
            wall_time_ms=None,
            validator=v.assert_uneventful,
            notes="first of two calibration runs for F2/F3's derived limits",
        ),
        ArmSpec(
            arm_id="F1b",
            mode="observe",
            argv=mflux_argv,
            sample_interval_ms=250,
            limit_bytes=None,
            wall_time_ms=None,
            validator=v.assert_uneventful,
            notes="second calibration run, for peak spread",
        ),
        ArmSpec(
            arm_id="F2",
            mode="run",
            argv=mflux_argv,
            sample_interval_ms=50,
            limit_bytes=_lookup(limit_bytes, "F2"),
            wall_time_ms=None,
            validator=v.assert_uneventful,
            notes="headroom_limit(max(F1a.peak, F1b.peak)); must not fire",
        ),
        ArmSpec(
            arm_id="F3",
            mode="run",
            argv=mflux_argv,
            sample_interval_ms=50,
            limit_bytes=_lookup(limit_bytes, "F3"),
            wall_time_ms=None,
            validator=lambda report: (
                v.assert_graceful_footprint(report)
                if v.observed_band(report) == "graceful"
                else v.assert_emergency_footprint(report)
            ),
            notes=(
                "derive_limit(late_peak(F1 samples)); load moves GB between samples, "
                "emergency predicted"
            ),
        ),
        ArmSpec(
            arm_id="C0",
            mode="observe",
            argv=(
                sys.executable,
                RESUMABLE_LORA,
                "train",
                "--checkpoints",
                "checkpoints",
                "--total-steps",
                "20",
                "--calibrate",
            ),
            sample_interval_ms=50,
            limit_bytes=None,
            wall_time_ms=None,
            validator=v.assert_uneventful,
            notes=(
                "calibration worker: no checkpoint request arrives; "
                "its stdout JSON derives C1's limit/timeouts"
            ),
        ),
        ArmSpec(
            arm_id="C1",
            mode="python",
            argv=(
                sys.executable,
                RESUMABLE_LORA,
                "launch",
                "--arm",
                "c1",
                "--total-steps",
                "400",
                *_required_flag("--max-footprint-bytes", _lookup(limit_bytes, "C1"), "<limit>"),
                *(
                    ("--wall-time-ms", str(_lookup(wall_time_ms, "C1")))
                    if _lookup(wall_time_ms, "C1") is not None
                    else ()
                ),
                *_required_flag(
                    "--checkpoint-timeout-ms",
                    _lookup(checkpoint_timeout_ms, "C1"),
                    "<checkpoint-timeout>",
                ),
            ),
            sample_interval_ms=50,
            limit_bytes=_lookup(limit_bytes, "C1"),
            wall_time_ms=_lookup(wall_time_ms, "C1"),
            validator=_assert_cooperative_wall_time,
            notes="PYTHON_ADAPTER.md pattern, forced by wall_time_ms from C0; limit_C never fires",
        ),
        ArmSpec(
            arm_id="C2",
            mode="python",
            argv=(
                sys.executable,
                RESUMABLE_LORA,
                "launch",
                "--arm",
                "c2",
                "--total-steps",
                "450",
                *_required_flag("--max-footprint-bytes", _lookup(limit_bytes, "C2"), "<limit>"),
                *_required_flag(
                    "--checkpoint-timeout-ms",
                    _lookup(checkpoint_timeout_ms, "C2"),
                    "<checkpoint-timeout>",
                ),
            ),
            sample_interval_ms=50,
            limit_bytes=_lookup(limit_bytes, "C2"),
            wall_time_ms=None,
            validator=lambda report: v.assert_child_exit(report, 0),
            notes=(
                "resume from C1's artifact (--resume-report/--c1-checkpoints added by hand at "
                "launch, once C1's attempt directory is known); wait >= 30s after C1 "
                "(PYTHON_API.md walkthrough)"
            ),
        ),
    ]
