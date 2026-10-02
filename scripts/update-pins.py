#!/usr/bin/env python3
"""Refresh downloaded-tool hashes and versions in the repository.

The script intentionally edits tracked files only; callers review its diff and
run CI before merging. GitHub release assets and Rust archives are downloaded
over HTTPS and hashed locally instead of trusting an unverified checksum value.
"""

import argparse
import hashlib
import json
import os
import re
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FILES = [
    ROOT / ".github/workflows/ci.yml",
    ROOT / ".github/workflows/release.yml",
    ROOT / "docs/security.md",
]


def fetch(url: str, token: str | None = None) -> bytes:
    headers = {"Accept": "application/vnd.github+json", "User-Agent": "dbxctl-pin-updater"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=60) as response:
        return response.read()


def digest(url: str) -> str:
    return hashlib.sha256(fetch(url)).hexdigest()


def replace(pattern: str, replacement: str, files: list[Path] | None = None) -> None:
    if files is None:
        files = FILES
    count = 0
    for path in files:
        text = path.read_text()
        updated, matches = re.subn(pattern, replacement, text)
        if matches:
            path.write_text(updated)
            count += matches
    if count == 0:
        raise RuntimeError(f"pin pattern not found: {pattern}")


def replace_value(name: str, value: str) -> None:
    pattern = rf"{re.escape(name)}:\s*([0-9a-f]{{64}})"
    old_values = {
        match.group(1)
        for path in FILES
        for match in re.finditer(pattern, path.read_text())
    }
    if not old_values:
        raise RuntimeError(f"pin variable not found: {name}")
    for old_value in old_values:
        replace(re.escape(old_value), value)


def github_release(repo: str, tag: str, token: str | None) -> dict:
    encoded = urllib.parse.quote(tag, safe="")
    url = f"https://api.github.com/repos/{repo}/releases/tags/{encoded}"
    # Databricks organization SSO can reject unrelated authenticated tokens;
    # public release metadata needs no authentication.
    return json.loads(fetch(url, None if repo == "databricks/cli" else token))


def asset_url(release: dict, name: str) -> str:
    for asset in release["assets"]:
        if asset["name"] == name:
            return asset["browser_download_url"]
    raise RuntimeError(f"release asset not found: {name}")


def rust_image_digest(version: str) -> str:
    token_url = "https://auth.docker.io/token?" + urllib.parse.urlencode(
        {"service": "registry.docker.io", "scope": "repository:library/rust:pull"}
    )
    token = json.loads(fetch(token_url))["token"]
    request = urllib.request.Request(
        f"https://registry-1.docker.io/v2/library/rust/manifests/{version}-bookworm",
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json",
        },
    )
    with urllib.request.urlopen(request, timeout=60) as response:
        index = json.load(response)
    for manifest in index["manifests"]:
        platform = manifest.get("platform", {})
        if platform.get("os") == "linux" and platform.get("architecture") == "amd64":
            return manifest["digest"].removeprefix("sha256:")
    raise RuntimeError("rust image has no linux/amd64 manifest")


def update_rust(version: str, date: str) -> None:
    base = f"https://static.rust-lang.org/dist/{date}"
    components = {
        "CLIPPY_SHA256": f"clippy-{version}-x86_64-unknown-linux-gnu.tar.xz",
        "LLVM_TOOLS_SHA256": f"llvm-tools-{version}-x86_64-unknown-linux-gnu.tar.xz",
        "RUSTFMT_SHA256": f"rustfmt-{version}-x86_64-unknown-linux-gnu.tar.xz",
        "MACOS_ARM64_CARGO_SHA256": f"cargo-{version}-aarch64-apple-darwin.tar.xz",
        "MACOS_ARM64_RUSTC_SHA256": f"rustc-{version}-aarch64-apple-darwin.tar.xz",
        "MACOS_ARM64_STD_SHA256": f"rust-std-{version}-aarch64-apple-darwin.tar.xz",
        "MACOS_X64_CARGO_SHA256": f"cargo-{version}-x86_64-apple-darwin.tar.xz",
        "MACOS_X64_RUSTC_SHA256": f"rustc-{version}-x86_64-apple-darwin.tar.xz",
        "MACOS_X64_STD_SHA256": f"rust-std-{version}-x86_64-apple-darwin.tar.xz",
        "WINDOWS_X64_CARGO_SHA256": f"cargo-{version}-x86_64-pc-windows-msvc.tar.xz",
        "WINDOWS_X64_RUSTC_SHA256": f"rustc-{version}-x86_64-pc-windows-msvc.tar.xz",
        "WINDOWS_X64_STD_SHA256": f"rust-std-{version}-x86_64-pc-windows-msvc.tar.xz",
    }
    for variable, archive in components.items():
        value = fetch(f"{base}/{archive}.sha256").decode().split()[0]
        replace_value(variable, value)

    current = re.search(r'channel = "([0-9.]+)"', (ROOT / "rust-toolchain.toml").read_text()).group(1)
    current_date = re.search(r"static\.rust-lang\.org/dist/([0-9-]+)/", FILES[0].read_text()).group(1)
    for path in [*FILES, ROOT / "rust-toolchain.toml", ROOT / "Cargo.toml", ROOT / "README.md", ROOT / "docs/development.md"]:
        text = path.read_text().replace(current, version).replace(current_date, date)
        path.write_text(text)
    image_digest = rust_image_digest(version)
    replace(r"(rust:[0-9.]+-bookworm@sha256:)[0-9a-f]{64}", rf"\g<1>{image_digest}")


