import copy
import hashlib
import importlib.util
import io
import json
import re
import tempfile
import unittest
from pathlib import Path
from unittest import mock


REPOSITORY = Path(__file__).parents[1]
SCRIPT = REPOSITORY / "scripts" / "update-pins.py"
SPEC = importlib.util.spec_from_file_location("update_pins", SCRIPT)
update_pins = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(update_pins)

REAL_PINS = json.loads((REPOSITORY / ".github/pins.json").read_text())


class Response(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


def leaves(value):
    if isinstance(value, dict):
        for child in value.values():
            yield from leaves(child)
    else:
        yield value


def hashlib_for(value):
    return hashlib.sha256(value.encode()).hexdigest()


def argparse_namespace(**values):
    class Namespace:
        pass

    result = Namespace()
    for name, value in values.items():
        setattr(result, name, value)
    return result


class RepositoryFixture(unittest.TestCase):
    """A scratch repository with the real pins and docs that mention them."""

    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary_directory.name)
        for directory in (".github/workflows", "docs"):
            (self.root / directory).mkdir(parents=True)
        self.pins_file = self.root / ".github/pins.json"
        self.security = self.root / "docs/security.md"
        self.pins = copy.deepcopy(REAL_PINS)
        self.pins_file.write_text(json.dumps(self.pins, indent=2, sort_keys=True) + "\n")
        rust, tools = self.pins["rust"], self.pins["tools"]
        self.security.write_text(
            f"rust:{rust['version']}-bookworm@sha256:{rust['image_digest']}\n"
            f"| cargo-llvm-cov | {tools['cargo-llvm-cov']['version']} | `{tools['cargo-llvm-cov']['sha256']}` |\n"
            f"| cargo-audit | {tools['cargo-audit']['version']}, musl | `{tools['cargo-audit']['sha256']}` |\n"
            f"| cargo-deny | {tools['cargo-deny']['version']}, musl | `{tools['cargo-deny']['sha256']}` |\n"
            f"| Databricks CLI | {self.pins['databricks_cli']['version']}, Linux amd64 | "
            f"`{self.pins['databricks_cli']['sha256']}` |\n"
            f"| Clippy | `{rust['components']['clippy']}` |\n"
            f"| x86_64-apple-darwin | `{rust['standalone']['x86_64-apple-darwin']['cargo']}` |\n"
            f"Rust {rust['version']} archives from {rust['dist_date']}; checksum-pinned "
            f"Syft {self.pins['syft']['version']}.\n"
        )
        (self.root / "rust-toolchain.toml").write_text(f'[toolchain]\nchannel = "{rust["version"]}"\n')
        (self.root / "README.md").write_text(f"- Rust {rust['version']} for source builds\n")
        (self.root / "docs/development.md").write_text(f"Rust {rust['version']} is pinned.\n")
        # A workflow that mentions every pin. The updater must leave it alone.
        self.workflow = self.root / ".github/workflows/ci.yml"
        self.workflow.write_text("\n".join(str(value) for value in leaves(self.pins)) + "\n")
        self.patchers = [
            mock.patch.object(update_pins, "ROOT", self.root),
            mock.patch.object(update_pins, "PINS", self.pins_file),
            mock.patch.object(update_pins, "DOCS", [self.security]),
        ]
        for patcher in self.patchers:
            patcher.start()

    def tearDown(self):
        for patcher in reversed(self.patchers):
            patcher.stop()
        self.temporary_directory.cleanup()

    def snapshot(self):
        return {
            path.relative_to(self.root).as_posix(): path.read_text()
            for path in sorted(self.root.rglob("*"))
            if path.is_file()
        }


