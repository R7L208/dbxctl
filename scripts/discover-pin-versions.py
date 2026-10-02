#!/usr/bin/env python3
"""Discover current stable versions used by the supply-chain pin updater."""

import argparse
import json
import os
import re
import urllib.parse
from pathlib import Path

import importlib.util


UPDATER_PATH = Path(__file__).with_name("update-pins.py")
SPEC = importlib.util.spec_from_file_location("update_pins", UPDATER_PATH)
update_pins = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(update_pins)


def latest_release_tag(repo: str, token: str | None) -> str:
    url = f"https://api.github.com/repos/{repo}/releases/latest"
    effective_token = None if repo == "databricks/cli" else token
    return json.loads(update_pins.fetch(url, effective_token))["tag_name"]


def latest_audit_version(token: str | None) -> str:
    url = "https://api.github.com/repos/rustsec/rustsec/releases?per_page=100"
    releases = json.loads(update_pins.fetch(url, token))
    for release in releases:
        match = re.fullmatch(r"cargo-audit/v([0-9]+(?:\.[0-9]+)+)", release["tag_name"])
        if match:
            return match.group(1)
    raise RuntimeError("no cargo-audit release found")


def stable_rust() -> tuple[str, str]:
    manifest = update_pins.fetch("https://static.rust-lang.org/dist/channel-rust-stable.toml").decode()
    date = re.search(r'^date = "([0-9-]+)"$', manifest, re.MULTILINE)
    rust_section = re.search(r'^\[pkg\.rust\]\n(.*?)(?=^\[)', manifest, re.MULTILINE | re.DOTALL)
    version = re.search(r'^version = "([0-9]+(?:\.[0-9]+)+)', rust_section.group(1), re.MULTILINE) if rust_section else None
    if not date or not version:
        raise RuntimeError("stable Rust manifest is missing date or version")
    return version.group(1), date.group(1)


def discover(token: str | None) -> dict[str, str]:
    rust, rust_date = stable_rust()
    return {
        "rust": rust,
        "rust_date": rust_date,
        "llvm_cov": latest_release_tag("taiki-e/cargo-llvm-cov", token).removeprefix("v"),
        "audit": latest_audit_version(token),
        "deny": latest_release_tag("EmbarkStudios/cargo-deny", token).removeprefix("v"),
        "syft": latest_release_tag("anchore/syft", token).removeprefix("v"),
        "databricks": latest_release_tag("databricks/cli", token).removeprefix("v"),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--github-output", type=Path)
    args = parser.parse_args()
    versions = discover(os.environ.get("GITHUB_TOKEN"))
    if args.github_output:
        with args.github_output.open("a") as output:
            for name, version in versions.items():
                output.write(f"{name}={version}\n")
    else:
        print(json.dumps(versions, sort_keys=True))


if __name__ == "__main__":
    main()
