#!/usr/bin/env python3
"""Profile representative private TLS workloads separately from acceptance timing."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import platform
import resource
import subprocess
from datetime import datetime, timezone


def completed(command: list[str], log: pathlib.Path, environment: dict[str, str]) -> dict[str, float | int]:
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    with log.open("w") as stream:
        subprocess.run(command, env=environment, stdout=stream, stderr=subprocess.STDOUT, check=True)
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    if "test result: ok. 1 passed; 0 failed;" not in log.read_text():
        raise RuntimeError(f"profiling workload did not complete successfully: {log}")
    return {name: getattr(after, name) - getattr(before, name)
            for name in ("ru_utime", "ru_stime", "ru_nvcsw", "ru_nivcsw")}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--data-output", type=pathlib.Path, required=True,
                        help="separate directory for large perf.data files; normally under target/")
    parser.add_argument("--baseline", type=pathlib.Path, required=True,
                        help="environment.json from the corrected, uninstrumented matrix")
    parser.add_argument("--messages", type=int, default=100_000)
    parser.add_argument("--mqtt", type=int, choices=(4, 5), default=5)
    parser.add_argument("--execution", choices=("dedicated", "shared"), default="dedicated")
    args = parser.parse_args()
    if not 0 < args.messages <= 2**32 - 1:
        parser.error("messages must be 1..2^32-1")
    baseline = json.loads(args.baseline.read_text())
    if len(baseline["runs"]) != 4:
        parser.error("baseline must contain both timing modes and separate allocation runs")
    for directory in (args.output, args.data_output):
        directory.mkdir(parents=True, exist_ok=True)
        if any(directory.iterdir()):
            parser.error(f"output directory must be empty: {directory}")
    root = pathlib.Path(__file__).resolve().parents[3]
    environment = os.environ.copy()
    for name in tuple(environment):
        if name.startswith("RUMQTTC_TLS_PROOF_"):
            del environment[name]
    # Retain the normal release optimizer/LTO settings, adding symbols for sampling.
    environment["CARGO_PROFILE_RELEASE_DEBUG"] = "1"
    environment["CARGO_PROFILE_RELEASE_STRIP"] = "none"
    build = ["cargo", "test", "--locked", "--release", "--manifest-path",
             str(root / "native-wrappers/Cargo.toml"), "-p", "rumqttc-wrapper-core-next",
             "--features", "tls-connector-proof", "--lib", "--no-run", "--message-format=json"]
    result = subprocess.run(build, env=environment, stdout=subprocess.PIPE, text=True, check=True)
    artifacts = [item["executable"] for line in result.stdout.splitlines()
                 if (item := json.loads(line)).get("reason") == "compiler-artifact" and item.get("executable")]
    if len(artifacts) != 1:
        raise RuntimeError("Cargo did not report exactly one test executable")
    sources = set(baseline["source_sha256"]) | {str(pathlib.Path(__file__).resolve().relative_to(root))}
    metadata = {
        "created_utc": datetime.now(timezone.utc).isoformat(),
        "platform": platform.platform(),
        "perf": subprocess.check_output(["perf", "--version"], text=True).strip(),
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "openssl": subprocess.check_output(["openssl", "version"], text=True).strip(),
        "git_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
        "source_sha256": {name: hashlib.sha256((root / name).read_bytes()).hexdigest() for name in sorted(sources)},
        "baseline": baseline,
        "build_command": build,
        "build_overrides": {name: environment[name] for name in
                            ("CARGO_PROFILE_RELEASE_DEBUG", "CARGO_PROFILE_RELEASE_STRIP")},
        "messages": args.messages,
        "mqtt": args.mqtt,
        "execution": args.execution,
        "runs": [],
    }
    command = [artifacts[0], "--exact", "backend::tls_connector_proof::profile_external_tls_workload",
               "--ignored", "--nocapture"]
    (args.output / "environment.json").write_text(json.dumps(metadata, indent=2) + "\n")
    for nested, payload in ((False, 1024), (True, 1024), (True, 16384)):
        for mode in ("native", "threaded", "inline"):
            label = f"{mode}-{'nested' if nested else 'single'}-{payload}"
            print(f"Profiling {label}", flush=True)
            env = environment | {
                "RUMQTTC_TLS_PROOF_HOST_MODE": "threaded" if mode == "threaded" else "inline",
                "RUMQTTC_TLS_PROOF_MESSAGES": str(args.messages),
                "RUMQTTC_TLS_PROOF_PROFILE_MQTT": str(args.mqtt),
                "RUMQTTC_TLS_PROOF_PROFILE_EXECUTION": args.execution,
                "RUMQTTC_TLS_PROOF_PROFILE_NESTED": str(nested).lower(),
                "RUMQTTC_TLS_PROOF_PROFILE_PAYLOAD": str(payload),
                "RUMQTTC_TLS_PROOF_PROFILE_EXTERNAL": str(mode != "native").lower(),
            }
            stats = ["perf", "stat", "-x", ";", "--no-big-num", "-e",
                     "task-clock:u", "-o", str(args.output / f"{label}-stat.txt"),
                     "--", *command]
            usage = completed(stats, args.output / f"{label}-stat-workload.txt", env)
            data = args.data_output / f"{label}.data"
            record = ["perf", "record", "--quiet", "-e", "cpu-clock:u", "-F", "499",
                      "--user-callchains", "--call-graph", "dwarf,8192", "-o", str(data), "--", *command]
            completed(record, args.output / f"{label}-record-workload.txt", env)
            report = ["perf", "report", "--stdio", "--no-children", "--percent-limit", "0",
                      "--show-nr-samples", "--sort", "symbol,dso", "--field-separator", ";", "-i", str(data)]
            with (args.output / f"{label}-report.txt").open("w") as stream:
                subprocess.run(report, stdout=stream, stderr=subprocess.STDOUT, check=True)
            callers = ["perf", "report", "--stdio", "--children", "--call-graph", "none",
                       "--percent-limit", "1", "--sort", "symbol", "-i", str(data)]
            with (args.output / f"{label}-callers.txt").open("w") as stream:
                subprocess.run(callers, stdout=stream, stderr=subprocess.STDOUT, check=True)
            metadata["runs"].append({
                "label": label, "environment": {key: value for key, value in env.items()
                                                 if key.startswith("RUMQTTC_TLS_PROOF_")},
                "stat_command": stats, "record_command": record, "report_command": report,
                "callers_command": callers,
                "stat_process_usage_including_perf": usage,
            })
            (args.output / "environment.json").write_text(json.dumps(metadata, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
