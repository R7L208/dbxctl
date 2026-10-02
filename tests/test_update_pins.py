import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).parents[1] / "scripts" / "update-pins.py"
SPEC = importlib.util.spec_from_file_location("update_pins", SCRIPT)
update_pins = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(update_pins)


class Response(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


class RepositoryFixture(unittest.TestCase):
    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary_directory.name)
        (self.root / ".github/workflows").mkdir(parents=True)
        (self.root / "docs").mkdir()
        self.ci = self.root / ".github/workflows/ci.yml"
        self.release = self.root / ".github/workflows/release.yml"
        self.security = self.root / "docs/security.md"
        self.files = [self.ci, self.release, self.security]
        self.patchers = [
            mock.patch.object(update_pins, "ROOT", self.root),
            mock.patch.object(update_pins, "FILES", self.files),
        ]
        for patcher in self.patchers:
            patcher.start()

    def tearDown(self):
        for patcher in reversed(self.patchers):
            patcher.stop()
        self.temporary_directory.cleanup()

    def write_supporting_files(self):
        (self.root / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.89.0"\n')
        (self.root / "Cargo.toml").write_text('rust-version = "1.89"\n')
        (self.root / "README.md").write_text("Rust 1.89.0\n")
        (self.root / "docs/development.md").write_text("Rust 1.89.0\n")


class ReplacementTests(RepositoryFixture):
    def test_replace_updates_every_matching_file(self):
        self.ci.write_text("pin=old\n")
        self.release.write_text("pin=old\n")
        self.security.write_text("unrelated\n")

        update_pins.replace("old", "new")

        self.assertEqual(self.ci.read_text(), "pin=new\n")
        self.assertEqual(self.release.read_text(), "pin=new\n")

    def test_replace_fails_closed_when_pattern_is_missing(self):
        for path in self.files:
            path.write_text("no expected pin\n")

        with self.assertRaisesRegex(RuntimeError, "pin pattern not found"):
            update_pins.replace("missing", "replacement")

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
        import hashlib

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


class CommandTests(unittest.TestCase):
    def test_main_dispatches_both_update_groups(self):
        args = argparse_namespace(
            rust="1.90.0",
            rust_date="2025-09-18",
            llvm_cov="1",
            audit="1",
            deny="1",
            syft="1",
            databricks="1",
            github_token=None,
        )
        with mock.patch("argparse.ArgumentParser.parse_args", return_value=args), mock.patch.object(
            update_pins, "update_rust"
        ) as update_rust, mock.patch.object(update_pins, "update_github_tools") as update_tools:
            update_pins.main()

        update_rust.assert_called_once_with("1.90.0", "2025-09-18")
        update_tools.assert_called_once_with(args)


class RustUpdateTests(RepositoryFixture):
    def test_rust_update_rewrites_versions_hashes_and_image_digest(self):
        variables = [
            "CLIPPY_SHA256",
            "LLVM_TOOLS_SHA256",
            "RUSTFMT_SHA256",
            "MACOS_ARM64_CARGO_SHA256",
            "MACOS_ARM64_RUSTC_SHA256",
            "MACOS_ARM64_STD_SHA256",
            "MACOS_X64_CARGO_SHA256",
            "MACOS_X64_RUSTC_SHA256",
            "MACOS_X64_STD_SHA256",
            "WINDOWS_X64_CARGO_SHA256",
            "WINDOWS_X64_RUSTC_SHA256",
            "WINDOWS_X64_STD_SHA256",
        ]
        self.ci.write_text(
            "\n".join(f"{name}: {'0' * 64}" for name in variables)
            + "\nhttps://static.rust-lang.org/dist/2025-08-07/tool-1.89.0.tar.xz\n"
            + f"image: rust:1.89.0-bookworm@sha256:{'1' * 64}\n"
        )
        self.release.write_text("Rust 1.89.0\n")
        self.security.write_text(f"rust:1.89.0-bookworm@sha256:{'1' * 64}\n")
        self.write_supporting_files()

        with mock.patch.object(update_pins, "fetch", return_value=("a" * 64 + "  archive\n").encode()), mock.patch.object(
            update_pins, "rust_image_digest", return_value="b" * 64
        ):
            update_pins.update_rust("1.90.0", "2025-09-18")

        combined = "".join(path.read_text() for path in self.files)
        self.assertNotIn("1.89.0", combined)
        self.assertNotIn("2025-08-07", combined)
        self.assertIn("CLIPPY_SHA256: " + "a" * 64, self.ci.read_text())
        self.assertIn("rust:1.90.0-bookworm@sha256:" + "b" * 64, combined)


class ToolUpdateTests(RepositoryFixture):
    def setUp(self):
        super().setUp()
        self.ci.write_text(
            f"TOOL_SHA256: {'0' * 64}\n"
            "https://github.com/taiki-e/cargo-llvm-cov/releases/download/v0.9.1/cargo-llvm-cov-x86_64-unknown-linux-gnu.tar.gz\n"
            f"AUDIT_SHA256: {'1' * 64}\n"
            "https://github.com/rustsec/rustsec/releases/download/cargo-audit/v0.22.2/cargo-audit-x86_64-unknown-linux-musl-v0.22.2.tgz\n"
            f"DENY_SHA256: {'2' * 64}\n"
            "https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny-0.20.2-x86_64-unknown-linux-musl.tar.gz\n"
            "https://github.com/databricks/cli/releases/download/v0.296.0/databricks_cli_0.296.0_linux_amd64.tar.gz\n"
            f"          echo '{'3' * 64}  /tmp/databricks-cli.tar.gz'\n"
        )
        self.release.write_text(
            f"SYFT_SHA256: {'4' * 64}\n"
            "https://github.com/anchore/syft/releases/download/v1.52.0/syft_1.52.0_linux_amd64.tar.gz\n"
            f"syft_archive=syft_1.52.0_darwin_arm64.tar.gz\n              syft_sha256={'5' * 64}\n"
            f"syft_archive=syft_1.52.0_darwin_amd64.tar.gz\n              syft_sha256={'6' * 64}\n"
            f"syft_archive=syft_1.52.0_windows_amd64.zip\n              syft_sha256={'7' * 64}\n"
        )
        self.security.write_text(
            f"| cargo-llvm-cov | 0.9.1 | `{'0' * 64}` |\n"
            f"| cargo-audit | 0.22.2 | `{'1' * 64}` |\n"
            f"| cargo-deny | 0.20.2 | `{'2' * 64}` |\n"
            "SBOM with checksum-pinned Syft 1.52.0.\n"
            f"| Databricks CLI | 0.296.0, Linux amd64 | `{'3' * 64}` |\n"
        )

    def test_tool_update_is_complete_and_idempotent(self):
        args = argparse_namespace(
            llvm_cov="0.9.2", audit="0.23.0", deny="0.21.0", syft="1.53.0", databricks="1.0.0", github_token="token"
        )

        def release(_repo, _tag, _token):
            names = [
                "cargo-llvm-cov-x86_64-unknown-linux-gnu.tar.gz",
                "cargo-audit-x86_64-unknown-linux-musl-v0.23.0.tgz",
                "cargo-deny-0.21.0-x86_64-unknown-linux-musl.tar.gz",
                "syft_1.53.0_linux_amd64.tar.gz",
                "syft_1.53.0_darwin_arm64.tar.gz",
                "syft_1.53.0_darwin_amd64.tar.gz",
                "syft_1.53.0_windows_amd64.zip",
                "databricks_cli_1.0.0_linux_amd64.tar.gz",
            ]
            return {"assets": [{"name": name, "browser_download_url": "https://assets/" + name} for name in names]}

        with mock.patch.object(update_pins, "github_release", side_effect=release), mock.patch.object(
            update_pins, "digest", side_effect=lambda url: hashlib_for(url)
        ):
            update_pins.update_github_tools(args)
            first = [path.read_text() for path in self.files]
            update_pins.update_github_tools(args)

        self.assertEqual(first, [path.read_text() for path in self.files])
        combined = "".join(first)
        for version in ("0.9.2", "0.23.0", "0.21.0", "1.53.0", "1.0.0"):
            self.assertIn(version, combined)
        for old in ("0.9.1", "0.22.2", "0.20.2", "1.52.0", "0.296.0"):
            self.assertNotIn(old, combined)
        self.assertIn("Syft 1.53.0.", combined)


def argparse_namespace(**values):
    class Namespace:
        pass

    result = Namespace()
    for name, value in values.items():
        setattr(result, name, value)
    return result


def hashlib_for(value):
    import hashlib

    return hashlib.sha256(value.encode()).hexdigest()


if __name__ == "__main__":
    unittest.main()
