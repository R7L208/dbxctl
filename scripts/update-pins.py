#!/usr/bin/env python3
"""Refresh downloaded-tool hashes and versions in the repository.

Every supply-chain pin lives in `.github/pins.json`, which the CI and release
workflows read at run time. The updater rewrites that file and the
human-facing tables in the docs. It never edits `.github/workflows/`, so the
token that opens the update pull request does not need permission to change
workflows. Callers review the diff and run CI before merging. GitHub release
assets and Rust archives are downloaded over HTTPS and hashed locally instead
of trusting an unverified checksum value.

`--emit-github-output PATH` validates the pins file and writes it, and the
derived Rust image reference, as step outputs for the workflows' `pins` job.
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
PINS = ROOT / ".github/pins.json"
DOCS = [ROOT / "docs/security.md"]

HEX = re.compile(r"[0-9a-f]{64}")
VERSION = re.compile(r"[0-9]+(?:\.[0-9]+)+")
DATE = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}")
RUST_COMPONENTS = ("clippy", "llvm-tools", "rustfmt")
RUST_TARGETS = ("aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-pc-windows-msvc")
STANDALONE = ("cargo", "rust-std", "rustc")
SYFT_PLATFORMS = ("darwin_amd64", "darwin_arm64", "linux_amd64", "windows_amd64")
TOOLS = ("cargo-audit", "cargo-deny", "cargo-llvm-cov")

# The exact shape of the pins file. Leaves name the pattern their value must match.
SCHEMA = {
    "databricks_cli": {"sha256": HEX, "version": VERSION},
    "rust": {
        "components": dict.fromkeys(RUST_COMPONENTS, HEX),
        "dist_date": DATE,
        "image_digest": HEX,
        "standalone": {target: dict.fromkeys(STANDALONE, HEX) for target in RUST_TARGETS},
        "version": VERSION,
    },
    "syft": {"sha256": dict.fromkeys(SYFT_PLATFORMS, HEX), "version": VERSION},
    "tools": {tool: {"sha256": HEX, "version": VERSION} for tool in TOOLS},
}


def validate(value, schema=SCHEMA, path="pins") -> None:
    if isinstance(schema, dict):
        if not isinstance(value, dict) or set(value) != set(schema):
            raise RuntimeError(f"{path} must have exactly the keys {sorted(schema)}")
        for key, child in schema.items():
            validate(value[key], child, f"{path}.{key}")
    elif not isinstance(value, str) or not schema.fullmatch(value):
        raise RuntimeError(f"{path} is not a valid pin: {value!r}")


def load_pins() -> dict:
    pins = json.loads(PINS.read_text())
    validate(pins)
    return pins


def save_pins(pins: dict) -> None:
    validate(pins)
    PINS.write_text(json.dumps(pins, indent=2, sort_keys=True) + "\n")


def rust_image(pins: dict) -> str:
    return f"rust:{pins['rust']['version']}-bookworm@sha256:{pins['rust']['image_digest']}"


def emit_github_output(path: Path) -> None:
    pins = load_pins()
    with path.open("a") as output:
        output.write(f"json={json.dumps(pins, sort_keys=True, separators=(',', ':'))}\n")
        output.write(f"rust_image={rust_image(pins)}\n")


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
        files = DOCS
    count = 0
    for path in files:
        text = path.read_text()
        updated, matches = re.subn(pattern, replacement, text)
        if matches:
            path.write_text(updated)
            count += matches
    if count == 0:
        raise RuntimeError(f"pin pattern not found: {pattern}")


def replace_hash(old: str, new: str) -> None:
    """Keep documented checksums in step with the pins file.

    The docs list only some checksums, so a checksum that isn't documented is
    not an error; the pins file remains the source of truth.
    """
    for path in DOCS:
        text = path.read_text()
        if old in text:
            path.write_text(text.replace(old, new))


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


def update_rust(pins: dict, version: str, date: str) -> None:
    rust = pins["rust"]
    base = f"https://static.rust-lang.org/dist/{date}"

    def archive_hash(component: str, target: str) -> str:
        return fetch(f"{base}/{component}-{version}-{target}.tar.xz.sha256").decode().split()[0]

    for component in RUST_COMPONENTS:
        new = archive_hash(component, "x86_64-unknown-linux-gnu")
        replace_hash(rust["components"][component], new)
        rust["components"][component] = new
    for target in RUST_TARGETS:
        for component in STANDALONE:
            new = archive_hash(component, target)
            replace_hash(rust["standalone"][target][component], new)
            rust["standalone"][target][component] = new

    image_digest = rust_image_digest(version)
    replace(r"(rust:[0-9.]+-bookworm@sha256:)[0-9a-f]{64}", rf"\g<1>{image_digest}")
    rust["image_digest"] = image_digest

    current, current_date = rust["version"], rust["dist_date"]
    for path in [*DOCS, ROOT / "rust-toolchain.toml", ROOT / "README.md", ROOT / "docs/development.md"]:
        text = path.read_text().replace(current, version).replace(current_date, date)
        path.write_text(text)
    rust["version"], rust["dist_date"] = version, date


def update_github_tools(pins: dict, args: argparse.Namespace) -> None:
    token = args.github_token
    tools = [
        ("cargo-llvm-cov", "taiki-e/cargo-llvm-cov", f"v{args.llvm_cov}", args.llvm_cov,
         "cargo-llvm-cov-x86_64-unknown-linux-gnu.tar.gz"),
        ("cargo-audit", "rustsec/rustsec", f"cargo-audit/v{args.audit}", args.audit,
         f"cargo-audit-x86_64-unknown-linux-musl-v{args.audit}.tgz"),
        ("cargo-deny", "EmbarkStudios/cargo-deny", args.deny, args.deny,
         f"cargo-deny-{args.deny}-x86_64-unknown-linux-musl.tar.gz"),
    ]
    for name, repo, tag, version, asset in tools:
        new = digest(asset_url(github_release(repo, tag, token), asset))
        replace_hash(pins["tools"][name]["sha256"], new)
        replace(rf"(\| {re.escape(name)} \| )[0-9.]+", rf"\g<1>{version}")
        pins["tools"][name] = {"sha256": new, "version": version}

    syft = github_release("anchore/syft", f"v{args.syft}", token)
    for platform in SYFT_PLATFORMS:
        extension = "zip" if platform.startswith("windows") else "tar.gz"
        pins["syft"]["sha256"][platform] = digest(asset_url(syft, f"syft_{args.syft}_{platform}.{extension}"))
    replace(r"Syft [0-9]+(?:\.[0-9]+)+", f"Syft {args.syft}")
    pins["syft"]["version"] = args.syft

    db = github_release("databricks/cli", f"v{args.databricks}", token)
    db_hash = digest(asset_url(db, f"databricks_cli_{args.databricks}_linux_amd64.tar.gz"))
    replace(
        r"Databricks CLI \| [0-9.]+, Linux amd64 \| `[0-9a-f]{64}`",
        f"Databricks CLI | {args.databricks}, Linux amd64 | `{db_hash}`",
    )
    pins["databricks_cli"] = {"sha256": db_hash, "version": args.databricks}


VERSION_ARGUMENTS = ("rust", "rust_date", "llvm_cov", "audit", "deny", "syft", "databricks")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--emit-github-output", type=Path, help="validate the pins and write workflow step outputs")
    parser.add_argument("--rust")
    parser.add_argument("--rust-date")
    parser.add_argument("--llvm-cov")
    parser.add_argument("--audit")
    parser.add_argument("--deny")
    parser.add_argument("--syft")
    parser.add_argument("--databricks")
    parser.add_argument("--github-token", default=os.environ.get("GITHUB_TOKEN"))
    args = parser.parse_args()
    if args.emit_github_output:
        emit_github_output(args.emit_github_output)
        return
    missing = [name for name in VERSION_ARGUMENTS if not getattr(args, name)]
    if missing:
        parser.error("missing required arguments: " + ", ".join("--" + name.replace("_", "-") for name in missing))
    pins = load_pins()
    update_rust(pins, args.rust, args.rust_date)
    update_github_tools(pins, args)
    save_pins(pins)


if __name__ == "__main__":
    main()
