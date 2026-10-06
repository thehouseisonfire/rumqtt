"""Interleaved, fresh-process C/Python execution measurements against Mosquitto.

Requires psutil. Builds/installation are explicit; this runner never edits sources.
Every case records resource snapshots and raw observation latencies in JSON.
"""

from __future__ import annotations

import argparse
import getpass
import hashlib
import importlib.util
import itertools
import json
import os
import platform
import queue
import shutil
import socket
import subprocess
import sys
import sysconfig
import tempfile
import threading
import time
from pathlib import Path

import psutil

WORKSPACE = Path(__file__).resolve().parents[2]


def snapshot(process: psutil.Process) -> dict:
    memory = process.memory_info()
    cpu = process.cpu_times()
    return {
        "rss_bytes": memory.rss,
        "virtual_bytes": memory.vms,
        "threads": process.num_threads(),
        "cpu_seconds": cpu.user + cpu.system,
    }


def broker_start(executable: str, config: Path, port: int, log) -> subprocess.Popen:
    broker = subprocess.Popen([executable, "-c", str(config)], stdout=log, stderr=log)
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if broker.poll() is not None:
            raise RuntimeError(f"Mosquitto exited with {broker.returncode}; see broker log")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                return broker
        except OSError:
            time.sleep(0.01)
    broker.kill()
    broker.wait()
    raise TimeoutError("Mosquitto startup timed out")