class SchemaTests(RepositoryFixture):
    def test_committed_pins_file_is_valid(self):
        update_pins.validate(REAL_PINS)

    def test_validate_rejects_missing_and_extra_keys(self):
        missing = copy.deepcopy(REAL_PINS)
        del missing["syft"]
        with self.assertRaisesRegex(RuntimeError, "exactly the keys"):
            update_pins.validate(missing)
        extra = copy.deepcopy(REAL_PINS)
        extra["tools"]["unknown"] = {"sha256": "0" * 64, "version": "1.0.0"}
        with self.assertRaisesRegex(RuntimeError, r"pins\.tools must have exactly the keys"):
            update_pins.validate(extra)

    def test_validate_rejects_malformed_values(self):
        for path, bad in [
            (("databricks_cli", "sha256"), "0" * 63),
            (("rust", "version"), "latest"),
            (("rust", "dist_date"), "2025-8-7"),
            (("syft", "sha256", "linux_amd64"), ""),
            (("tools", "cargo-deny", "version"), 1),
        ]:
            pins = copy.deepcopy(REAL_PINS)
            node = pins
            for key in path[:-1]:
                node = node[key]
            node[path[-1]] = bad
            with self.assertRaisesRegex(RuntimeError, "is not a valid pin"):
                update_pins.validate(pins)

    def test_validate_rejects_non_object_sections(self):
        pins = copy.deepcopy(REAL_PINS)
        pins["rust"]["components"] = []
        with self.assertRaisesRegex(RuntimeError, "components must have exactly the keys"):
            update_pins.validate(pins)

    def test_save_is_sorted_indented_and_newline_terminated(self):
        update_pins.save_pins(update_pins.load_pins())
        text = self.pins_file.read_text()
        self.assertEqual(text, json.dumps(REAL_PINS, indent=2, sort_keys=True) + "\n")

    def test_save_refuses_invalid_pins(self):
        pins = copy.deepcopy(REAL_PINS)
        pins["rust"]["image_digest"] = "not-a-digest"
        before = self.pins_file.read_text()
        with self.assertRaises(RuntimeError):
            update_pins.save_pins(pins)
        self.assertEqual(self.pins_file.read_text(), before)


class OutputTests(RepositoryFixture):
    def test_rust_image_combines_version_and_digest(self):
        self.assertEqual(
            update_pins.rust_image(REAL_PINS),
            f"rust:{REAL_PINS['rust']['version']}-bookworm@sha256:{REAL_PINS['rust']['image_digest']}",
        )

    def test_emit_github_output_writes_compact_json_and_image(self):
        output = self.root / "github-output"
        output.write_text("existing=1\n")
        update_pins.emit_github_output(output)
        lines = output.read_text().splitlines()
        self.assertEqual(lines[0], "existing=1")
        self.assertTrue(lines[1].startswith("json={"))
        self.assertEqual(json.loads(lines[1].removeprefix("json=")), REAL_PINS)
        self.assertEqual(lines[2], "rust_image=" + update_pins.rust_image(REAL_PINS))

    def test_emit_github_output_fails_closed_on_invalid_pins(self):
        self.pins_file.write_text('{"rust": {}}\n')
        with self.assertRaises(RuntimeError):
            update_pins.emit_github_output(self.root / "github-output")


class ReplacementTests(RepositoryFixture):
    def test_replace_updates_documentation(self):
        update_pins.replace(r"Syft [0-9]+(?:\.[0-9]+)+", "Syft 9.9.9")
        self.assertIn("Syft 9.9.9.", self.security.read_text())

    def test_replace_fails_closed_when_pattern_is_missing(self):
        with self.assertRaisesRegex(RuntimeError, "pin pattern not found"):
            update_pins.replace("missing", "replacement")

    def test_replace_hash_updates_documented_checksums_only(self):
        documented = self.pins["rust"]["components"]["clippy"]
        update_pins.replace_hash(documented, "e" * 64)
        self.assertIn("e" * 64, self.security.read_text())
        before = self.security.read_text()
        update_pins.replace_hash("f" * 64, "0" * 64)  # not documented: no change, no error
        self.assertEqual(self.security.read_text(), before)

    def test_asset_lookup_fails_when_expected_asset_is_missing(self):
        with self.assertRaisesRegex(RuntimeError, "release asset not found"):
            update_pins.asset_url({"assets": []}, "tool.tar.gz")


