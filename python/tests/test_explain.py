"""Tests for the plain-outcome explanation carried by ``RunResult``.

The expected text comes from the shared golden corpus that the native explainer is also tested
against, so the two implementations cannot drift apart without one of these tests going red.
"""

import dataclasses
import importlib.util
import json
import sys
import unittest
from collections.abc import Mapping
from importlib.metadata import PackageNotFoundError
from pathlib import Path
from types import ModuleType
from typing import cast
from unittest import mock

import mlx_guard
from mlx_guard._client import JsonValue, _freeze_json

# Resolved from this file, not the CWD: the wheel proof runs the suite from a temp directory.
ROOT = Path(__file__).resolve().parents[2]
CORPUS = ROOT / "crates" / "mlx-guard-core" / "tests" / "fixtures" / "explain"
LINK_PREFIX = "https://github.com/IonDen/mlx-guard/blob/"
COLLISION_CODES = (64, 70, 74, 75, 126, 127)
QUIET_CHILD_CODES = (0, 1, 2, 23, 63, 65, 69, 71, 73, 76, 125, 128, 255)


def explain(payload: Mapping[str, JsonValue]) -> str | None:
    """Call the explainer; imported here so each test reports its own failure."""
    from mlx_guard._explain import explain_payload

    return explain_payload(payload)


def load_json(relative: str) -> dict[str, object]:
    decoded = json.loads((ROOT / relative).read_text(encoding="utf-8"))
    assert isinstance(decoded, dict)
    return cast(dict[str, object], decoded)


def freeze(decoded: Mapping[str, object]) -> Mapping[str, JsonValue]:
    """Freeze a decoded report the way ``load_report`` does (mappingproxy objects, tuples)."""
    frozen = _freeze_json(decoded)
    assert isinstance(frozen, Mapping)
    return frozen


def cases() -> list[dict[str, str]]:
    decoded = json.loads((CORPUS / "cases.json").read_text(encoding="utf-8"))
    assert isinstance(decoded, list)
    return cast(list[dict[str, str]], decoded)


def case(row: str) -> dict[str, str]:
    for candidate in cases():
        if candidate["row"] == row:
            return candidate
    raise AssertionError(f"row {row!r} is not in cases.json")


def corpus_report(row: str) -> dict[str, object]:
    return load_json(case(row)["report"])


def expected_text(row: str, version: str) -> str:
    return case(row)["text"].replace("{version}", version)


def as_dict(value: object) -> dict[str, object]:
    assert isinstance(value, dict)
    return cast(dict[str, object], value)


def as_list(value: object) -> list[object]:
    assert isinstance(value, list)
    return cast(list[object], value)


def sample_report(payload: Mapping[str, JsonValue]) -> mlx_guard.Report:
    return mlx_guard.Report(
        schema_version=1,
        package_version="0.1.0",
        outcome=mlx_guard.Outcome(kind=mlx_guard.OutcomeKind.POLICY_INTERVENTION, at_ms=40),
        payload=payload,
    )