def update_github_tools(args: argparse.Namespace) -> None:
    token = args.github_token
    tools = [
        ("taiki-e/cargo-llvm-cov", f"v{args.llvm_cov}", "TOOL_SHA256", "cargo-llvm-cov-x86_64-unknown-linux-gnu.tar.gz", r"v[0-9.]+/cargo-llvm-cov"),
        ("rustsec/rustsec", f"cargo-audit/v{args.audit}", "AUDIT_SHA256", f"cargo-audit-x86_64-unknown-linux-musl-v{args.audit}.tgz", r"cargo-audit/v[0-9.]+/cargo-audit"),
        ("EmbarkStudios/cargo-deny", args.deny, "DENY_SHA256", f"cargo-deny-{args.deny}-x86_64-unknown-linux-musl.tar.gz", r"download/[0-9.]+/cargo-deny-[0-9.]+"),
    ]
    for repo, tag, variable, asset, version_pattern in tools:
        release = github_release(repo, tag, token)
        replace_value(variable, digest(asset_url(release, asset)))
        replacement = (
            f"v{args.llvm_cov}/cargo-llvm-cov" if variable == "TOOL_SHA256" else
            f"cargo-audit/v{args.audit}/cargo-audit" if variable == "AUDIT_SHA256" else
            f"download/{args.deny}/cargo-deny-{args.deny}"
        )
        replace(version_pattern, replacement)
    replace(r"cargo-audit-x86_64-unknown-linux-musl-v[0-9.]+\.tgz", f"cargo-audit-x86_64-unknown-linux-musl-v{args.audit}.tgz")
    replace(r"(\| cargo-llvm-cov \| )[0-9.]+", rf"\g<1>{args.llvm_cov}")
    replace(r"(\| cargo-audit \| )[0-9.]+", rf"\g<1>{args.audit}")
    replace(r"(\| cargo-deny \| )[0-9.]+", rf"\g<1>{args.deny}")

    syft = github_release("anchore/syft", f"v{args.syft}", token)
    linux_asset = f"syft_{args.syft}_linux_amd64.tar.gz"
    replace_value("SYFT_SHA256", digest(asset_url(syft, linux_asset)))
    for platform in ("darwin_arm64", "darwin_amd64", "windows_amd64"):
        asset = f"syft_{args.syft}_{platform}." + ("zip" if platform.startswith("windows") else "tar.gz")
        value = digest(asset_url(syft, asset))
        replace(
            rf"(syft_archive=syft_[0-9.]+_{platform}\.(?:tar\.gz|zip)\n\s+syft_sha256=)[0-9a-f]{{64}}",
            rf"\g<1>{value}",
        )
    replace(r"syft_[0-9.]+_", f"syft_{args.syft}_")
    replace(r"syft/releases/download/v[0-9.]+", f"syft/releases/download/v{args.syft}")
    replace(r"Syft [0-9]+(?:\.[0-9]+)+", f"Syft {args.syft}")

    db = github_release("databricks/cli", f"v{args.databricks}", token)
    db_asset = f"databricks_cli_{args.databricks}_linux_amd64.tar.gz"
    db_hash = digest(asset_url(db, db_asset))
    replace(r"databricks/cli/releases/download/v[0-9.]+/databricks_cli_[0-9.]+_linux_amd64", f"databricks/cli/releases/download/v{args.databricks}/databricks_cli_{args.databricks}_linux_amd64")
    replace(r"(?m)^\s*echo '[0-9a-f]{64}  /tmp/databricks-cli\.tar\.gz'", f"          echo '{db_hash}  /tmp/databricks-cli.tar.gz'")
    replace(r"Databricks CLI \| [0-9.]+, Linux amd64 \| `[0-9a-f]{64}`", f"Databricks CLI | {args.databricks}, Linux amd64 | `{db_hash}`")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rust", required=True)
    parser.add_argument("--rust-date", required=True)
    parser.add_argument("--llvm-cov", required=True)
    parser.add_argument("--audit", required=True)
    parser.add_argument("--deny", required=True)
    parser.add_argument("--syft", required=True)
    parser.add_argument("--databricks", required=True)
    parser.add_argument("--github-token", default=os.environ.get("GITHUB_TOKEN"))
    args = parser.parse_args()
    update_rust(args.rust, args.rust_date)
    update_github_tools(args)


if __name__ == "__main__":
    main()