class ApiTests(unittest.TestCase):
    @mock.patch("urllib.request.urlopen")
    def test_fetch_sets_headers_and_returns_response_body(self, urlopen):
        urlopen.return_value = Response(b"body")

        self.assertEqual(update_pins.fetch("https://example.test", "token"), b"body")
        request = urlopen.call_args.args[0]
        self.assertEqual(request.headers["Authorization"], "Bearer token")

    @mock.patch.object(update_pins, "fetch", return_value=b"artifact")
    def test_digest_hashes_downloaded_bytes(self, _fetch):
        self.assertEqual(update_pins.digest("https://example.test/artifact"), hashlib.sha256(b"artifact").hexdigest())

    @mock.patch.object(update_pins, "fetch", return_value=b'{"assets": []}')
    def test_databricks_release_metadata_uses_public_unauthenticated_api(self, fetch):
        update_pins.github_release("databricks/cli", "v1.2.3", "secret-token")

        _, token = fetch.call_args.args
        self.assertIsNone(token)

    @mock.patch.object(update_pins, "fetch", return_value=b'{"assets": []}')
    def test_other_github_release_metadata_uses_available_token(self, fetch):
        update_pins.github_release("anchore/syft", "v1.2.3", "secret-token")

        _, token = fetch.call_args.args
        self.assertEqual(token, "secret-token")

    @mock.patch.object(update_pins, "fetch", return_value=b'{"token": "registry-token"}')
    @mock.patch("urllib.request.urlopen")
    def test_rust_image_selects_linux_amd64_manifest(self, urlopen, _fetch):
        index = {
            "manifests": [
                {"digest": "sha256:arm", "platform": {"os": "linux", "architecture": "arm64"}},
                {"digest": "sha256:wanted", "platform": {"os": "linux", "architecture": "amd64"}},
            ]
        }
        urlopen.return_value = Response(json.dumps(index).encode())

        self.assertEqual(update_pins.rust_image_digest("1.99.0"), "wanted")

    @mock.patch.object(update_pins, "fetch", return_value=b'{"token": "registry-token"}')
    @mock.patch("urllib.request.urlopen")
    def test_rust_image_fails_without_linux_amd64_manifest(self, urlopen, _fetch):
        index = {"manifests": [{"digest": "sha256:arm", "platform": {"os": "linux", "architecture": "arm64"}}]}
        urlopen.return_value = Response(json.dumps(index).encode())

        with self.assertRaisesRegex(RuntimeError, "no linux/amd64"):
            update_pins.rust_image_digest("1.99.0")


class RustUpdateTests(RepositoryFixture):
    def test_rust_update_rewrites_pins_and_docs(self):
        old = self.pins["rust"]
        with mock.patch.object(update_pins, "fetch", return_value=("a" * 64 + "  archive\n").encode()), mock.patch.object(
            update_pins, "rust_image_digest", return_value="b" * 64
        ):
            pins = update_pins.load_pins()
            update_pins.update_rust(pins, "1.99.0", "2026-01-01")
            update_pins.save_pins(pins)

        saved = json.loads(self.pins_file.read_text())["rust"]
        self.assertEqual((saved["version"], saved["dist_date"], saved["image_digest"]), ("1.99.0", "2026-01-01", "b" * 64))
        self.assertEqual(set(saved["components"].values()), {"a" * 64})
        self.assertEqual({value for target in saved["standalone"].values() for value in target.values()}, {"a" * 64})
        docs = self.security.read_text()
        self.assertIn("rust:1.99.0-bookworm@sha256:" + "b" * 64, docs)
        self.assertNotIn(old["components"]["clippy"], docs)
        self.assertNotIn(old["dist_date"], docs)
        self.assertIn('channel = "1.99.0"', (self.root / "rust-toolchain.toml").read_text())
        self.assertIn("Rust 1.99.0", (self.root / "README.md").read_text())