class CorpusTests(unittest.TestCase):
    def test_every_corpus_row_matches(self) -> None:
        # Bug: the Python mirror drifts from the native golden text on any row (a word, a
        # number format, a clause order or the link).
        all_cases = cases()
        self.assertGreater(len(all_cases), 0, "cases.json lists no rows")
        for entry in all_cases:
            with self.subTest(row=entry["row"]):
                decoded = load_json(entry["report"])
                version = decoded["package_version"]
                assert isinstance(version, str)
                self.assertEqual(
                    explain(freeze(decoded)), entry["text"].replace("{version}", version)
                )

    def test_anchors_match_the_shared_list(self) -> None:
        # Bug: an anchor added, renamed or reordered in the Python ANCHORS without the shared
        # anchors.txt that the native list and the troubleshooting page are checked against.
        from mlx_guard._explain import ANCHORS

        listed = tuple((CORPUS / "anchors.txt").read_text(encoding="utf-8").splitlines())
        self.assertIsInstance(ANCHORS, tuple)
        self.assertEqual(ANCHORS, listed)
        for entry in cases():
            with self.subTest(row=entry["row"]):
                anchor = entry["text"].rsplit("#", 1)[1]
                self.assertIn(anchor, ANCHORS)

    def test_exit_zero_and_plain_child_failure_have_no_explanation(self) -> None:
        # Bug: a line produced on success or on a command's own non-reserved failure code.
        for name in ("report-quiet-exit-0.json", "report-quiet-child-1.json"):
            with self.subTest(fixture=name):
                decoded = load_json(str((CORPUS / name).relative_to(ROOT)))
                self.assertIsNone(explain(freeze(decoded)))

    def test_codes_next_to_reserved_ones_are_quiet_and_reserved_ones_are_explained(
        self,
    ) -> None:
        # Bug: the collision set compared with the wrong codes or with `<`/`>` instead of
        # membership, so 63/65, 69/71, 73/76 or 125/128 get lines, or 64..127 lose them.
        quiet = load_json(str((CORPUS / "report-quiet-child-1.json").relative_to(ROOT)))
        for code in QUIET_CHILD_CODES:
            with self.subTest(code=code):
                as_dict(quiet["outcome"])["code"] = code
                as_dict(as_dict(quiet["outcome"])["child_status"])["code"] = code
                self.assertIsNone(explain(freeze(quiet)))
        collision = corpus_report("C-collision")
        for code in COLLISION_CODES:
            with self.subTest(code=code):
                as_dict(collision["outcome"])["code"] = code
                as_dict(as_dict(collision["outcome"])["child_status"])["code"] = code
                text = explain(freeze(collision))
                assert text is not None, f"child code {code} must be explained"
                self.assertEqual(
                    text.split("\n")[0],
                    f"what happened: the command itself exited with code {code}, and mlx-guard "
                    f"did not intervene; {code} is also one of mlx-guard's own exit codes.",
                )
                self.assertTrue(text.endswith("#a-code-mlx-guard-also-uses"), text)


class FormattingTests(unittest.TestCase):
    """Number formatting the corpus does not reach, with values taken from the spec rules."""

    def test_byte_values_follow_the_integer_rules(self) -> None:
        # Bug: float formatting (`.2f`, `round()`) instead of integer hundredths, which renders
        # 1 073 741 823 bytes as "1024.00 MiB" instead of stepping up to "1.00 GiB".
        pairs = (
            (1023, 100, "1023 B", "100 B"),
            (1024, 100, "1.00 KiB", "100 B"),
            (1_048_575, 1024, "1.00 MiB", "1.00 KiB"),
            (1_073_741_823, 1024, "1.00 GiB", "1.00 KiB"),
            (6_499_453_256, 6_442_450_944, "6.05 GiB", "6.00 GiB"),
        )
        original = "reached 120 B, at or above the 100 B limit"
        for measured, limit, measured_text, limit_text in pairs:
            with self.subTest(measured=measured, limit=limit):
                decoded = corpus_report("75-footprint-timed-out")
                as_dict(decoded["configuration"])["max_footprint_bytes"] = limit
                for transition in as_list(decoded["transitions"]):
                    if as_dict(transition)["from"] == "warning":
                        as_dict(transition)["aggregate_footprint_bytes"] = measured
                expected = expected_text("75-footprint-timed-out", "0.1.0")
                self.assertIn(original, expected)
                self.assertEqual(
                    explain(freeze(decoded)),
                    expected.replace(
                        original, f"reached {measured_text}, at or above the {limit_text} limit"
                    ),
                )

    def test_equal_renders_of_different_values_print_both_in_bytes(self) -> None:
        # Bug: the equal-render rule missing, so "reached 6.00 GiB, at or above the 6.00 GiB
        # limit" hides that the two values differ.
        decoded = corpus_report("75-footprint-timed-out")
        as_dict(decoded["configuration"])["max_footprint_bytes"] = 6_442_450_944
        for transition in as_list(decoded["transitions"]):
            if as_dict(transition)["from"] == "warning":
                as_dict(transition)["aggregate_footprint_bytes"] = 6_442_450_945
        self.assertEqual(
            explain(freeze(decoded)),
            expected_text("75-footprint-timed-out", "0.1.0").replace(
                "reached 120 B, at or above the 100 B limit",
                "reached 6442450945 bytes, at or above the 6442450944 bytes limit",
            ),
        )

    def test_durations_follow_the_integer_rules(self) -> None:
        # Bug: float or banker's rounding in the duration formatter, or an off-by-one at the
        # 1 s, 60 s or 1 h unit boundaries.
        durations = (
            (13, "13 ms"),
            (999, "999 ms"),
            (1000, "1.0 s"),
            (39_964, "40.0 s"),
            (59_949, "59.9 s"),
            (59_960, "1 min"),
            (600_000, "10 min"),
            (3_599_499, "59 min 59 s"),
            (3_599_500, "1 h"),
            (7_384_200, "2 h 3 min"),
        )
        for ms, rendered in durations:
            with self.subTest(ms=ms):
                decoded = corpus_report("75-wall")
                as_dict(decoded["configuration"])["wall_time_ms"] = ms
                as_dict(as_list(decoded["signals"])[0])["at_ms"] = ms
                expected = expected_text("75-wall", "0.1.0")
                self.assertIn("the 30 ms wall-time limit", expected)
                self.assertIn("process group at 31 ms.", expected)
                self.assertEqual(
                    explain(freeze(decoded)),
                    expected.replace(
                        "the 30 ms wall-time limit", f"the {rendered} wall-time limit"
                    ).replace("process group at 31 ms.", f"process group at {rendered}."),
                )


