#!/usr/bin/env python3
"""Resolve and authenticate the latest compatible rumqttc-c release."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tarfile
import urllib.request

REPOSITORY = "thehouseisonfire/rumqtt"
TAG = re.compile(r"^rumqttc-c-v(\d+)\.(\d+)\.(\d+)$")


def parse_version(version: str) -> tuple[int, int, int] | None:
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?", version)
    return tuple(map(int, match.groups())) if match else None


def select_baseline(version: str, tags: list[str]) -> str | None:
    current = parse_version(version)
    if current is None:
        return None
    major, minor, patch = current
    candidates: list[tuple[tuple[int, int, int], str]] = []
    for tag in tags:
        match = TAG.fullmatch(tag)
        if not match:
            continue
        candidate = tuple(map(int, match.groups()))
        same_line = candidate[:2] == current[:2] if major == 0 else candidate[0] == major
        if same_line and candidate < current:
            candidates.append((candidate, tag))
    if candidates:
        return max(candidates)[1]
    if (major == 0 and patch > 0) or (major > 0 and (minor > 0 or patch > 0)):
        raise RuntimeError(f"{version} promises compatibility but no prior release baseline exists")
    return None


def request_json(url: str):
    headers = {"Accept": "application/vnd.github+json", "User-Agent": "rumqttc-abi-check"}
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers)) as response:
        return json.load(response)


def download(url: str, destination: pathlib.Path) -> None:
    request = urllib.request.Request(url, headers={"User-Agent": "rumqttc-abi-check"})
    with urllib.request.urlopen(request) as response, destination.open("wb") as output:
        while chunk := response.read(1024 * 1024):
            output.write(chunk)


def safe_extract(archive: pathlib.Path, destination: pathlib.Path) -> None:
    root = destination.resolve()
    with tarfile.open(archive, "r:gz") as bundle:
        for member in bundle.getmembers():
            target = (destination / member.name).resolve()
            if root not in target.parents and target != root:
                raise RuntimeError(f"unsafe archive member: {member.name}")
            if member.issym():
                link_target = (target.parent / member.linkname).resolve()
                if root not in link_target.parents and link_target != root:
                    raise RuntimeError(f"unsafe archive symlink: {member.name}")
            if member.islnk():
                link_target = (destination / member.linkname).resolve()
                if root not in link_target.parents and link_target != root:
                    raise RuntimeError(f"unsafe archive hard link: {member.name}")
        bundle.extractall(destination, filter="data")


def verify_checksum(archive: pathlib.Path, checksum: pathlib.Path) -> None:
    expected = checksum.read_text(encoding="utf-8").split()[0].lower()
    actual = hashlib.sha256(archive.read_bytes()).hexdigest()
    if not re.fullmatch(r"[0-9a-f]{64}", expected) or expected != actual:
        raise RuntimeError(f"checksum mismatch for {archive.name}")


def remove_resolver_state(output: pathlib.Path) -> None:
    for path in (output / "no-baseline", output / "baseline.json", output / "extracted"):
        if path.is_symlink() or path.is_file():
            path.unlink()
        elif path.exists():
            shutil.rmtree(path)


def resolve(args: argparse.Namespace) -> int:
    output = pathlib.Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    remove_resolver_state(output)
    releases = request_json(f"https://api.github.com/repos/{REPOSITORY}/releases?per_page=100")
    tag = select_baseline(args.version, [release["tag_name"] for release in releases])
    if tag is None:
        (output / "no-baseline").write_text(f"no published baseline for {args.version}\n", encoding="utf-8")
        print(f"no published baseline for {args.version}")
        return 0

    release = next(item for item in releases if item["tag_name"] == tag)
    asset_name = f"rumqttc-c-{args.platform}.tar.gz"
    checksum_name = f"{asset_name}.sha256"
    assets = {asset["name"]: asset for asset in release["assets"]}
    if asset_name not in assets or checksum_name not in assets:
        raise RuntimeError(f"{tag} does not contain the paired {args.platform} archive and checksum")
    archive = output / asset_name
    checksum = output / checksum_name
    download(assets[asset_name]["browser_download_url"], archive)
    download(assets[checksum_name]["browser_download_url"], checksum)
    verify_checksum(archive, checksum)
    if not args.skip_attestation:
        subprocess.run(
            ["gh", "attestation", "verify", str(archive), "--repo", REPOSITORY],
            check=True,
        )
    extracted = output / "extracted"
    extracted.mkdir()
    safe_extract(archive, extracted)
    root_entries = [entry for entry in extracted.iterdir() if entry.is_dir()]
    if len(root_entries) != 1:
        raise RuntimeError("baseline archive must have exactly one top-level directory")
    root = root_entries[0]
    contract = root / "share" / "rumqttc" / "abi-contract.json"
    header = root / "include" / "rumqttc.h"
    current = parse_version(args.version)
    if current is None:
        raise RuntimeError(f"invalid package version: {args.version}")
    abi_line = f"0.{current[1]}" if current[0] == 0 else str(current[0])
    library_name = {
        "linux-x86_64": f"librumqttc.so.{abi_line}",
        "macos-arm64": f"librumqttc.{abi_line}.dylib",
        "windows-x86_64": f"rumqttc-{abi_line.replace('.', '_')}.dll",
    }.get(args.platform)
    if library_name is None:
        raise RuntimeError(f"unsupported baseline platform: {args.platform}")
    library = root / "lib" / library_name
    if not contract.is_file() or not header.is_file() or not library.is_file():
        raise RuntimeError("baseline archive lacks its paired header, library, or ABI contract")
    contract_data = json.loads(contract.read_text(encoding="utf-8"))
    if contract_data.get("package_version") != tag.removeprefix("rumqttc-c-v"):
        raise RuntimeError("baseline contract package version does not match its release tag")
    metadata = {
        "tag": tag,
        "root": str(root),
        "header": str(header),
        "library": str(library),
        "contract": str(contract),
    }
    (output / "baseline.json").write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
    print(tag)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--skip-attestation", action="store_true", help=argparse.SUPPRESS)
    return resolve(parser.parse_args())


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"baseline error: {error}", file=sys.stderr)
        raise SystemExit(1) from error
