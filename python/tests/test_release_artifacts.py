from __future__ import annotations

import base64
import hashlib
import importlib.util
import json
import tempfile
import unittest
import zipfile
from pathlib import Path
from types import ModuleType


def _load_sanitizer() -> ModuleType:
    path = Path(__file__).resolve().parents[2] / "scripts/sanitize_wheel_sbom.py"
    spec = importlib.util.spec_from_file_location("sanitize_wheel_sbom", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ReleaseArtifactTests(unittest.TestCase):
    def test_wheel_sbom_is_path_free_and_record_is_repaired(self) -> None:
        sanitizer = _load_sanitizer()
        with tempfile.TemporaryDirectory() as temporary:
            wheel = Path(temporary, "demo-1.0-py3-none-any.whl")
            sbom_name = "demo-1.0.dist-info/sboms/demo.cyclonedx.json"
            record_name = "demo-1.0.dist-info/RECORD"
            document = {
                "bomFormat": "CycloneDX",
                "specVersion": "1.5",
                "serialNumber": "urn:uuid:11111111-1111-1111-1111-111111111111",
                "metadata": {
                    "timestamp": "2026-08-12T00:00:00Z",
                    "component": {
                        "type": "application",
                        "bom-ref": "path+file:///Users/person/work/demo#1.0",
                        "name": "demo",
                        "version": "1.0",
                        "purl": "pkg:cargo/demo@1.0?download_url=file://.",
                    },
                },
                "components": [
                    {
                        "type": "library",
                        "bom-ref": "registry+https://example.invalid/index#dep@2.0",
                        "name": "dep",
                        "version": "2.0",
                        "purl": "pkg:cargo/dep@2.0",
                    }
                ],
                "dependencies": [
                    {
                        "ref": "path+file:///Users/person/work/demo#1.0",
                        "dependsOn": ["registry+https://example.invalid/index#dep@2.0"],
                    }
                ],
            }
            with zipfile.ZipFile(wheel, "w") as archive:
                archive.writestr("demo/__init__.py", "")
                archive.writestr(sbom_name, json.dumps(document))
                archive.writestr(record_name, f"{sbom_name},sha256=old,1\n{record_name},,\n")

            sanitizer.sanitize_wheel(wheel)
            first = wheel.read_bytes()
            sanitizer.sanitize_wheel(wheel)
            self.assertEqual(wheel.read_bytes(), first)

            with zipfile.ZipFile(wheel) as archive:
                sbom_bytes = archive.read(sbom_name)
                sbom = json.loads(sbom_bytes)
                record = archive.read(record_name).decode()

            rendered = json.dumps(sbom, sort_keys=True)
            self.assertNotIn("/Users/", rendered)
            self.assertNotIn("path+file:", rendered)
            self.assertNotIn("download_url=file:", rendered)
            self.assertNotIn("timestamp", sbom["metadata"])
            self.assertEqual(sbom["metadata"]["component"]["bom-ref"], "pkg:cargo/demo@1.0")
            self.assertEqual(sbom["dependencies"][0]["ref"], "pkg:cargo/demo@1.0")

            digest = base64.urlsafe_b64encode(hashlib.sha256(sbom_bytes).digest()).rstrip(b"=")
            expected = f"{sbom_name},sha256={digest.decode()},{len(sbom_bytes)}"
            self.assertIn(expected, record.splitlines())


if __name__ == "__main__":
    unittest.main()
