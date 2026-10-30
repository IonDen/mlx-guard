"""Plain outcome lines for a final report: what happened, and what to do next.

The text is derived from the report payload alone, with the same rules and the same integer
arithmetic as the native explainer, so both render the same bytes for the same report. Every
lookup is checked: a field that is absent or has an unexpected shape makes its rule not match,
and the text falls back to a less specific sentence instead of raising.
"""

import re
from collections.abc import Mapping
from typing import TYPE_CHECKING, NamedTuple

if TYPE_CHECKING:
    from ._client import JsonValue

_MAX_U64 = (1 << 64) - 1
_MAX_U8 = 255
_SIGKILL = 9
_SIGTERM = 15
_COLLISION_CODES = (64, 70, 74, 75, 126, 127)
_GROUP = "owned_process_group"
_DELIVERED = "delivered"
_EXTERNAL = "external_signal"
_DOC_URL = "https://github.com/IonDen/mlx-guard/blob/{ref}/docs/TROUBLESHOOTING.md#{anchor}"
_PACKAGE_VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.+-]*)?")
_MAX_PACKAGE_VERSION_BYTES = 64

ANCHORS: tuple[str, ...] = (
    "64-the-command-line",
    "64-a-launch-setting",
    "70-before-launch",
    "70-while-starting-the-command",
    "70-footprint-readings-lost",
    "70-a-signal-not-delivered",
    "70-an-internal-failure",
    "74-report-path-already-used",
    "74-report-directory",
    "74-before-launch",
    "74-after-launch",
    "75-the-footprint-limit",
    "75-emergency-kill",
    "75-the-wall-time-limit",
    "75-the-launching-process-exited",
    "75-a-signal-the-command-ignored",
    "75-processes-left-behind",
    "75-cause-not-recorded",
    "126-not-executable",
    "127-not-found",
    "signal-passed-on-by-mlx-guard",
    "signal-from-somewhere-else",
    "a-code-mlx-guard-also-uses",
)

_SIGNAL_NAMES = {
    1: "SIGHUP",
    2: "SIGINT",
    3: "SIGQUIT",
    6: "SIGABRT",
    9: "SIGKILL",
    10: "SIGBUS",
    11: "SIGSEGV",
    13: "SIGPIPE",
    15: "SIGTERM",
    30: "SIGUSR1",
}

_OBSERVATION_ERRORS = {
    "permission_denied": "permission denied",
    "process_missing": "a process disappeared",
    "malformed_kernel_data": "the kernel returned malformed data",
    "clock_anomaly": "the clock went backwards",
    "internal": "an internal error",
}


class _Lines(NamedTuple):
    happened: str
    to_do: str
    anchor: str


class _Signal(NamedTuple):
    at_ms: int
    signal: int
    target: str
    result: str
    reason: str | None


class _Transition(NamedTuple):
    source: str
    target: str
    footprint: int | None


class _View(NamedTuple):
    """The report facts the rules read, each already checked for its expected shape."""

    kind: str | None
    at_ms: int | None
    code: int | None
    signal: int | None
    parent_exited_at_ms: int | None
    mode: str | None
    configuration: object
    samples: tuple[object, ...]
    transitions: tuple[_Transition, ...]
    signals: tuple[_Signal, ...]
    checkpoint_status: str | None
    checkpoint_request_id: int | None
    checkpoint_reason: str | None

    def config(self, key: str) -> int | None:
        return _uint(_get(self.configuration, key))


def explain_payload(payload: "Mapping[str, JsonValue]") -> str | None:
    """Return the two plain lines for a report payload, or ``None`` when none are printed."""
    if not isinstance(payload, Mapping):
        return None
    lines = _explain(_view(payload))
    if lines is None:
        return None
    version = _package_version(payload.get("package_version"))
    ref = "main" if version is None else f"v{version}"
    url = _DOC_URL.format(ref=ref, anchor=lines.anchor)
    return f"what happened: {lines.happened}\nwhat to do: {lines.to_do} More: {url}"


# ---- Checked lookups ----


def _get(container: object, key: str) -> object:
    if isinstance(container, Mapping):
        return container.get(key)
    return None


def _uint(value: object, maximum: int = _MAX_U64) -> int | None:
    if isinstance(value, int) and not isinstance(value, bool) and 0 <= value <= maximum:
        return value
    return None


def _text(value: object) -> str | None:
    return value if isinstance(value, str) else None


