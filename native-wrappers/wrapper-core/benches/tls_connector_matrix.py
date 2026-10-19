#!/usr/bin/env python3
"""Build once and retain reproducible private TODO49 timing/allocation evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import platform
import subprocess
import sys
from datetime import datetime, timezone


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--messages", type=int, default=10_000)
    parser.add_argument("--repetitions", type=int, default=7)
    parser.add_argument("--allocations", action="store_true")
    args = parser.parse_args()
    if not 0 < args.messages <= 2**32 - 1 or args.repetitions < 1:
        parser.error("messages must be 1..2^32-1 and repetitions must be positive")
    root = pathlib.Path(__file__).resolve().parents[3]
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        parser.error("output directory must be empty to preserve previous evidence")
    build = ["cargo", "test", "--locked", "--release", "--manifest-path",
             str(root / "native-wrappers/Cargo.toml"), "-p", "rumqttc-wrapper-core-next",
             "--features", "tls-connector-proof", "--lib", "--no-run", "--message-format=json"]
    result = subprocess.run(build, cwd=root, stdout=subprocess.PIPE, text=True, check=True)
    executable = None
    for line in result.stdout.splitlines():
        item = json.loads(line)
        if item.get("reason") == "compiler-artifact" and item.get("executable"):
            executable = item["executable"]
    if executable is None:
        raise SystemExit("Cargo did not report a test executable")
    sources = ["native-wrappers/wrapper-core/src/backend/tls_connector_proof.rs",
               "native-wrappers/wrapper-core/tests/fixtures/tls_connector.c",
               "native-wrappers/wrapper-core/build.rs", "native-wrappers/wrapper-core/Cargo.toml",
               "native-wrappers/Cargo.lock", str(pathlib.Path(__file__).resolve().relative_to(root)),
               "native-wrappers/wrapper-core/benches/summarize_tls_connector.py"]
    metadata = {
        "created_utc": datetime.now(timezone.utc).isoformat(),
        "platform": platform.platform(),
        "machine": platform.machine(),
        "logical_cpus": os.cpu_count(),
        "python": platform.python_version(),
        "libc": platform.libc_ver(),
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "openssl": subprocess.check_output(["openssl", "version"], text=True).strip(),
        "git_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
        "source_sha256": {name: hashlib.sha256((root / name).read_bytes()).hexdigest() for name in sources},
        "messages": args.messages,
        "repetitions": args.repetitions,
        "build_command": build,
        "runs": [],
    }
    (output / "environment.json").write_text(json.dumps(metadata, indent=2) + "\n")
    command = [executable, "--exact", "backend::tls_connector_proof::measure_external_tls_delivery_gate",
               "--ignored", "--nocapture"]
    passed = True
    for instrumented in ([False, True] if args.allocations else [False]):
        for mode in ("threaded", "inline"):
            label = mode + ("-allocations" if instrumented else "")
            environment = os.environ.copy()
            environment["RUMQTTC_TLS_PROOF_HOST_MODE"] = mode
            environment["RUMQTTC_TLS_PROOF_MESSAGES"] = str(args.messages)
            environment["RUMQTTC_TLS_PROOF_REPETITIONS"] = str(1 if instrumented else args.repetitions)
            environment.pop("RUMQTTC_TLS_PROOF_COUNT_ALLOCATIONS", None)
            if instrumented:
                environment["RUMQTTC_TLS_PROOF_COUNT_ALLOCATIONS"] = "1"
            print(f"Measuring {label}", flush=True)
            with (output / f"{label}.txt").open("w") as log:
                subprocess.run(command, cwd=root, env=environment, stdout=log, stderr=subprocess.STDOUT, check=True)
            summary = [sys.executable, str(pathlib.Path(__file__).with_name("summarize_tls_connector.py")),
                       str(output / f"{label}.txt"), "--output", str(output / f"{label}.csv"),
                       "--repetitions", str(1 if instrumented else args.repetitions)]
            if instrumented:
                summary.append("--allocation-samples")
            status = subprocess.run(summary, cwd=root, check=False).returncode
            if status not in (0, 1):
                raise SystemExit("invalid or incomplete measurement output")
            metadata["runs"].append({"mode": mode, "instrumented": instrumented,
                                     "gate_passed": None if instrumented else status == 0})
            (output / "environment.json").write_text(json.dumps(metadata, indent=2) + "\n")
            passed &= instrumented or status == 0
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