class ToolUpdateTests(RepositoryFixture):
    ARGS = dict(llvm_cov="0.9.9", audit="0.29.0", deny="0.29.1", syft="1.99.0", databricks="1.13.0", github_token="token")

    @staticmethod
    def release(_repo, _tag, _token):
        names = [
            "cargo-llvm-cov-x86_64-unknown-linux-gnu.tar.gz",
            "cargo-audit-x86_64-unknown-linux-musl-v0.29.0.tgz",
            "cargo-deny-0.29.1-x86_64-unknown-linux-musl.tar.gz",
            "syft_1.99.0_linux_amd64.tar.gz",
            "syft_1.99.0_darwin_arm64.tar.gz",
            "syft_1.99.0_darwin_amd64.tar.gz",
            "syft_1.99.0_windows_amd64.zip",
            "databricks_cli_1.13.0_linux_amd64.tar.gz",
        ]
        return {"assets": [{"name": name, "browser_download_url": "https://assets/" + name} for name in names]}

    def run_update(self):
        with mock.patch.object(update_pins, "github_release", side_effect=self.release), mock.patch.object(
            update_pins, "digest", side_effect=hashlib_for
        ):
            pins = update_pins.load_pins()
            update_pins.update_github_tools(pins, argparse_namespace(**self.ARGS))
            update_pins.save_pins(pins)

    def test_tool_update_is_complete_and_idempotent(self):
        self.run_update()
        first = self.snapshot()
        self.run_update()
        self.assertEqual(first, self.snapshot())

        saved = json.loads(self.pins_file.read_text())
        self.assertEqual(saved["tools"]["cargo-audit"], {
            "sha256": hashlib_for("https://assets/cargo-audit-x86_64-unknown-linux-musl-v0.29.0.tgz"),
            "version": "0.29.0",
        })
        self.assertEqual(saved["syft"]["version"], "1.99.0")
        self.assertEqual(saved["syft"]["sha256"]["windows_amd64"], hashlib_for("https://assets/syft_1.99.0_windows_amd64.zip"))
        docs = self.security.read_text()
        self.assertIn("| cargo-deny | 0.29.1, musl |", docs)
        self.assertIn("Syft 1.99.0.", docs)
        self.assertIn(saved["databricks_cli"]["sha256"], docs)


class BoundaryTests(RepositoryFixture):
    """The updater must never edit workflow files (issue #58)."""

    ALLOWED = {".github/pins.json", "docs/security.md", "docs/development.md", "README.md", "rust-toolchain.toml"}

    def test_full_update_writes_only_allowed_files(self):
        before = self.snapshot()
        args = argparse_namespace(
            rust="1.99.0", rust_date="2026-01-01", **ToolUpdateTests.ARGS,
        )
        with mock.patch("argparse.ArgumentParser.parse_args", return_value=args), mock.patch.object(
            update_pins, "fetch", return_value=("c" * 64 + "  archive\n").encode()
        ), mock.patch.object(update_pins, "rust_image_digest", return_value="d" * 64), mock.patch.object(
            update_pins, "github_release", side_effect=ToolUpdateTests.release
        ), mock.patch.object(update_pins, "digest", side_effect=hashlib_for):
            args.emit_github_output = None
            update_pins.main()
        after = self.snapshot()

        changed = {path for path in before if before[path] != after[path]}
        self.assertTrue(changed, "the update should change something")
        self.assertLessEqual(changed, self.ALLOWED)
        self.assertEqual(before[".github/workflows/ci.yml"], after[".github/workflows/ci.yml"])

    def test_repository_workflows_contain_no_pinned_values(self):
        values = {str(value) for value in leaves(REAL_PINS)}
        for workflow in sorted((REPOSITORY / ".github/workflows").glob("*.yml")):
            text = workflow.read_text()
            with self.subTest(workflow=workflow.name):
                for value in values:
                    self.assertNotIn(value, text, f"{workflow.name} hard-codes a pin; move it to .github/pins.json")
                # Actions are pinned by 40-character commit SHAs; a 64-character
                # value is a checksum or image digest and belongs in pins.json.
                self.assertIsNone(re.search(r"(?<![0-9a-f])[0-9a-f]{64}(?![0-9a-f])", text))
                self.assertNotIn("@sha256:", text)


class CommandTests(RepositoryFixture):
    def test_main_emit_mode_only_writes_outputs(self):
        output = self.root / "github-output"
        args = argparse_namespace(
            emit_github_output=output, rust=None, rust_date=None, llvm_cov=None, audit=None,
            deny=None, syft=None, databricks=None, github_token=None,
        )
        before = self.snapshot()
        with mock.patch("argparse.ArgumentParser.parse_args", return_value=args):
            update_pins.main()
        self.assertIn("rust_image=", output.read_text())
        after = self.snapshot()
        del after["github-output"]
        self.assertEqual(before, after)

    def test_main_requires_every_version_outside_emit_mode(self):
        with mock.patch("sys.argv", ["update-pins.py", "--rust", "1.99.0"]), mock.patch("sys.stderr", io.StringIO()) as stderr:
            with self.assertRaises(SystemExit):
                update_pins.main()
        self.assertIn("--rust-date", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
