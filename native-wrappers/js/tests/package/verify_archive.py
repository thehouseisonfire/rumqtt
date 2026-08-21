#!/usr/bin/env python3
"""Verify that a packed npm archive contains only its declared release payload."""

from __future__ import annotations

import argparse
import json
import pathlib
import tarfile


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("archive", type=pathlib.Path)
    parser.add_argument("--binary")
    args = parser.parse_args()

    with tarfile.open(args.archive, "r:gz") as package:
        files = sorted(member.name for member in package.getmembers() if member.isfile())
        manifest_file = package.extractfile("package/package.json")
        if manifest_file is None:
            raise SystemExit("archive package.json is not a regular file")
        manifest = json.load(manifest_file)

    expected = ["package/README.md", "package/package.json"]
    if args.binary:
        expected.append(f"package/{args.binary}")
        if manifest.get("main") != args.binary:
            raise SystemExit(f"platform main does not select {args.binary}")
        if not manifest.get("os") or not manifest.get("cpu"):
            raise SystemExit("platform package is missing os/cpu constraints")
    else:
        expected.extend(
            [
                "package/index.cjs",
                "package/index.d.ts",
                "package/index.js",
                "package/loader.cjs",
            ]
        )
        if manifest.get("name") != "@rumqtt-next/rumqttc":
            raise SystemExit("unexpected main package name")
        if set(manifest.get("optionalDependencies", {})) != {
            "@rumqtt-next/rumqttc-linux-x64-gnu",
            "@rumqtt-next/rumqttc-linux-x64-musl",
            "@rumqtt-next/rumqttc-linux-arm64-gnu",
            "@rumqtt-next/rumqttc-darwin-x64",
            "@rumqtt-next/rumqttc-darwin-arm64",
            "@rumqtt-next/rumqttc-win32-x64-msvc",
        }:
            raise SystemExit("main package platform dependency graph is incomplete")

    if files != sorted(expected):
        raise SystemExit(f"archive contents differ: expected {sorted(expected)!r}, got {files!r}")
    print(f"verified {args.archive}: {len(files)} files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