def stop(process: subprocess.Popen) -> None:
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def distribution(values: list[int]) -> dict:
    if not values:
        return {}
    values = sorted(values)
    return {
        "samples": len(values),
        **{f"p{percentile}_ns": values[(len(values) - 1) * percentile // 100] for percentile in [50, 95, 99]},
        "max_ns": values[-1],
    }


def run_case(
    args,
    consumer: str,
    mode: str,
    count: int,
    protocol: int,
    qos: int,
    scenario: str,
    transport: str,
    repetition: int,
    stage: list[Path] | None,
) -> dict:
    label = f"{consumer}-{mode}-{count}-v{protocol}-q{qos}-{scenario}-{transport}-{repetition}"
    environment = os.environ.copy()
    environment["RUMQTTC_EXECUTION_ROUNDS"] = str(args.rounds)
    environment["RUMQTTC_EXECUTION_TRANSPORT"] = transport
    child = broker = None
    result = {
        "case": label,
        "consumer": consumer,
        "mode": mode,
        "clients": count,
        "protocol": protocol,
        "qos": qos,
        "scenario": scenario,
        "transport": transport,
        "repetition": repetition,
        "status": "incomplete",
    }
    with tempfile.TemporaryDirectory(prefix="rumqtt-execution-") as directory:
        directory = Path(directory)
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        environment["RUMQTTC_TEST_PORT"] = str(port)
        config = directory / "mosquitto.conf"
        lines = [f"listener {port} 127.0.0.1", "allow_anonymous true", "persistence false"]
        if os.name != "nt":
            lines.append(f"user {getpass.getuser()}")
        if transport in ["ws", "wss"]:
            lines.append("protocol websockets")
        if transport in ["tls", "wss"]:
            fixture_path = WORKSPACE / "c/tests/native/broker_fixture.py"
            spec = importlib.util.spec_from_file_location("execution_tls_fixture", fixture_path)
            fixture = importlib.util.module_from_spec(spec)
            sys.modules[spec.name] = fixture
            sys.path.insert(0, str(fixture_path.parent))
            try:
                spec.loader.exec_module(fixture)
            finally:
                sys.path.pop(0)
            fixture.make_tls_fixture(str(directory))
            lines += [
                f"cafile {directory / 'ca.pem'}",
                f"certfile {directory / 'server.pem'}",
                f"keyfile {directory / 'server.key'}",
            ]
            environment["RUMQTTC_EXECUTION_CA"] = str(directory / "ca.pem")
        config.write_text("\n".join(lines) + "\n")
        with (
            (args.output / f"{label}.broker.log").open("w") as broker_log,
            (args.output / f"{label}.stderr.log").open("w") as child_log,
        ):
            try:
                broker = broker_start(args.mosquitto, config, port, broker_log)
                binary = args.shards_binary if mode == "shards" else args.binary
                command = [str(binary.resolve()), mode, str(count), str(protocol), str(qos), scenario]
                if consumer == "python":
                    environment["PYTHONPATH"] = str(stage[mode == "shards"])
                    command = [
                        sys.executable,
                        "-u",
                        str(Path(__file__).with_name("execution_python.py")),
                        mode,
                        str(count),
                        str(protocol),
                        str(qos),
                        scenario,
                    ]
                child = subprocess.Popen(
                    command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=child_log, text=True, env=environment
                )
                output = queue.Queue()

                def read_output():
                    for line in child.stdout:
                        output.put(line)
                    output.put(None)

                reader = threading.Thread(target=read_output)
                reader.start()

                def message():
                    line = output.get(timeout=120)
                    if line is None:
                        raise RuntimeError("consumer exited before its next barrier")
                    return json.loads(line)

                process = psutil.Process(child.pid)
                result["startup"] = message()
                if result["startup"]["phase"] == "failed_start":
                    error = result["startup"].get("error", "").lower()
                    resource_error = any(
                        message in error
                        for message in [
                            "resource temporarily unavailable",
                            "cannot allocate memory",
                            "not enough memory",
                            "insufficient system resources",
                        ]
                    )
                    result["status"] = "resource_limit" if resource_error else "failed"
                    result["error"] = result["startup"].get("error", "unknown startup error")
                    child.wait(timeout=60)
                else:
                    result["connected"] = snapshot(process)
                    time.sleep(0.2)
                    result["idle"] = snapshot(process)
                    if scenario == "reconnect":
                        stop(broker)
                        time.sleep(0.1)
                        broker = broker_start(args.mosquitto, config, port, broker_log)
                    child.stdin.write("\n")
                    child.stdin.flush()
                    result["measurements"] = message()
                    result["after_teardown"] = snapshot(process)
                    child.stdin.write("\n")
                    child.stdin.flush()
                    child.wait(timeout=60)
                    if child.returncode != 0:
                        raise RuntimeError(f"consumer exit status {child.returncode}")
                    result["status"] = "passed"
                    result["distributions"] = {
                        name: distribution(result["measurements"].get(name, []))
                        for name in ["admission_ns", "completion_ns", "event_ns", "loop_delay_ns"]
                    }
            except Exception as error:
                result["status"] = "failed"
                result["error"] = str(error)
            finally:
                if child is not None:
                    stop(child)
                    if "reader" in locals():
                        reader.join(timeout=10)
                if broker is not None:
                    stop(broker)
    (args.output / f"{label}.json").write_text(json.dumps(result, indent=2) + "\n")
    print(f"{label}: {result['status']}", flush=True)
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--consumers", nargs="+", choices=["c", "python"])
    parser.add_argument("--python-library", type=Path)
    parser.add_argument("--c-library", type=Path, help="Loaded shared C library, for artifact hashing")
    parser.add_argument("--shards-library", type=Path)
    parser.add_argument("--shards-binary", type=Path, help="Private two-current-thread-runtime prototype only")
    parser.add_argument("--shards-python-library", type=Path)
    parser.add_argument("--mosquitto", default=os.environ.get("MOSQUITTO_BIN", "mosquitto"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--counts", type=int, nargs="+", default=[1, 10, 100, 1000])
    parser.add_argument("--protocols", type=int, nargs="+", choices=[1, 2], default=[1, 2])
    parser.add_argument("--qos", type=int, nargs="+", choices=[0, 1, 2], default=[0, 1, 2])
    parser.add_argument(
        "--scenarios",
        nargs="+",
        choices=["idle", "publish", "incoming", "periodic", "reconnect", "hotspot"],
        default=["idle", "publish", "incoming", "periodic", "reconnect"],
    )
    parser.add_argument("--transports", nargs="+", choices=["tcp", "tls", "ws", "wss"], default=["tcp"])
    parser.add_argument("--repetitions", type=int, default=7)
    parser.add_argument("--rounds", type=int, default=10)
    args = parser.parse_args()
    if any(count < 1 or count > 1000 for count in args.counts) or not 1 <= args.rounds <= 1000 or args.repetitions < 1:
        parser.error("Counts and rounds must be 1..1000 and repetitions must be positive")
    if args.consumers and "python" in args.consumers and not args.python_library:
        parser.error("The Python consumer requires --python-library")
    args.output.mkdir(parents=True, exist_ok=True)
    metadata = {
        "platform": platform.platform(),
        "python": sys.version,
        "cpus": os.cpu_count(),
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=WORKSPACE, text=True).strip(),
        "broker": subprocess.run([args.mosquitto, "-h"], capture_output=True, text=True).stdout.splitlines()[:3],
        "rounds": args.rounds,
        "repetitions": args.repetitions,
        "binary": str(args.binary.resolve()),
        "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
        "c_library_sha256": hashlib.sha256(args.c_library.read_bytes()).hexdigest() if args.c_library else None,
        "shards_library_sha256": hashlib.sha256(args.shards_library.read_bytes()).hexdigest()
        if args.shards_library
        else None,
        "source_sha256": {
            str(path.relative_to(WORKSPACE)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in [
                WORKSPACE / "Cargo.lock",
                *sorted((WORKSPACE / "wrapper-core/src").rglob("*.rs")),
                *sorted((WORKSPACE / "c/src").rglob("*.rs")),
                *sorted((WORKSPACE / "python/src").rglob("*.rs")),
            ]
        },
        "resource_environment": {key: os.environ.get(key) for key in ["RUST_MIN_STACK", "MALLOC_ARENA_MAX"]},
        "python_library_sha256": hashlib.sha256(args.python_library.read_bytes()).hexdigest()
        if args.python_library
        else None,
        "shards_binary_sha256": hashlib.sha256(args.shards_binary.read_bytes()).hexdigest()
        if args.shards_binary
        else None,
        "shards_python_library_sha256": hashlib.sha256(args.shards_python_library.read_bytes()).hexdigest()
        if args.shards_python_library
        else None,
    }
    (args.output / "environment.json").write_text(json.dumps(metadata, indent=2) + "\n")
    stage = None
    if args.shards_binary and args.python_library and not args.shards_python_library:
        parser.error("Python shard comparisons require --shards-python-library")
    if args.python_library:
        stage = []
        for name, library in [("shared", args.python_library), ("shards", args.shards_python_library)]:
            destination = WORKSPACE / f"target/execution-python-{name}"
            if library:
                shutil.copytree(WORKSPACE / "python/python/rumqttc", destination / "rumqttc", dirs_exist_ok=True)
                shutil.copy2(library, destination / "rumqttc" / ("_native" + sysconfig.get_config_var("EXT_SUFFIX")))
            stage.append(destination)
    failures = []
    for repetition in range(args.repetitions):
        consumers = args.consumers or (["c", "python"] if stage else ["c"])
        for consumer, count, protocol, qos, scenario, transport in itertools.product(
            consumers, args.counts, args.protocols, args.qos, args.scenarios, args.transports
        ):
            if scenario in ["idle", "reconnect"] and qos != args.qos[0]:
                continue
            modes = ["dedicated", "shared"] + (["shards"] if args.shards_binary else [])
            offset = repetition % len(modes)
            modes = modes[offset:] + modes[:offset]
            for mode in modes:
                result = run_case(args, consumer, mode, count, protocol, qos, scenario, transport, repetition, stage)
                if result["status"] == "failed":
                    failures.append(result["case"])
    return bool(failures)


if __name__ == "__main__":
    raise SystemExit(main())