class LinkVersionTests(unittest.TestCase):
    def test_link_version_comes_from_the_payload(self) -> None:
        # Bug: the explainer links the installed package version instead of the report's own.
        for entry in cases():
            with self.subTest(row=entry["row"]):
                decoded = load_json(entry["report"])
                decoded["package_version"] = "9.8.7"
                text = explain(freeze(decoded))
                self.assertEqual(text, entry["text"].replace("{version}", "9.8.7"))
                assert text is not None
                self.assertIn(f"{LINK_PREFIX}v9.8.7/docs/TROUBLESHOOTING.md#", text)

    def test_missing_package_version_falls_back_to_main(self) -> None:
        # Bug: a KeyError (or a "vNone" link) on a payload without a string package_version.
        for replacement in (None, 7):
            for entry in cases():
                with self.subTest(row=entry["row"], package_version=replacement):
                    decoded = load_json(entry["report"])
                    if replacement is None:
                        del decoded["package_version"]
                    else:
                        decoded["package_version"] = replacement
                    self.assertIn("blob/v{version}/", entry["text"])
                    self.assertEqual(
                        explain(freeze(decoded)),
                        entry["text"].replace("blob/v{version}/", "blob/main/"),
                    )

    def test_malformed_package_version_falls_back_to_main(self) -> None:
        # Bug: package_version interpolated without the shape check the native report reader
        # applies, so a path segment, a fragment, a newline or an oversized value lands inside
        # the link. `$` instead of a full match lets the trailing newline through.
        malformed = (
            "1.2.3/../x",
            "1.2.3\n",
            "1.2.3#frag",
            "1.2.3-" + "a" * 59,  # 65 bytes: one past the 64-byte bound
        )
        for value in malformed:
            for entry in cases():
                with self.subTest(row=entry["row"], package_version=value):
                    decoded = load_json(entry["report"])
                    decoded["package_version"] = value
                    self.assertEqual(
                        explain(freeze(decoded)),
                        entry["text"].replace("blob/v{version}/", "blob/main/"),
                    )

    def test_longest_valid_package_version_keeps_its_tag(self) -> None:
        # Bug: the 64-byte bound applied as `< 64`, so the longest valid version loses its tag.
        value = "1.2.3-" + "a" * 58  # 64 bytes, the largest valid length
        self.assertEqual(len(value.encode("utf-8")), 64)
        entry = case("75-wall")
        decoded = load_json(entry["report"])
        decoded["package_version"] = value
        self.assertEqual(explain(freeze(decoded)), entry["text"].replace("{version}", value))

    def test_explanation_is_total_without_package_metadata(self) -> None:
        # Bug: any distribution-metadata read inside the explainer (mlx_guard.__version__,
        # importlib.metadata, binary_version), at call time or cached when the module loads.
        missing = PackageNotFoundError("mlx-guard")
        with (
            mock.patch("importlib.metadata.version", side_effect=missing),
            mock.patch("importlib.metadata.distribution", side_effect=missing),
            mock.patch("importlib.metadata.metadata", side_effect=missing),
            mock.patch("mlx_guard.version", side_effect=missing),
            mock.patch("mlx_guard._binary.version", side_effect=missing),
            mock.patch("mlx_guard._binary.distribution", side_effect=missing),
        ):
            with self.assertRaises(PackageNotFoundError):
                _ = mlx_guard.__version__
            with fresh_explain_module() as fresh:
                for entry in cases():
                    with self.subTest(row=entry["row"]):
                        decoded = load_json(entry["report"])
                        version = decoded["package_version"]
                        assert isinstance(version, str)
                        expected = entry["text"].replace("{version}", version)
                        self.assertEqual(explain(freeze(decoded)), expected)
                        self.assertEqual(fresh.explain_payload(freeze(decoded)), expected)