def _package_version(value: object) -> str | None:
    """Return the version only when it has the shape the native report reader accepts."""
    if not isinstance(value, str) or len(value.encode()) > _MAX_PACKAGE_VERSION_BYTES:
        return None
    return value if _PACKAGE_VERSION.fullmatch(value) else None


def _items(value: object) -> tuple[object, ...]:
    return tuple(value) if isinstance(value, (list, tuple)) else ()


def _signal_records(value: object) -> tuple[_Signal, ...]:
    records: list[_Signal] = []
    for item in _items(value):
        at_ms = _uint(_get(item, "at_ms"))
        signal = _uint(_get(item, "signal"), _MAX_U8)
        target = _text(_get(item, "target"))
        result = _text(_get(item, "result"))
        if at_ms is None or signal is None or target is None or result is None:
            continue
        records.append(_Signal(at_ms, signal, target, result, _text(_get(item, "reason"))))
    return tuple(records)


def _transition_records(value: object) -> tuple[_Transition, ...]:
    records: list[_Transition] = []
    for item in _items(value):
        source = _text(_get(item, "from"))
        target = _text(_get(item, "to"))
        if source is None or target is None:
            continue
        footprint = _uint(_get(item, "aggregate_footprint_bytes"))
        records.append(_Transition(source, target, footprint))
    return tuple(records)


def _view(payload: Mapping[str, object]) -> _View:
    outcome = payload.get("outcome")
    configuration = payload.get("configuration")
    checkpoint = payload.get("checkpoint")
    return _View(
        kind=_text(_get(outcome, "kind")),
        at_ms=_uint(_get(outcome, "at_ms")),
        code=_uint(_get(outcome, "code"), _MAX_U8),
        signal=_uint(_get(outcome, "signal"), _MAX_U8),
        parent_exited_at_ms=_uint(_get(outcome, "parent_exited_at_ms")),
        mode=_text(_get(configuration, "mode")),
        configuration=configuration,
        samples=_items(payload.get("samples")),
        transitions=_transition_records(payload.get("transitions")),
        signals=_signal_records(payload.get("signals")),
        checkpoint_status=_text(_get(checkpoint, "status")),
        checkpoint_request_id=_uint(_get(checkpoint, "request_id")),
        checkpoint_reason=_text(_get(checkpoint, "reason")),
    )


# ---- Integer-only formatting ----


