"""Host preflight for the in-house trial: a green/red safety gate, plus ``provenance.json``.

Never downloads anything. Refuses to continue when the host is not in a safe, honest state to
launch a heavy arm: not enough free memory, running on battery, or a model/dataset the next arm
needs is not already cached locally (in which case it prints the download size instead of
fetching it). ``provenance.json`` never carries a filesystem path — see ``scan_for_paths``.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

_FREE_PERCENT_RE = re.compile(r"System-wide memory free percentage:\s*(\d+)%")
_MIN_FREE_PERCENT = 20.0


class PreflightError(RuntimeError):
    """The host is not in a safe or honest state to launch a trial arm."""


def parse_memory_pressure_free_percent(text: str) -> float:
    """Extract the "System-wide memory free percentage" figure from `memory_pressure` output."""
    match = _FREE_PERCENT_RE.search(text)
    if not match:
        raise PreflightError(
            "could not find 'System-wide memory free percentage' in memory_pressure output"
        )
    return float(match.group(1))


def is_on_battery_power(pmset_output: str) -> bool:
    """Return True when `pmset -g batt`'s first line names battery power rather than AC."""
    first_line = pmset_output.splitlines()[0] if pmset_output else ""
    return "Battery Power" in first_line


def check_gate(
    *, free_percent: float, on_battery: bool, min_free_percent: float = _MIN_FREE_PERCENT
) -> None:
    """Raise ``PreflightError`` when the host is not safe to launch a heavy arm right now."""
    if on_battery:
        raise PreflightError("refusing: running on battery power")
    if free_percent < min_free_percent:
        raise PreflightError(
            f"refusing: memory_pressure free {free_percent:.0f}% "
            f"is below the {min_free_percent:.0f}% floor"
        )


@dataclass(frozen=True, slots=True)
class ModelRequirement:
    """A model or dataset an arm needs, with its approximate size if it must be downloaded."""

    name: str
    cache_check: Callable[[], bool]
    download_size_bytes: int


def check_requirements(requirements: list[ModelRequirement]) -> list[str]:
    """Return one message per requirement not cached, naming its size; empty when all present."""
    messages = []
    for req in requirements:
        if req.cache_check():
            continue
        gib = req.download_size_bytes / 1024**3
        messages.append(f"{req.name} is not cached locally (~{gib:.1f} GiB download)")
    return messages


# Marker strings a value is checked against, never a temp-file path this code opens itself.
_PATH_MARKERS = ("/Users/", "/Volumes/", "/private/", "/tmp/", "/var/")  # noqa: S108


def _looks_like_a_path(value: str) -> bool:
    """Return True for a value shaped like a filesystem path, not merely one containing ``/``.

    An absolute path, a home-relative path (``~``), or one containing a well-known mount or
    prefix — never a bare string that happens to contain a slash, such as a git branch name
    (``trial/in-house-recipes``).
    """
    return value.startswith(("/", "~")) or any(marker in value for marker in _PATH_MARKERS)


def scan_for_paths(payload: Mapping[str, Any], *, prefix: str = "") -> list[str]:
    """Dotted keys of every string value that looks like a filesystem path."""
    offenders: list[str] = []
    for key, value in payload.items():
        dotted = f"{prefix}.{key}" if prefix else key
        if isinstance(value, Mapping):
            offenders.extend(scan_for_paths(value, prefix=dotted))
        elif isinstance(value, str) and _looks_like_a_path(value):
            offenders.append(dotted)
    return offenders


def _run(argv: list[str]) -> str:
    return subprocess.run(argv, capture_output=True, text=True, check=True).stdout  # noqa: S603 — literal argv, fixed tool list


def _git(*args: str) -> str:
    return _run(["/usr/bin/git", *args]).strip()


def build_provenance(*, binary: Path, rust_tree_reference: str = "b6a0504") -> dict[str, Any]:
    """Assemble the path-free ``provenance.json`` payload from real host and tool facts."""
    memory_pressure_text = _run(["/usr/bin/memory_pressure"])
    free_percent = parse_memory_pressure_free_percent(memory_pressure_text)
    pmset_text = _run(["/usr/bin/pmset", "-g", "batt"])
    on_battery = is_on_battery_power(pmset_text)

    sw_vers = {
        line.split(":", 1)[0].strip(): line.split(":", 1)[1].strip()
        for line in _run(["/usr/bin/sw_vers"]).splitlines()
        if ":" in line
    }
    worktree_clean = _git("status", "--porcelain") == ""
    diff_clean = (
        subprocess.run(  # noqa: S603 — literal argv, fixed tool
            [
                "/usr/bin/git",
                "diff",
                "--quiet",
                rust_tree_reference,
                "--",
                "crates",
                "Cargo.toml",
                "Cargo.lock",
            ],
            check=False,
        ).returncode
        == 0
    )
    binary_bytes = binary.read_bytes()
    import hashlib

    sha256 = hashlib.sha256(binary_bytes).hexdigest()
    version_out = subprocess.run(  # noqa: S603 — literal argv, absolute binary path
        [str(binary), "--version"], capture_output=True, text=True, check=True
    ).stdout.strip()

    payload: dict[str, Any] = {
        "commit": _git("rev-parse", "HEAD"),
        "branch": _git("rev-parse", "--abbrev-ref", "HEAD"),
        "worktree_clean": worktree_clean,
        "rust_tree_equals": rust_tree_reference if diff_clean else None,
        "binary": {
            "version": version_out,
            "sha256": sha256,
            "profile": "release",
            "wheel_filename": None,
        },
        "macos": {
            "ProductVersion": sw_vers.get("ProductVersion"),
            "BuildVersion": sw_vers.get("BuildVersion"),
        },
        "chip": _run(["/usr/sbin/sysctl", "-n", "machdep.cpu.brand_string"]).strip()
        or _run(["/usr/sbin/sysctl", "-n", "hw.model"]).strip(),
        "memory_bytes": int(_run(["/usr/sbin/sysctl", "-n", "hw.memsize"]).strip()),
        "power_source": "battery" if on_battery else "ac",
        "memory_pressure_free_percent": free_percent,
        "captured_at": datetime.now(UTC).isoformat(),
    }
    return payload


def main(argv: list[str] | None = None) -> int:
    """CLI entry point: refuse loudly on an unsafe host, else write provenance.json to stdout."""
    args = argv if argv is not None else sys.argv[1:]
    if len(args) != 1:
        print("usage: preflight.py <path-to-mlx-guard-binary>", file=sys.stderr)
        return 2
    binary = Path(args[0])

    try:
        memory_pressure_text = _run(["/usr/bin/memory_pressure"])
        free_percent = parse_memory_pressure_free_percent(memory_pressure_text)
        pmset_text = _run(["/usr/bin/pmset", "-g", "batt"])
        on_battery = is_on_battery_power(pmset_text)
        check_gate(free_percent=free_percent, on_battery=on_battery)
        payload = build_provenance(binary=binary)
        offenders = scan_for_paths(payload)
        if offenders:
            raise PreflightError(
                f"provenance.json would leak a path-shaped value at: {', '.join(offenders)}"
            )
    except PreflightError as exc:
        print(f"preflight refused: {exc}", file=sys.stderr)
        return 1

    print(json.dumps(payload, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
