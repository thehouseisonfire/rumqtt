#!/usr/bin/env python3
"""Validate and compile the production recipes under docs/recipes."""

from __future__ import annotations

import argparse
import json
import re
import shlex
import shutil
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.request
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RECIPES = ROOT / "docs" / "recipes"
BUILD_ROOT = ROOT / "target" / "recipe-check"
FENCE_RE = re.compile(r"^```(?P<language>[^\n]*)\n(?P<body>.*?)^```\s*$", re.MULTILINE | re.DOTALL)
EXAMPLE_RE = re.compile(
    r"`(?P<path>(?:rumqttc-v[45]|session-store-file/v[45])/examples/"
    r"(?P<file>[A-Za-z0-9_-]+\.rs))`"
)
EXTERNAL_LINK_RE = re.compile(r"\]\((?P<url>https://[^)\s]+)\)")


@dataclass(frozen=True)
class Fence:
    path: Path
    line: int
    language: str
    body: str


def fail(message: str) -> None:
    raise ValueError(message)


def recipe_files() -> list[Path]:
    return sorted(RECIPES.rglob("*.md"))


def fences() -> list[Fence]:
    found: list[Fence] = []
    for path in recipe_files():
        content = path.read_text(encoding="utf-8")
        for match in FENCE_RE.finditer(content):
            found.append(
                Fence(
                    path=path,
                    line=content.count("\n", 0, match.start()) + 1,
                    language=match.group("language").strip(),
                    body=match.group("body"),
                )
            )
    return found


def cargo_metadata() -> dict:
    packages = []
    for extra_arguments in ([], ["--manifest-path", "session-store-file/Cargo.toml"]):
        result = subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1", *extra_arguments],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        )
        packages.extend(json.loads(result.stdout)["packages"])
    return {"packages": packages}


def validate_toml(all_fences: list[Fence]) -> None:
    for fence in all_fences:
        if fence.language != "toml":
            continue
        try:
            tomllib.loads(fence.body)
        except tomllib.TOMLDecodeError as error:
            fail(f"{fence.path.relative_to(ROOT)}:{fence.line}: invalid TOML: {error}")


def validate_examples(metadata: dict) -> None:
    registered: dict[str, set[str]] = {}
    for package in metadata["packages"]:
        examples = {target["name"] for target in package["targets"] if "example" in target["kind"]}
        registered[package["name"]] = examples

    package_for_directory = {
        "rumqttc-v4": "rumqttc-v4-next",
        "rumqttc-v5": "rumqttc-v5-next",
        "session-store-file/adapter": "rumqttc-session-store-file-next",
    }
    for path in recipe_files():
        content = path.read_text(encoding="utf-8")
        for match in EXAMPLE_RE.finditer(content):
            relative = Path(match.group("path"))
            line = content.count("\n", 0, match.start()) + 1
            if not (ROOT / relative).is_file():
                fail(f"{path.relative_to(ROOT)}:{line}: missing example {relative}")
            directory = relative.parts[0]
            if directory == "session-store-file":
                directory = "/".join(relative.parts[:2])
            package = package_for_directory[directory]
            example = relative.stem
            if example not in registered.get(package, set()):
                fail(
                    f"{path.relative_to(ROOT)}:{line}: {relative} is not a registered "
                    f"example of {package}"
                )


def validate_external_links() -> None:
    links: dict[str, tuple[Path, int]] = {}
    for path in recipe_files():
        content = path.read_text(encoding="utf-8")
        for match in EXTERNAL_LINK_RE.finditer(content):
            links.setdefault(match.group("url"), (path, content.count("\n", 0, match.start()) + 1))

    for url, (path, line) in links.items():
        request = urllib.request.Request(
            url,
            headers={"Range": "bytes=0-0", "User-Agent": "rumqttc-recipe-link-check/1.0"},
        )
        last_error: Exception | None = None
        for attempt in range(3):
            try:
                with urllib.request.urlopen(request, timeout=15) as response:
                    response.read(1)
                break
            except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError) as error:
                last_error = error
                if attempt < 2:
                    time.sleep(attempt + 1)
        else:
            fail(f"{path.relative_to(ROOT)}:{line}: external link failed: {url}: {last_error}")


def option_value(arguments: list[str], short: str, long: str) -> str | None:
    for index, argument in enumerate(arguments):
        if argument in (short, long):
            if index + 1 >= len(arguments):
                fail(f"missing value after {argument}")
            return arguments[index + 1]
        if argument.startswith(f"{long}="):
            return argument.split("=", 1)[1]
    return None


def shell_commands(fence: Fence) -> Iterator[tuple[int, str]]:
    command = ""
    start_line = 1
    for offset, raw_line in enumerate(fence.body.splitlines(), start=1):
        stripped = raw_line.strip()
        if not command:
            start_line = offset
        if stripped.endswith("\\"):
            command += stripped[:-1].rstrip() + " "
            continue
        command += stripped
        yield start_line, command
        command = ""
    if command:
        fail(f"{fence.path.relative_to(ROOT)}:{fence.line + start_line}: unterminated shell continuation")


