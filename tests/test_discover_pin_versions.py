import importlib.util
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).parents[1] / "scripts" / "discover-pin-versions.py"
SPEC = importlib.util.spec_from_file_location("discover_pin_versions", SCRIPT)
discover = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(discover)


class DiscoveryTests(unittest.TestCase):
    def test_stable_rust_parses_version_and_release_date(self):
        manifest = b'date = "2026-01-01"\n[pkg.rust]\nversion = "1.99.0 (hash 2026-01-01)"\n[pkg.other]\n'
        with mock.patch.object(discover.update_pins, "fetch", return_value=manifest):
            self.assertEqual(discover.stable_rust(), ("1.99.0", "2026-01-01"))

    def test_stable_rust_fails_closed_on_changed_manifest(self):
        with mock.patch.object(discover.update_pins, "fetch", return_value=b"invalid"):
            with self.assertRaisesRegex(RuntimeError, "missing date or version"):
                discover.stable_rust()

    def test_audit_ignores_unrelated_rustsec_releases(self):
        releases = [{"tag_name": "cvss/v3.0.0"}, {"tag_name": "cargo-audit/v0.24.0"}]
        with mock.patch.object(discover.update_pins, "fetch", return_value=json.dumps(releases).encode()):
            self.assertEqual(discover.latest_audit_version("token"), "0.24.0")

    def test_databricks_discovery_omits_token(self):
        with mock.patch.object(discover.update_pins, "fetch", return_value=b'{"tag_name":"v1.2.3"}') as fetch:
            self.assertEqual(discover.latest_release_tag("databricks/cli", "token"), "v1.2.3")
        self.assertIsNone(fetch.call_args.args[1])

    def test_discover_normalizes_version_prefixes(self):
        with mock.patch.object(discover, "stable_rust", return_value=("1.99.0", "2026-01-01")), mock.patch.object(
            discover, "latest_audit_version", return_value="0.24.0"
        ), mock.patch.object(discover, "latest_release_tag", side_effect=["v0.10.0", "0.22.0", "v2.0.0", "v1.20.0"]):
            versions = discover.discover("token")
        self.assertEqual(
            versions,
            {
                "rust": "1.99.0",
                "rust_date": "2026-01-01",
                "llvm_cov": "0.10.0",
                "audit": "0.24.0",
                "deny": "0.22.0",
                "syft": "2.0.0",
                "databricks": "1.20.0",
            },
        )

    def test_main_appends_github_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            versions = {"rust": "1.99.0", "rust_date": "2026-01-01"}
            with mock.patch.object(discover, "discover", return_value=versions), mock.patch(
                "argparse.ArgumentParser.parse_args", return_value=type("Args", (), {"github_output": output})()
            ), mock.patch.dict(os.environ, {"GITHUB_TOKEN": "token"}):
                discover.main()
            self.assertEqual(output.read_text(), "rust=1.99.0\nrust_date=2026-01-01\n")


if __name__ == "__main__":
    unittest.main()
