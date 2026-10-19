#!/usr/bin/env python3
"""Measure explicit readiness binaries without building or modifying sources.

--profile LABEL=MODE:BINARY --library LABEL=LIBRARY (repeat each for every profile).
Requires psutil and Mosquitto. Modes: poll, active (original), reactor, blocking.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import socket
import statistics
import subprocess
import tempfile
import time

import psutil
from execution_matrix import broker_start, distribution, snapshot, stop


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run_case(args, label, mode, binary, library, count, protocol, repetition):
    with tempfile.TemporaryDirectory(prefix="rumqttc-readiness-") as directory:
        directory = Path(directory)
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        config = directory / "mosquitto.conf"
        config.write_text(f"listener {port} 127.0.0.1\nallow_anonymous true\npersistence false\n")
        broker = child = None
        with (directory / "broker.log").open("w+") as broker_log, (directory / "consumer.log").open("w+") as log:
            try:
                broker = broker_start(args.mosquitto, config, port, broker_log)
                environment = os.environ.copy()
                environment["RUMQTTC_TEST_PORT"] = str(port)
                environment["PATH"] = str(library.parent) + os.pathsep + environment.get("PATH", "")
                command = [str(binary), mode, str(count), str(args.rounds), str(protocol)]
                if args.capacity is not None:
                    command.extend(["--capacity", str(args.capacity)])
                if args.host_work_us is not None:
                    command.extend(["--host-work-us", str(args.host_work_us)])
                child = subprocess.Popen(command,
                                         stdout=subprocess.PIPE, stderr=log, text=True, env=environment)
                process = psutil.Process(child.pid)
                result = {"profile": label, "mode": mode, "clients": count, "protocol": protocol,
                          "repetition": repetition, "binary_sha256": sha256(binary),
                          "library_sha256": sha256(library)}
                idle = None
                for line in child.stdout:
                    if line.strip() == "IDLE":
                        idle = snapshot(process)
                        idle["os_resources"] = process.num_handles() if os.name == "nt" else process.num_fds()
                        result["idle_start"] = idle
                    elif line.strip() == "PUBLISH":
                        sample = snapshot(process)
                        result["idle_end"] = sample
                        result["idle_cpu_seconds"] = sample["cpu_seconds"] - idle["cpu_seconds"]
                    elif line.startswith("{"):
                        result.update(json.loads(line))
                if child.wait(timeout=10) != 0:
                    log.seek(0)
                    raise RuntimeError(log.read())
                if "operations" not in result or result["operations"] != count * args.rounds:
                    raise RuntimeError("consumer did not confirm all observations")
                result["throughput"] = result["operations"] * 1e9 / result["elapsed_ns"]
                result["latency"] = distribution(result["latency_ns"])
                if "publish_cpu_ns" in result:
                    result["cpu_ns_per_operation"] = result["publish_cpu_ns"] / result["operations"]
                return result
            finally:
                if child is not None:
                    stop(child)
                if broker is not None:
                    stop(broker)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", action="append", required=True, help="LABEL=MODE:BINARY")
    parser.add_argument("--library", action="append", required=True, help="LABEL=LIBRARY")
    parser.add_argument("--mosquitto", default="mosquitto")
    parser.add_argument("--clients", default="1,32")
    parser.add_argument("--protocols", default="1,2")
    parser.add_argument("--rounds", type=int, default=256)
    parser.add_argument("--repetitions", type=int, default=5)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cpus", help="optional CPU affinity inherited by brokers and consumers")
    parser.add_argument("--capacity", type=int, help="registration capacity; omit for the original workload")
    parser.add_argument("--host-work-us", type=int,
                        help="unrelated host computation per round; omit for original workload")
    args = parser.parse_args()
    if args.cpus:
        psutil.Process().cpu_affinity([int(cpu) for cpu in args.cpus.split(",")])
    libraries = {label: Path(path).resolve() for label, path in (entry.split("=", 1) for entry in args.library)}
    profiles = []
    for entry in args.profile:
        label, value = entry.split("=", 1)
        mode, binary = value.split(":", 1)
        if mode not in {"poll", "active", "reactor", "blocking"}:
            parser.error("profile mode must be poll, active, reactor, or blocking")
        profiles.append((label, mode, Path(binary).resolve(), libraries[label]))
    report = {"platform": platform.platform(), "machine": platform.machine(),
              "created_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "rounds": args.rounds, "repetitions": args.repetitions, "runs": [],
              "cpu_affinity": psutil.Process().cpu_affinity() if hasattr(psutil.Process(), "cpu_affinity") else None,
              "capacity": args.capacity, "host_work_us": args.host_work_us}
    root = Path(__file__).resolve().parents[3]
    report["rustc"] = subprocess.check_output(["rustc", "--version"], text=True).strip()
    report["broker"] = subprocess.check_output([args.mosquitto, "-h"], text=True).splitlines()[0]
    report["head"] = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    report["tracked_diff_sha256"] = hashlib.sha256(subprocess.check_output(["git", "diff", "--binary"], cwd=root)).hexdigest()
    report["source_hashes"] = {str(path.relative_to(root)): sha256(path) for path in [
        root / "native-wrappers/wrapper-core/src/readiness.rs",
        root / "native-wrappers/wrapper-core/src/readiness/platform.rs",
        root / "native-wrappers/c/src/readiness.rs", Path(__file__).resolve(),
        Path(__file__).with_name("readiness.c").resolve(),
    ]}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    for count in map(int, args.clients.split(",")):
        for protocol in map(int, args.protocols.split(",")):
            for repetition in range(args.repetitions):
                offset = repetition % len(profiles)
                order = profiles[offset:] + profiles[:offset]
                if repetition % 2:
                    order = list(reversed(order))
                for label, mode, binary, library in order:
                    result = run_case(args, label, mode, binary, library, count, protocol, repetition)
                    report["runs"].append(result)
                    args.output.write_text(json.dumps(report, separators=(",", ":")) + "\n")
                    print(f"{label} clients={count} v{protocol} run={repetition}: "
                          f"{result['throughput']:.0f}/s p99={result['latency']['p99_ns']/1000:.1f}us "
                          f"idle_waits={result['idle_waits']}", flush=True)
    report["summary"] = []
    for count in map(int, args.clients.split(",")):
        for protocol in map(int, args.protocols.split(",")):
            for label, *_ in profiles:
                runs = [r for r in report["runs"] if (r["profile"], r["clients"], r["protocol"]) == (label, count, protocol)]
                summary = {"profile": label, "clients": count, "protocol": protocol,
                    "throughput_median": statistics.median(r["throughput"] for r in runs),
                    "throughput_min": min(r["throughput"] for r in runs),
                    "throughput_max": max(r["throughput"] for r in runs),
                    "p99_ns_median": statistics.median(r["latency"]["p99_ns"] for r in runs),
                    "p99_ns_min": min(r["latency"]["p99_ns"] for r in runs),
                    "p99_ns_max": max(r["latency"]["p99_ns"] for r in runs),
                    "idle_cpu_seconds_median": statistics.median(r["idle_cpu_seconds"] for r in runs),
                    "idle_waits_median": statistics.median(r["idle_waits"] for r in runs)}
                if all("cpu_ns_per_operation" in r for r in runs):
                    summary["cpu_ns_per_operation_median"] = statistics.median(r["cpu_ns_per_operation"] for r in runs)
                    summary["publish_cpu_ns_median"] = statistics.median(r["publish_cpu_ns"] for r in runs)
                    summary["idle_cpu_ns_median"] = statistics.median(r["idle_cpu_ns"] for r in runs)
                report["summary"].append(summary)
    args.output.write_text(json.dumps(report, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    main()