def _format_bytes(value: int) -> str:
    units = ((1 << 10, "KiB"), (1 << 20, "MiB"), (1 << 30, "GiB"))
    fitting = [index for index, (unit, _) in enumerate(units) if value >= unit]
    if not fitting:
        return f"{value} B"
    index = fitting[-1]
    rendered = (value * 100 + units[index][0] // 2) // units[index][0]
    if rendered >= 102_400 and index + 1 < len(units):
        index += 1
        rendered = (value * 100 + units[index][0] // 2) // units[index][0]
    return f"{rendered // 100}.{rendered % 100:02d} {units[index][1]}"


def _format_duration(ms: int) -> str:
    if ms < 1000:
        return f"{ms} ms"
    tenths = (ms + 50) // 100
    if tenths < 600:
        return f"{tenths // 10}.{tenths % 10} s"
    seconds = (ms + 500) // 1000
    if seconds < 3600:
        rest = seconds % 60
        return f"{seconds // 60} min" if rest == 0 else f"{seconds // 60} min {rest} s"
    minutes = (ms + 30_000) // 60_000
    rest = minutes % 60
    return f"{minutes // 60} h" if rest == 0 else f"{minutes // 60} h {rest} min"


def _signal_name(signal: int) -> str:
    return _SIGNAL_NAMES.get(signal, f"signal {signal}")


def _compared_bytes(*values: int) -> list[str]:
    """Render values a sentence compares; if two differ but render alike, print all as bytes."""
    rendered = [_format_bytes(value) for value in values]
    ambiguous = any(
        values[left] != values[right] and rendered[left] == rendered[right]
        for left in range(len(values))
        for right in range(left + 1, len(values))
    )
    return [f"{value} bytes" for value in values] if ambiguous else rendered


# ---- Shared clauses ----


def _is_group_signal(record: _Signal, signal: int) -> bool:
    return record.target == _GROUP and record.signal == signal


def _first_group_signal(view: _View, signal: int) -> _Signal | None:
    return next((record for record in view.signals if _is_group_signal(record, signal)), None)


def _sent_to_group(signal: int, at_ms: int) -> str:
    return (
        f"sent {_signal_name(signal)} to the command's process group at {_format_duration(at_ms)}"
    )


def _action_clause(view: _View) -> str:
    """Describe what mlx-guard did from the signal records; only a delivered one was sent."""
    signals = view.signals
    term_index = next(
        (index for index, record in enumerate(signals) if _is_group_signal(record, _SIGTERM)),
        None,
    )
    term = None if term_index is None else signals[term_index]
    request = next(
        (
            record
            for record in signals
            if record.target == "cooperative_endpoint" and record.result == _DELIVERED
        ),
        None,
    )
    if request is not None:
        asked = f"asked the command to save a checkpoint at {_format_duration(request.at_ms)}"
        if term_index is not None and term is not None:
            if term.result == _DELIVERED:
                suffix = _kill_suffix(signals, term_index)
                return f"{asked} and then sent SIGTERM to its process group{suffix}"
            if term.result == "process_missing":
                suffix = _kill_suffix(signals, term_index)
                return (
                    f"{asked}; the command ended before the SIGTERM that followed could reach "
                    f"it{suffix}"
                )
        return asked
    if term_index is not None and term is not None:
        if term.result == _DELIVERED:
            return f"{_sent_to_group(_SIGTERM, term.at_ms)}{_kill_suffix(signals, term_index)}"
        if term.result == "process_missing":
            return (
                "found the command already ended when it sent SIGTERM at "
                f"{_format_duration(term.at_ms)}{_kill_suffix(signals, term_index)}"
            )
    kill = next(
        (
            record
            for record in signals
            if _is_group_signal(record, _SIGKILL) and record.result == _DELIVERED
        ),
        None,
    )
    if kill is not None:
        return _sent_to_group(_SIGKILL, kill.at_ms)
    if view.at_ms is None:
        return "stopped the command"
    return f"stopped the command at {_format_duration(view.at_ms)}"


def _kill_suffix(signals: tuple[_Signal, ...], term_index: int) -> str:
    """Only a delivered group KILL after the first TERM; a forwarded one is a second signal.

    After a TERM that found the command ended, that second-signal KILL is its own clause.
    """
    term = signals[term_index]
    kill = next(
        (
            record
            for record in signals[term_index + 1 :]
            if _is_group_signal(record, _SIGKILL) and record.result == _DELIVERED
        ),
        None,
    )
    if kill is None:
        return ""
    gap = _format_duration(max(kill.at_ms - term.at_ms, 0))
    if kill.reason == _EXTERNAL and term.result == "process_missing":
        return f"; a SIGKILL for a second terminal signal reached its process group {gap} later"
    if kill.reason == _EXTERNAL:
        return f", and SIGKILL {gap} later, after a second terminal signal arrived"
    if kill.reason == term.reason:
        return f", and SIGKILL {gap} later because it was still running"
    return ""


def _checkpoint_clause(view: _View) -> str:
    status, request_id = view.checkpoint_status, view.checkpoint_request_id
    if status == "acknowledged_unverified_durability" and request_id is not None:
        return f" The command acknowledged checkpoint request {request_id}."
    if status == "timed_out" and request_id is not None:
        timeout = view.config("checkpoint_timeout_ms")
        if timeout is None:
            return ""
        return (
            f" The command did not acknowledge checkpoint request {request_id} within "
            f"{_format_duration(timeout)}; it may still have saved it."
        )
    if status == "requested_unverified":
        if request_id is not None:
            return (
                f" The command had not acknowledged checkpoint request {request_id} when it ended."
            )
        return " A checkpoint was requested but not acknowledged."
    if status == "cancelled":
        return " The checkpoint request could not be delivered."
    return ""


def _resume_clause(view: _View) -> str:
    status, request_id = view.checkpoint_status, view.checkpoint_request_id
    if request_id is None:
        return ""
    if status == "acknowledged_unverified_durability":
        return f"; resume from the state saved for checkpoint request {request_id}"
    if status == "timed_out":
        return f"; if the command saved state for checkpoint request {request_id}, resume from it"
    return ""


# ---- Rows, in rule order ----

_RETRY_NEW_REPORT = "give the retry a new --report name, because this run kept its report."


def _explain(view: _View) -> _Lines | None:
    kind = view.kind
    if kind == "child_exited":
        return _child_exited(view.code)
    if kind == "child_signaled":
        return None if view.signal is None else _child_signaled(view, view.signal)
    if kind == "invalid_configuration":
        return _Lines(
            "mlx-guard refused a launch setting before starting the command, so nothing ran; "
            "the error line on stderr names the setting.",
            "--cwd must name an existing directory, and --env must not set "
            "MLX_GUARD_CHECKPOINT_FD; fix the one named, and " + _RETRY_NEW_REPORT,
            "64-a-launch-setting",
        )
    if kind == "launch_not_executable":
        return _Lines(
            "the file named after -- exists but could not be run, so nothing was launched.",
            "make it executable (chmod +x) or put its interpreter first, as in -- python "
            "script.py; " + _RETRY_NEW_REPORT,
            "126-not-executable",
        )
    if kind == "launch_not_found":
        return _Lines(
            "no executable was found for the command named after --, so nothing was launched.",
            "check the spelling, give the full path, or activate the environment that provides "
            "it; " + _RETRY_NEW_REPORT,
            "127-not-found",
        )
    if kind == "supervisor_failure":
        return _supervisor_failure(view)
    if kind == "policy_intervention":
        return _policy_intervention(view)
    if kind == "partial_artifact_failure":
        return _Lines(
            "the command was supervised, but mlx-guard could not write its complete report; "
            "safety enforcement was not affected.",
            "check free disk space and that nothing else changed the report directory during "
            "the run; the hidden .<name>.journal beside the report path keeps what was saved. "
            "Use a new --report name next time.",
            "74-after-launch",
        )
    return None


def _child_exited(code: int | None) -> _Lines | None:
    if code is None or code not in _COLLISION_CODES:
        return None
    return _Lines(
        f"the command itself exited with code {code}, and mlx-guard did not intervene; "
        f"{code} is also one of mlx-guard's own exit codes.",
        "read the command's own output; the report's outcome.kind is child_exited.",
        "a-code-mlx-guard-also-uses",
    )


def _child_signaled(view: _View, signal: int) -> _Lines:
    passed_on = any(
        record.reason == _EXTERNAL and record.result == _DELIVERED and record.signal == signal
        for record in view.signals
    )
    name = _signal_name(signal)
    if passed_on:
        if signal == _SIGKILL:
            happened = (
                "the command ended on SIGKILL, which mlx-guard sent after a second terminal "
                "signal arrived."
            )
        else:
            happened = f"the command ended on {name}, which mlx-guard received and passed on to it."
        return _Lines(happened, "nothing to do if you sent it.", "signal-passed-on-by-mlx-guard")
    happened = f"the command ended on {name}, and it did not come from mlx-guard"
    if signal == _SIGKILL:
        return _Lines(
            happened + "; something else stopped it, and macOS can do this when memory runs short.",
            "check the command's own output and the macOS logs; if memory ran short, set "
            "--max-footprint (with mlx-guard run) below that point so mlx-guard stops the "
            "command first.",
            "signal-from-somewhere-else",
        )
    return _Lines(
        happened + ".",
        "the command's own output or its macOS crash report says why.",
        "signal-from-somewhere-else",
    )


def _supervisor_failure(view: _View) -> _Lines:
    if not view.samples and view.at_ms == 0:
        return _Lines(
            "mlx-guard could not finish starting the run, so it supervised nothing; the error "
            "line on stderr names the step.",
            "run it again; if it repeats, open an issue with that error line and the redacted "
            "report.",
            "70-while-starting-the-command",
        )
    if view.mode == "enforce":
        not_delivered = _not_delivered(view)
        if not_delivered is not None:
            return not_delivered
    readings_lost = any(record.reason == "observation_failure" for record in view.signals) or (
        view.mode == "observe"
        and any(transition.target == "supervisor_error" for transition in view.transitions)
    )
    if readings_lost:
        lost = _readings_lost(view)
        if lost is not None:
            return lost
    return _internal_failure(view)


def _not_delivered(view: _View) -> _Lines | None:
    words = {"permission_denied": "permission denied", "failed": "failed"}
    record = next(
        (record for record in view.signals if record.target == _GROUP and record.result in words),
        None,
    )
    if record is None:
        return None
    return _Lines(
        f"mlx-guard could not deliver {_signal_name(record.signal)} to the command's process "
        "group, which still had a live member or could not be checked "
        f"(result: {words[record.result]}).",
        "check whether the command is still running and stop it yourself, then open an issue "
        "with the redacted report.",
        "70-a-signal-not-delivered",
    )


def _last_reading(view: _View) -> str:
    if not view.samples:
        return ""
    reading = _get(view.samples[-1], "aggregate_footprint_bytes")
    status = _text(_get(reading, "status"))
    if status == "error":
        code = _text(_get(reading, "code"))
        words = None if code is None else _OBSERVATION_ERRORS.get(code)
    elif status == "stale":
        words = "the reading was too old"
    elif status in ("unavailable", "unknown"):
        words = "no reading"
    else:
        words = None
    return "" if words is None else f" (last reading: {words})"


def _readings_lost(view: _View) -> _Lines | None:
    missing = view.config("max_missing_samples")
    if missing is None:
        return None
    lost = (
        f"mlx-guard could not read the OS-accounted footprint for {missing} samples in a "
        f"row{_last_reading(view)}"
    )
    if view.mode == "enforce":
        return _Lines(
            f"{lost}, so it stopped the command rather than let it run unwatched: it "
            f"{_action_clause(view)}.",
            "run it again on a less loaded machine; if it repeats, open an issue with the "
            "redacted report.",
            "70-footprint-readings-lost",
        )
    if view.mode == "observe" and view.at_ms is not None:
        return _Lines(
            f"{lost}, so observation ended at {_format_duration(view.at_ms)}. Observe mode "
            "does not stop the command, so it may still be running.",
            "check whether the command is still running and stop it yourself if you need to, "
            "then observe again.",
            "70-footprint-readings-lost",
        )
    return None


def _internal_failure(view: _View) -> _Lines:
    term = next(
        (
            record
            for record in view.signals
            if _is_group_signal(record, _SIGTERM) and record.result == _DELIVERED
        ),
        None,
    )
    sent = "" if term is None else f", and it {_sent_to_group(_SIGTERM, term.at_ms)}"
    return _Lines(
        f"mlx-guard hit an internal failure while supervising{sent}; the error line on stderr "
        "names it.",
        "check whether the command is still running, then run it again; if it repeats, open an "
        "issue with that error line and the redacted report.",
        "70-an-internal-failure",
    )


def _policy_intervention(view: _View) -> _Lines:
    for rule in (_emergency, _recorded_cause, _ignored_signal, _survivors):
        lines = rule(view)
        if lines is not None:
            return lines
    if view.checkpoint_reason is not None or any(
        record.reason is not None for record in view.signals
    ):
        return _Lines(
            f"mlx-guard {_action_clause(view)}.{_checkpoint_clause(view)}",
            "read signals in the report to see what mlx-guard did; no limit was crossed.",
            "75-cause-not-recorded",
        )
    return _Lines(
        f"mlx-guard {_action_clause(view)}, and this report does not record which limit caused "
        f"it.{_checkpoint_clause(view)}",
        "read signals and transitions in the report to see which limit was crossed.",
        "75-cause-not-recorded",
    )


def _emergency(view: _View) -> _Lines | None:
    transition = next(
        (
            transition
            for transition in view.transitions
            if transition.source in ("normal", "warning") and transition.target == "emergency"
        ),
        None,
    )
    if transition is None:
        return None
    value = transition.footprint
    threshold = view.config("emergency_footprint_bytes")
    limit = view.config("max_footprint_bytes")
    kill = _first_group_signal(view, _SIGKILL)
    if value is None or threshold is None or limit is None or kill is None:
        return None
    at = _format_duration(kill.at_ms)
    if kill.result == _DELIVERED:
        action = (
            f"sent SIGKILL to the command's process group at once at {at}, with no checkpoint "
            "and no grace period"
        )
    elif kill.result == "process_missing":
        action = f"found the command already ended when it sent SIGKILL at {at}"
    else:
        return None
    value_text, threshold_text, limit_text = _compared_bytes(value, threshold, limit)
    return _Lines(
        f"one sample of the OS-accounted footprint, {value_text}, was at or above the emergency "
        f"threshold of {threshold_text}, which is {_format_bytes(max(threshold - limit, 0))} "
        f"more than your {limit_text} limit, so mlx-guard {action}.",
        "find what allocates that much between samples, or raise --max-footprint if that peak "
        "is expected; a shorter --sample-interval sees growth sooner.",
        "75-emergency-kill",
    )


def _recorded_cause(view: _View) -> _Lines | None:
    causes = ("footprint", "wall_time", "parent_exit")
    cause = view.checkpoint_reason
    if cause is None:
        cause = next(
            (record.reason for record in view.signals if record.reason in causes),
            None,
        )
    if cause == "footprint":
        return _footprint(view)
    if cause == "wall_time":
        return _wall_time(view)
    if cause == "parent_exit":
        return _parent_exit(view)
    return None


def _footprint(view: _View) -> _Lines | None:
    limit = view.config("max_footprint_bytes")
    samples = view.config("required_breach_samples")
    if limit is None or samples is None:
        return None
    breach = next(
        (
            transition
            for transition in view.transitions
            if transition.source == "warning"
            and transition.target in ("checkpoint_requested", "terminating")
        ),
        None,
    )
    if breach is not None and breach.footprint is not None:
        value_text, limit_text = _compared_bytes(breach.footprint, limit)
        measured = (
            f"reached {value_text}, at or above the {limit_text} limit you set, on {samples} "
            "samples in a row"
        )
    else:
        measured = (
            f"was at or above the {_format_bytes(limit)} limit you set on {samples} samples "
            "in a row"
        )
    return _Lines(
        f"the OS-accounted footprint {measured}, so mlx-guard {_action_clause(view)}."
        f"{_checkpoint_clause(view)}",
        "observe the command to see its peak, then fix the growth or raise --max-footprint"
        f"{_resume_clause(view)}.",
        "75-the-footprint-limit",
    )


def _wall_time(view: _View) -> _Lines | None:
    wall = view.config("wall_time_ms")
    if wall is None:
        return None
    return _Lines(
        f"the run reached the {_format_duration(wall)} wall-time limit you set, so mlx-guard "
        f"{_action_clause(view)}.{_checkpoint_clause(view)}",
        "raise --wall-time if the command needs longer, or find out why it ran longer than "
        f"expected{_resume_clause(view)}.",
        "75-the-wall-time-limit",
    )


def _parent_exit(view: _View) -> _Lines | None:
    parent = view.parent_exited_at_ms
    if parent is None:
        return None
    return _Lines(
        f"the process that started mlx-guard exited at {_format_duration(parent)}, and the "
        "default --on-parent-exit=terminate stops the command with it, so mlx-guard "
        f"{_action_clause(view)}.",
        "keep the launching process running until the run ends, or pass "
        "--on-parent-exit=detach to let the command outlive it.",
        "75-the-launching-process-exited",
    )


def _ignored_signal(view: _View) -> _Lines | None:
    """Both the KILL and an earlier forwarded signal must have reached the group."""
    signals = view.signals
    kill_index = next(
        (
            index
            for index, record in enumerate(signals)
            if _is_group_signal(record, _SIGKILL) and record.reason == _EXTERNAL
        ),
        None,
    )
    if kill_index is None or signals[kill_index].result != _DELIVERED:
        return None
    forwarded = next(
        (
            record
            for record in signals[:kill_index]
            if record.reason == _EXTERNAL and record.result == _DELIVERED
        ),
        None,
    )
    if forwarded is None:
        return None
    return _Lines(
        f"the command was still running after the {_signal_name(forwarded.signal)} that "
        "mlx-guard passed on to it, so mlx-guard sent SIGKILL to its process group at "
        f"{_format_duration(signals[kill_index].at_ms)}.",
        "nothing, if you meant to stop it; otherwise make the command exit when it receives "
        "SIGINT or SIGTERM.",
        "75-a-signal-the-command-ignored",
    )


def _survivors(view: _View) -> _Lines | None:
    if not any(record.reason == "root_exit_cleanup" for record in view.signals):
        return None
    kill = _first_group_signal(view, _SIGKILL)
    if kill is None:
        return None
    at = _format_duration(kill.at_ms)
    if kill.result == _DELIVERED:
        action = f"sent SIGKILL to them at {at}"
    elif kill.result == "process_missing":
        action = f"found them already ended when it sent SIGKILL at {at}"
    else:
        return None
    return _Lines(
        "the command exited, but processes it started were still running after SIGTERM, so "
        f"mlx-guard {action}; the command's own exit status is in the report under "
        "outcome.child_status.",
        "make the command wait for, or stop, the processes it starts before it exits.",
        "75-processes-left-behind",
    )
