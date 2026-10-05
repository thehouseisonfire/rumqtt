#!/usr/bin/env python3
"""Interleave release wrapper shutdown measurements, optionally against a Git baseline."""
from __future__ import annotations

import argparse
import io
import itertools
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tempfile


def build(workspace: Path, output: Path, enabled: bool, target: Path) -> None:
    command = ["cargo", "bench", "--manifest-path", str(workspace / "Cargo.toml"),
               "-p", "rumqttc-wrapper-core-next", "--bench", "ordered_shutdown", "--no-run",
               "--no-default-features", "--locked", "--message-format=json"]
    if enabled:
        command += ["--features", "ordered-shutdown"]
    result = subprocess.run(command, env={**os.environ, "CARGO_TARGET_DIR": str(target)},
                            check=True, text=True, stdout=subprocess.PIPE)
    artifacts = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
    executable = next(item["executable"] for item in artifacts
                      if item.get("reason") == "compiler-artifact" and item.get("executable")
                      and item["target"]["name"] == "ordered_shutdown")
    shutil.copy2(executable, output)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", help="Git revision to compare with a feature-disabled build")
    parser.add_argument("--messages", type=int, default=2000)
    parser.add_argument("--repetitions", type=int, default=5)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    workspace = Path(__file__).resolve().parents[2]
    repository = workspace.parent
    target = workspace / "target"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="rumqtt-ordered-bench-") as temporary:
        temporary = Path(temporary)
        profiles = {"disabled": temporary / "disabled", "enabled-unused": temporary / "enabled"}
        if args.baseline:
            archive = subprocess.check_output(["git", "archive", args.baseline], cwd=repository)
            baseline = temporary / "baseline"
            baseline.mkdir()
            with tarfile.open(fileobj=io.BytesIO(archive)) as source:
                source.extractall(baseline, filter="data")
            baseline_workspace = baseline / "native-wrappers"
            shutil.copytree(workspace / "wrapper-core" / "benches",
                            baseline_workspace / "wrapper-core" / "benches", dirs_exist_ok=True)
            manifest = baseline_workspace / "wrapper-core" / "Cargo.toml"
            manifest.write_text(manifest.read_text() + '\n[[bench]]\nname = "ordered_shutdown"\nharness = false\n')
            # The benchmark checks this cfg even when compiling a revision without the API.
            text = manifest.read_text()
            text = text.replace("[features]\n", '[features]\nordered-shutdown = []\n', 1)
            manifest.write_text(text)
            profiles = {"baseline-disabled": temporary / "baseline-bin", **profiles}
            build(baseline_workspace, profiles["baseline-disabled"], False, target)
        build(workspace, profiles["disabled"], False, target)
        build(workspace, profiles["enabled-unused"], True, target)
        profiles["enabled-ordered"] = profiles["enabled-unused"]
        configurations = list(itertools.product((False, True), (0, 1, 2), (1, 8), (1, 64), (1, 64)))
        metadata = {"platform": platform.platform(), "cpu": platform.processor(),
                    "revision": subprocess.check_output(
                        ["git", "rev-parse", "HEAD"], cwd=repository, text=True).strip(),
                    "baseline": args.baseline,
                    "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
                    "messages": args.messages, "repetitions": args.repetitions,
                    "configurations": len(configurations), "payload_bytes": 64,
                    "allocation_repetitions": 1,
                    "broker": "immediately acknowledging loopback TCP peer; TCP_NODELAY"}
        with args.output.open("w") as output:
            output.write(json.dumps({"metadata": metadata}) + "\n")
            for scenario, (mqtt5, qos, producers, capacity, inflight) in enumerate(configurations):
                for repetition in range(args.repetitions):
                    names = list(profiles)
                    # Rotate profile order to avoid consistently favoring one build.
                    offset = (scenario + repetition) % len(names)
                    names = names[offset:] + names[:offset]
                    for name in names:
                        command = [str(profiles[name]), "--messages", str(args.messages), "--qos", str(qos),
                                   "--producers", str(producers), "--capacity", str(capacity),
                                   "--inflight", str(inflight)]
                        if mqtt5:
                            command += ["--v5"]
                        if name == "enabled-ordered":
                            command += ["--ordered"]
                        row = json.loads(subprocess.check_output(command, text=True, timeout=90))
                        row.update(profile=name, repetition=repetition)
                        output.write(json.dumps(row) + "\n")
                        output.flush()
                        if repetition == 0:
                            allocation_row = json.loads(subprocess.check_output(
                                command + ["--allocations"], text=True, timeout=90))
                            allocation_row.update(profile=name, repetition=0)
                            output.write(json.dumps(allocation_row) + "\n")
                            output.flush()
                print(f"measured {scenario + 1}/{len(configurations)} configurations", flush=True)


if __name__ == "__main__":
    main()