class ShapeTests(unittest.TestCase):
    def test_unexpected_shapes_never_raise(self) -> None:
        # Bug: a subscript or attribute access instead of a checked `.get`, so a report section
        # of the wrong type raises KeyError, TypeError or AttributeError out of RunResult.
        # Failures are collected rather than reported per subTest: there are about 10 000 cases.
        replacements: tuple[object, ...] = (None, "x", 7, True, -1, [], {}, ["x"], [{}])
        problems: list[str] = []

        def check(label: str, broken: dict[str, object]) -> None:
            try:
                text = explain(freeze(broken))
            except Exception as error:  # Any exception is the bug under test.
                problems.append(f"{label}: raised {error!r}")
                return
            if text is not None and not (
                isinstance(text, str)
                and text.startswith("what happened: ")
                and text.count("\n") == 1
                and "\nwhat to do: " in text
            ):
                problems.append(f"{label}: returned {text!r}")

        for entry in cases():
            decoded = load_json(entry["report"])
            row = entry["row"]
            for key in ("configuration", "samples", "transitions", "signals", "checkpoint"):
                for replacement in replacements:
                    broken = json.loads(json.dumps(decoded))
                    broken[key] = replacement
                    check(f"{row} {key}={replacement!r}", broken)
            for section in ("configuration", "checkpoint", "outcome"):
                for field in as_dict(decoded[section]):
                    if section == "outcome" and field == "kind":
                        continue
                    for replacement in replacements:
                        broken = json.loads(json.dumps(decoded))
                        broken[section][field] = replacement
                        check(f"{row} {section}.{field}={replacement!r}", broken)
            for listed in ("signals", "transitions"):
                for index, record in enumerate(as_list(decoded[listed])):
                    for field in as_dict(record):
                        for replacement in replacements:
                            broken = json.loads(json.dumps(decoded))
                            broken[listed][index][field] = replacement
                            check(f"{row} {listed}[{index}].{field}={replacement!r}", broken)
        self.assertEqual(problems[:10], [], f"{len(problems)} malformed payloads failed")

    def test_a_policy_intervention_without_its_sections_still_explains_itself(self) -> None:
        # Bug: an unexpected shape returns None (or raises) instead of the row's fallback text,
        # so a 75 that reached the caller carries no explanation at all.
        decoded = corpus_report("75-wall")
        for key in ("configuration", "transitions", "signals", "checkpoint", "samples"):
            decoded[key] = "x"
        text = explain(freeze(decoded))
        assert text is not None
        self.assertTrue(text.startswith("what happened: "), text)
        self.assertIn("\nwhat to do: ", text)
        self.assertEqual(text.count("\n"), 1, text)
        self.assertIn(f"{LINK_PREFIX}v0.1.0/docs/TROUBLESHOOTING.md#75-", text)


