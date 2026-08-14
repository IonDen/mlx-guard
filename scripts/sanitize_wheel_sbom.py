#!/usr/bin/env python3
"""Remove checkout paths from a maturin CycloneDX SBOM inside a wheel."""

from __future__ import annotations

import argparse
import base64
import csv
import hashlib
import io
import json
import os
import re
import tempfile
import uuid
import zipfile
from pathlib import Path
from typing import Any
from urllib.parse import quote

_LOCAL_REFERENCE = re.compile(r"(?:path\+)?file://|/(?:Users|home|private|tmp)/")


def _sanitize_purl(value: str) -> str:
    base_and_query, separator, fragment = value.partition("#")
    base, query_separator, query = base_and_query.partition("?")
    if query_separator:
        kept = [field for field in query.split("&") if not field.startswith("download_url=file:")]
        base_and_query = base + (f"?{'&'.join(kept)}" if kept else "")
    return base_and_query + (separator + fragment if separator else "")


def _component_ref(component: dict[str, Any]) -> str:
    purl = component.get("purl")
    if isinstance(purl, str):
        return _sanitize_purl(purl)
    name = quote(str(component["name"]), safe="-._~")
    version = quote(str(component["version"]), safe="-._~")
    return f"pkg:cargo/{name}@{version}"


def _collect_reference_replacements(value: object, replacements: dict[str, str]) -> None:
    if isinstance(value, dict):
        old_ref = value.get("bom-ref")
        if (
            isinstance(old_ref, str)
            and _LOCAL_REFERENCE.search(old_ref)
            and "name" in value
            and "version" in value
        ):
            replacements[old_ref] = _component_ref(value)
        for child in value.values():
            _collect_reference_replacements(child, replacements)
    elif isinstance(value, list):
        for child in value:
            _collect_reference_replacements(child, replacements)


def _rewrite_references(value: object, replacements: dict[str, str]) -> object:
    if isinstance(value, dict):
        rewritten: dict[str, object] = {}
        for key, child in value.items():
            if key == "purl" and isinstance(child, str):
                rewritten[key] = _sanitize_purl(child)
            else:
                rewritten[key] = _rewrite_references(child, replacements)
        return rewritten
    if isinstance(value, list):
        return [_rewrite_references(child, replacements) for child in value]
    if isinstance(value, str):
        return replacements.get(value, value)
    return value


def _collect_component_references(value: object, references: set[str]) -> None:
    if isinstance(value, dict):
        reference = value.get("bom-ref")
        if isinstance(reference, str) and "name" in value and "version" in value:
            references.add(reference)
        for child in value.values():
            _collect_component_references(child, references)
    elif isinstance(value, list):
        for child in value:
            _collect_component_references(child, references)


def sanitize_document(document: dict[str, Any]) -> dict[str, Any]:
    """Return a deterministic CycloneDX document without local build paths."""
    replacements: dict[str, str] = {}
    _collect_reference_replacements(document, replacements)
    sanitized = _rewrite_references(document, replacements)
    if not isinstance(sanitized, dict):
        raise TypeError("sanitized SBOM must be a JSON object")
    metadata = sanitized.get("metadata")
    if isinstance(metadata, dict):
        metadata.pop("timestamp", None)
    references: set[str] = set()
    _collect_component_references(sanitized, references)
    sanitized["serialNumber"] = (
        f"urn:uuid:{uuid.uuid5(uuid.NAMESPACE_URL, '|'.join(sorted(references)))}"
    )
    rendered = json.dumps(sanitized, sort_keys=True)
    if _LOCAL_REFERENCE.search(rendered):
        raise ValueError("SBOM still contains a local filesystem reference")
    return sanitized


def _record_bytes(record: bytes, sbom_name: str, sbom_bytes: bytes) -> bytes:
    rows = list(csv.reader(io.StringIO(record.decode("utf-8"))))
    digest = base64.urlsafe_b64encode(hashlib.sha256(sbom_bytes).digest()).rstrip(b"=").decode()
    found = False
    for row in rows:
        if row and row[0] == sbom_name:
            row[:] = [sbom_name, f"sha256={digest}", str(len(sbom_bytes))]
            found = True
    if not found:
        raise ValueError("wheel RECORD does not name the embedded SBOM")
    output = io.StringIO(newline="")
    csv.writer(output, lineterminator="\n").writerows(rows)
    return output.getvalue().encode("utf-8")


def sanitize_wheel(wheel: Path) -> None:
    """Sanitize the embedded SBOM and repair RECORD in place."""
    with zipfile.ZipFile(wheel) as archive:
        entries = [(info, archive.read(info.filename)) for info in archive.infolist()]
    sboms = [name for info, _ in entries if (name := info.filename).endswith(".cyclonedx.json")]
    records = [name for info, _ in entries if (name := info.filename).endswith(".dist-info/RECORD")]
    if len(sboms) != 1 or len(records) != 1:
        raise ValueError("wheel must contain exactly one CycloneDX SBOM and one RECORD")
    sbom_name, record_name = sboms[0], records[0]
    contents = {info.filename: data for info, data in entries}
    document = json.loads(contents[sbom_name])
    if not isinstance(document, dict):
        raise ValueError("embedded SBOM must be a JSON object")
    sbom_bytes = (json.dumps(sanitize_document(document), indent=2, sort_keys=True) + "\n").encode()
    record_bytes = _record_bytes(contents[record_name], sbom_name, sbom_bytes)
    if contents[sbom_name] == sbom_bytes and contents[record_name] == record_bytes:
        return
    contents[sbom_name] = sbom_bytes
    contents[record_name] = record_bytes
    original_mode = wheel.stat().st_mode
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{wheel.name}.", dir=wheel.parent)
    os.close(descriptor)
    temporary = Path(temporary_name)
    try:
        with zipfile.ZipFile(temporary, "w") as archive:
            for info, _ in entries:
                archive.writestr(info, contents[info.filename])
        temporary.chmod(original_mode)
        os.replace(temporary, wheel)
    finally:
        temporary.unlink(missing_ok=True)


def main() -> int:
    """Run the wheel sanitizer command."""
    parser = argparse.ArgumentParser()
    parser.add_argument("wheel", type=Path)
    args = parser.parse_args()
    sanitize_wheel(args.wheel)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