def validate_commands(all_fences: list[Fence], metadata: dict) -> None:
    packages = {package["name"]: package for package in metadata["packages"]}
    for fence in all_fences:
        if fence.language not in ("bash", "sh", "shell"):
            continue
        for offset, line in shell_commands(fence):
            if not line or line.startswith("#"):
                continue
            arguments = shlex.split(line)
            if arguments[:2] == ["docker", "compose"]:
                compose_file = option_value(arguments, "-f", "--file")
                if compose_file is None or not (ROOT / compose_file).is_file():
                    fail(
                        f"{fence.path.relative_to(ROOT)}:{fence.line + offset}: Docker Compose "
                        "command must reference an existing repository file"
                    )
                continue
            if not line.startswith("cargo "):
                continue
            if len(arguments) < 2 or arguments[1] not in ("check", "run", "test"):
                fail(f"{fence.path.relative_to(ROOT)}:{fence.line + offset}: unsupported Cargo command")
            package_name = option_value(arguments, "-p", "--package")
            if package_name is None or package_name not in packages:
                fail(
                    f"{fence.path.relative_to(ROOT)}:{fence.line + offset}: command must name a "
                    "workspace package with -p/--package"
                )
            package = packages[package_name]
            feature_value = option_value(arguments, "-F", "--features")
            if feature_value:
                requested = set(re.split(r"[ ,]+", feature_value))
                unknown = requested.difference(package["features"])
                if unknown:
                    fail(
                        f"{fence.path.relative_to(ROOT)}:{fence.line + offset}: unknown features for "
                        f"{package_name}: {', '.join(sorted(unknown))}"
                    )
            example = option_value(arguments, "", "--example")
            if example:
                examples = {target["name"] for target in package["targets"] if "example" in target["kind"]}
                if example not in examples:
                    fail(
                        f"{fence.path.relative_to(ROOT)}:{fence.line + offset}: unknown example "
                        f"{example} for {package_name}"
                    )


def compile_documented_commands(all_fences: list[Fence]) -> None:
    for fence in all_fences:
        if fence.language not in ("bash", "sh", "shell"):
            continue
        for _offset, line in shell_commands(fence):
            if not line.startswith("cargo "):
                continue
            arguments = shlex.split(line)
            if arguments[1] == "run":
                arguments[1] = "check"
            subprocess.run(arguments, cwd=ROOT, check=True)


def rust_source(fence: Fence) -> str:
    lines = []
    for line in fence.body.splitlines():
        lines.append(line[2:] if line.startswith("# ") else line)
    body = "\n".join(lines)
    return "\n".join(
        (
            "#![allow(dead_code, unused_imports, unused_mut, unused_variables)]",
            "fn main() {",
            body,
            "}",
            "",
        )
    )


def write_compile_crate(all_fences: list[Fence], package: str, directory: str) -> Path:
    crate_root = BUILD_ROOT / directory
    if crate_root.exists():
        shutil.rmtree(crate_root)
    source_root = crate_root / "src" / "bin"
    source_root.mkdir(parents=True)
    protocol = "v4" if directory.endswith("v4") else "v5"
    rust_fences = []
    for fence in all_fences:
        language_parts = {part.strip() for part in fence.language.split(",")}
        if "rust" not in language_parts:
            continue
        if language_parts.intersection({"v4", "v5"}) not in (set(), {protocol}):
            continue
        rust_fences.append(fence)
    for index, fence in enumerate(rust_fences):
        slug = re.sub(r"[^a-z0-9]+", "_", fence.path.stem.lower()).strip("_")
        (source_root / f"recipe_{index:02}_{slug}.rs").write_text(rust_source(fence), encoding="utf-8")

    # include_bytes! examples intentionally name deployment-provided files. Empty
    # placeholders are sufficient for compile validation without committing keys.
    for name in ("AmazonRootCA1.pem", "device-certificate.pem.crt", "private.pem.key"):
        (source_root / name).write_bytes(b"")

    manifest = f'''[package]
name = "recipe-check-{directory}"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
tokio = {{ version = "1", features = ["macros", "rt"] }}
tracing-subscriber = {{ version = "0.3", features = ["env-filter", "fmt"] }}

[dependencies.rumqttc]
package = "{package}"
path = "../../../{directory}"
features = ["http-proxy", "socks-proxy", "tracing", "websocket"]

[workspace]
'''
    manifest_path = crate_root / "Cargo.toml"
    manifest_path.write_text(manifest, encoding="utf-8")
    return manifest_path


def compile_rust(all_fences: list[Fence]) -> None:
    for package, directory in (("rumqttc-v4-next", "rumqttc-v4"), ("rumqttc-v5-next", "rumqttc-v5")):
        manifest = write_compile_crate(all_fences, package, directory)
        subprocess.run(
            ["cargo", "check", "--quiet", "--manifest-path", str(manifest), "--bins"],
            cwd=ROOT,
            check=True,
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--no-compile", action="store_true", help="skip compiling Rust fences")
    parser.add_argument("--no-external", action="store_true", help="skip checking external links")
    args = parser.parse_args()

    try:
        all_fences = fences()
        metadata = cargo_metadata()
        validate_toml(all_fences)
        validate_examples(metadata)
        validate_commands(all_fences, metadata)
        if not args.no_external:
            validate_external_links()
        if not args.no_compile:
            compile_rust(all_fences)
            compile_documented_commands(all_fences)
    except (ValueError, subprocess.CalledProcessError) as error:
        print(f"recipe validation failed: {error}", file=sys.stderr)
        return 1

    print("Production recipe snippets, examples, and commands are valid.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