class RunResultFieldTests(unittest.TestCase):
    def test_run_result_positional_construction_still_works(self) -> None:
        # Bug: the field added without a default or before the existing four, which breaks the
        # positional `RunResult(returncode, report, stdout, stderr)` that callers' fakes use.
        report = sample_report(freeze(corpus_report("75-wall")))
        result = mlx_guard.RunResult(75, report, b"out", b"err")
        self.assertIsNone(result.explanation)
        self.assertEqual(result.returncode, 75)
        self.assertIs(result.report, report)
        self.assertEqual((result.stdout, result.stderr), (b"out", b"err"))
        keyword = mlx_guard.RunResult(
            returncode=75,
            report=report,
            stdout=None,
            stderr=None,
            explanation="what happened: a\nwhat to do: b",
        )
        self.assertEqual(keyword.explanation, "what happened: a\nwhat to do: b")

    def test_explanation_is_keyword_only_and_frozen(self) -> None:
        # Bug: the field declared positional (a fifth positional argument accepted and
        # `__match_args__` widened), mutable, or outside the slots.
        fields = dataclasses.fields(mlx_guard.RunResult)
        self.assertEqual(
            [field.name for field in fields],
            ["returncode", "report", "stdout", "stderr", "explanation"],
        )
        self.assertTrue(fields[-1].kw_only)
        self.assertIsNone(fields[-1].default)
        self.assertEqual(
            mlx_guard.RunResult.__match_args__, ("returncode", "report", "stdout", "stderr")
        )
        report = sample_report(freeze(corpus_report("75-wall")))
        with self.assertRaises(TypeError):
            mlx_guard.RunResult(75, report, None, None, "text")  # type: ignore[misc]
        result = mlx_guard.RunResult(75, report, None, None, explanation="text")
        with self.assertRaises(dataclasses.FrozenInstanceError):
            result.explanation = "other"  # type: ignore[misc]
        self.assertIn("explanation", mlx_guard.RunResult.__slots__)
        self.assertFalse(hasattr(result, "__dict__"))
        self.assertEqual(dataclasses.replace(result, returncode=1).explanation, "text")

    def test_equality_and_repr_include_the_explanation(self) -> None:
        # Bug: the field excluded from comparison (`compare=False`) or from the repr, so two
        # results with different explanations compare equal.
        report = sample_report(freeze(corpus_report("75-wall")))
        first = mlx_guard.RunResult(75, report, None, None, explanation="one")
        self.assertEqual(first, mlx_guard.RunResult(75, report, None, None, explanation="one"))
        self.assertNotEqual(first, mlx_guard.RunResult(75, report, None, None, explanation="two"))
        self.assertNotEqual(first, mlx_guard.RunResult(75, report, None, None))
        self.assertTrue(repr(first).endswith(", explanation='one')"), repr(first))


class fresh_explain_module:  # A context manager, named to read like a function call.
    """Load a second copy of the explainer so module-level work runs under the caller's patches."""

    name = "mlx_guard._explain_without_metadata"

    def __enter__(self) -> ModuleType:
        path = Path(mlx_guard.__file__).with_name("_explain.py")
        spec = importlib.util.spec_from_file_location(self.name, path)
        assert spec is not None and spec.loader is not None
        module = importlib.util.module_from_spec(spec)
        sys.modules[self.name] = module
        spec.loader.exec_module(module)
        return module

    def __exit__(self, *_exc: object) -> None:
        sys.modules.pop(self.name, None)


if __name__ == "__main__":
    unittest.main()
