#!/usr/bin/env python3
"""Start a reproducible Mosquitto broker and validate broker-backed scenarios."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any

BENCHMARKS_DIR = Path(__file__).resolve().parent
REPO_ROOT = BENCHMARKS_DIR.parent
if str(BENCHMARKS_DIR) not in sys.path:
    sys.path.insert(0, str(BENCHMARKS_DIR))

import runner  # noqa: E402

DEFAULT_DOCKER_IMAGE = "eclipse-mosquitto:2.0"
CONTAINER_TCP_PORT = 1883
CONTAINER_TLS_PORT = 8883
CONTAINER_WEBSOCKET_PORT = 9001
VALID_FIXTURE_TRANSPORTS = {"all", "tcp", "tls", "websocket"}
SUMMARY_FILENAME = "broker-validation-summary.json"


@dataclass(frozen=True)
class BrokerPorts:
    tcp: int
    tls: int
    websocket: int

    def as_dict(self) -> dict[str, int]:
        return {
            "tcp": self.tcp,
            "tls": self.tls,
            "websocket": self.websocket,
        }

    def selected(self, transports: list[str] | tuple[str, ...] | None) -> dict[str, int]:
        ports = self.as_dict()
        if transports is None:
            return ports
        return {transport: ports[transport] for transport in transports}


@dataclass(frozen=True)
class BrokerPaths:
    root: Path
    config_dir: Path
    config: Path
    ca_cert: Path
    cert: Path
    key: Path


@dataclass(frozen=True)
class ScenarioRef:
    name: str
    path: Path
    data: dict[str, Any]

    @property
    def transport(self) -> str | None:
        value = self.data.get("transport")
        return value if isinstance(value, str) else None

    @property
    def requires_broker(self) -> bool:
        return bool(self.data.get("requires_broker"))

    @property
    def is_soak(self) -> bool:
        return "soak" in self.name


class FixtureError(RuntimeError):
    """An expected fixture setup or validation error."""


def utc_timestamp() -> str:
    return dt.datetime.now(dt.UTC).strftime("%Y%m%d-%H%M%SZ")


def run_process(
    cmd: list[str],
    *,
    cwd: Path | None = None,
    timeout: int | None = None,
    check: bool = False,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=str(cwd) if cwd else None,
        timeout=timeout,
        text=True,
        capture_output=True,
        check=check,
    )


def free_local_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def allocate_ports() -> BrokerPorts:
    ports: set[int] = set()
    while len(ports) < 3:
        ports.add(free_local_port())
    tcp, tls, websocket = sorted(ports)
    return BrokerPorts(tcp=tcp, tls=tls, websocket=websocket)


def wait_for_port(host: str, port: int, *, timeout_sec: float) -> bool:
    deadline = time.monotonic() + timeout_sec
    while time.monotonic() < deadline:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            sock.settimeout(0.25)
            try:
                sock.connect((host, port))
                return True
            except OSError:
                time.sleep(0.1)
    return False


def wait_for_broker_ports(
    ports: BrokerPorts,
    *,
    timeout_sec: float,
    transports: list[str] | tuple[str, ...] | None = None,
) -> None:
    for name, port in ports.selected(transports).items():
        if not wait_for_port("127.0.0.1", port, timeout_sec=timeout_sec):
            raise FixtureError(f"broker {name} listener did not become ready on port {port}")


def container_mosquitto_config() -> str:
    return mosquitto_config(
        tcp_port=CONTAINER_TCP_PORT,
        tls_port=CONTAINER_TLS_PORT,
        websocket_port=CONTAINER_WEBSOCKET_PORT,
        certfile="/mosquitto/config/certs/server.crt",
        keyfile="/mosquitto/config/certs/server.key",
    )


def mosquitto_config(
    *,
    tcp_port: int,
    tls_port: int,
    websocket_port: int,
    certfile: str,
    keyfile: str,
    bind_host: str | None = None,
    include_websocket: bool = True,
) -> str:
    host = f" {bind_host}" if bind_host else ""
    lines = [
        "persistence false",
        "allow_anonymous true",
        "log_type error",
        "",
        f"listener {tcp_port}{host}",
        "protocol mqtt",
        "",
        f"listener {tls_port}{host}",
        "protocol mqtt",
        f"certfile {certfile}",
        f"keyfile {keyfile}",
        "",
    ]
    if include_websocket:
        lines.extend(
            [
                f"listener {websocket_port}{host}",
                "protocol websockets",
                "",
            ]
        )
    return "\n".join(lines)


def create_self_signed_certificate(paths: BrokerPaths) -> None:
    if shutil.which("openssl") is None:
        raise FixtureError("openssl is required to generate the temporary Mosquitto TLS certificate")

    extfile = paths.root / "openssl-san.cnf"
    extfile.write_text(
        "\n".join(
            [
                "[req]",
                "distinguished_name=req_distinguished_name",
                "x509_extensions=v3_req",
                "prompt=no",
                "[req_distinguished_name]",
                "CN=localhost",
                "[v3_req]",
                "subjectAltName=@alt_names",
                "[alt_names]",
                "DNS.1=localhost",
                "IP.1=127.0.0.1",
                "",
            ]
        ),
        encoding="utf-8",
    )
    cmd = [
        "openssl",
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-days",
        "1",
        "-keyout",
        str(paths.key),
        "-out",
        str(paths.cert),
        "-config",
        str(extfile),
        "-extensions",
        "v3_req",
    ]
    proc = run_process(cmd, timeout=30)
    if proc.returncode != 0:
        raise FixtureError(f"failed to generate temporary TLS certificate: {proc.stderr.strip()}")
    shutil.copyfile(paths.cert, paths.ca_cert)
    paths.cert.chmod(0o644)
    paths.ca_cert.chmod(0o644)
    paths.key.chmod(0o644)


def prepare_broker_paths(temp_root: Path, *, config_text: str) -> BrokerPaths:
    config_dir = temp_root / "config"
    certs_dir = config_dir / "certs"
    certs_dir.mkdir(parents=True, exist_ok=True)
    paths = BrokerPaths(
        root=temp_root,
        config_dir=config_dir,
        config=config_dir / "mosquitto.conf",
        ca_cert=certs_dir / "ca.crt",
        cert=certs_dir / "server.crt",
        key=certs_dir / "server.key",
    )
    create_self_signed_certificate(paths)
    paths.config.write_text(config_text, encoding="utf-8")
    return paths


def docker_image_from_env() -> str:
    return os.environ.get("RUMQTT_BENCH_MOSQUITTO_IMAGE", DEFAULT_DOCKER_IMAGE)


def build_docker_run_command(
    *,
    container_name: str,
    image: str,
    ports: BrokerPorts,
    config_dir: Path,
) -> list[str]:
    return [
        "docker",
        "run",
        "-d",
        "--name",
        container_name,
        "-p",
        f"127.0.0.1:{ports.tcp}:{CONTAINER_TCP_PORT}",
        "-p",
        f"127.0.0.1:{ports.tls}:{CONTAINER_TLS_PORT}",
        "-p",
        f"127.0.0.1:{ports.websocket}:{CONTAINER_WEBSOCKET_PORT}",
        "-v",
        f"{config_dir}:/mosquitto/config:ro",
        image,
        "mosquitto",
        "-c",
        "/mosquitto/config/mosquitto.conf",
    ]


class DockerBroker:
    def __init__(self, *, image: str, ports: BrokerPorts, paths: BrokerPaths) -> None:
        self.image = image
        self.ports = ports
        self.paths = paths
        self.container_name = f"rumqtt-bench-{uuid.uuid4().hex[:12]}"

    @property
    def backend_name(self) -> str:
        return "docker"

    def start(self) -> None:
        if shutil.which("docker") is None:
            raise FixtureError("docker is required for --backend docker")
        cmd = build_docker_run_command(
            container_name=self.container_name,
            image=self.image,
            ports=self.ports,
            config_dir=self.paths.config_dir,
        )
        proc = run_process(cmd, cwd=REPO_ROOT)
        if proc.returncode != 0:
            raise FixtureError(f"failed to start Mosquitto Docker container: {proc.stderr.strip()}")
        try:
            wait_for_broker_ports(self.ports, timeout_sec=15.0)
        except Exception:
            logs = run_process(["docker", "logs", self.container_name], timeout=10)
            detail = logs.stderr.strip() or logs.stdout.strip()
            if detail:
                raise FixtureError(f"broker container did not become ready: {detail}")
            raise

    def stop(self) -> None:
        run_process(["docker", "rm", "-f", self.container_name], timeout=30)

    def metadata(self) -> dict[str, Any]:
        return {
            "backend": self.backend_name,
            "image": self.image,
            "container": self.container_name,
        }


def probe_system_mosquitto_websockets(mosquitto_bin: str) -> tuple[bool, str]:
    with tempfile.TemporaryDirectory(prefix="rumqtt-mosquitto-probe-") as temp:
        root = Path(temp)
        config = root / "probe.conf"
        port = free_local_port()
        config.write_text(
            "\n".join(
                [
                    "allow_anonymous true",
                    "persistence false",
                    f"listener {port} 127.0.0.1",
                    "protocol websockets",
                    "",
                ]
            ),
            encoding="utf-8",
        )
        proc = subprocess.Popen(
            [mosquitto_bin, "-c", str(config), "-v"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            deadline = time.monotonic() + 2.0
            while time.monotonic() < deadline:
                code = proc.poll()
                if code is not None:
                    stdout, stderr = proc.communicate(timeout=1)
                    message = (stderr or stdout).strip()
                    return False, message or f"mosquitto exited with status {code}"
                if wait_for_port("127.0.0.1", port, timeout_sec=0.1):
                    return True, ""
            return False, "timed out while probing Mosquitto websocket support"
        finally:
            if proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=2)


class SystemBroker:
    def __init__(
        self,
        *,
        mosquitto_bin: str,
        ports: BrokerPorts,
        paths: BrokerPaths,
        active_transports: list[str],
    ) -> None:
        self.mosquitto_bin = mosquitto_bin
        self.ports = ports
        self.paths = paths
        self.active_transports = active_transports
        self.proc: subprocess.Popen[str] | None = None
        self.log_handle = None
        self.log = paths.root / "mosquitto.log"

    @property
    def backend_name(self) -> str:
        return "system"

    def start(self) -> None:
        self.log_handle = self.log.open("w", encoding="utf-8")
        self.proc = subprocess.Popen(
            [self.mosquitto_bin, "-c", str(self.paths.config), "-v"],
            stdout=self.log_handle,
            stderr=subprocess.STDOUT,
            text=True,
        )
        try:
            wait_for_broker_ports(self.ports, timeout_sec=10.0, transports=self.active_transports)
        except Exception as exc:
            if self.proc.poll() is None:
                self.stop()
            detail = self.log.read_text(encoding="utf-8", errors="replace").strip()
            raise FixtureError(f"system Mosquitto did not become ready: {detail or exc}") from exc

    def stop(self) -> None:
        if self.proc is None:
            return
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)
        if self.log_handle is not None:
            self.log_handle.close()
            self.log_handle = None

    def metadata(self) -> dict[str, Any]:
        return {
            "backend": self.backend_name,
            "image": None,
            "mosquitto_binary": self.mosquitto_bin,
        }


def broker_urls(ports: BrokerPorts) -> dict[str, str]:
    return {
        "tcp": f"mqtt://127.0.0.1:{ports.tcp}",
        "tls": f"mqtts://localhost:{ports.tls}",
        "websocket": f"ws://127.0.0.1:{ports.websocket}/mqtt",
    }


def load_all_scenarios(root: Path = REPO_ROOT) -> list[ScenarioRef]:
    refs: list[ScenarioRef] = []
    for path in sorted((root / "benchmarks" / "scenarios").glob("*.toml")):
        loaded_path, data = runner.load_scenario(root, str(path))
        refs.append(ScenarioRef(name=data["name"], path=loaded_path, data=data))
    return refs


def select_scenarios(
    scenarios: list[ScenarioRef],
    *,
    transport: str,
    requested: list[str],
    include_soak: bool,
) -> tuple[list[ScenarioRef], list[dict[str, str]]]:
    requested_set = set(requested)
    available = {scenario.name: scenario for scenario in scenarios}
    skipped: list[dict[str, str]] = []
    selected: list[ScenarioRef] = []

    for name in sorted(requested_set - set(available)):
        skipped.append({"name": name, "reason": "not_found"})

    candidates = [
        scenario
        for scenario in scenarios
        if not requested_set or scenario.name in requested_set
    ]
    for scenario in candidates:
        reason = None
        if not scenario.requires_broker:
            reason = "does_not_require_broker"
        elif transport != "all" and scenario.transport != transport:
            reason = "transport_mismatch"
        elif scenario.is_soak and not include_soak:
            reason = "soak_skipped"

        if reason is None:
            selected.append(scenario)
        else:
            skipped.append({"name": scenario.name, "reason": reason})

    selected.sort(key=lambda scenario: scenario.name)
    skipped.sort(key=lambda entry: entry["name"])
    return selected, skipped


def build_runner_command(
    *,
    scenario: ScenarioRef,
    broker_url: str,
    output_dir: Path,
    runs: int,
    warmup_runs: int,
    cargo_profile: str,
    timeout_sec: int,
    ca_cert: Path | None,
) -> list[str]:
    cmd = [
        sys.executable,
        str(BENCHMARKS_DIR / "runner.py"),
        "run",
        "--scenario",
        scenario.name,
        "--broker-url",
        broker_url,
        "--runs",
        str(runs),
        "--warmup-runs",
        str(warmup_runs),
        "--cargo-profile",
        cargo_profile,
        "--timeout-sec",
        str(timeout_sec),
        "--output-dir",
        str(output_dir),
    ]
    if ca_cert is not None:
        cmd.extend(["--ca-cert", str(ca_cert)])
    return cmd


def run_validation_scenario(
    *,
    scenario: ScenarioRef,
    urls: dict[str, str],
    output_dir: Path,
    ca_cert_path: Path,
    runs: int,
    warmup_runs: int,
    cargo_profile: str,
    timeout_sec: int,
) -> dict[str, Any]:
    transport = scenario.transport or "tcp"
    scenario_output = output_dir / "scenarios" / scenario.name
    scenario_output.mkdir(parents=True, exist_ok=True)
    ca_cert = ca_cert_path if transport == "tls" else None
    cmd = build_runner_command(
        scenario=scenario,
        broker_url=urls[transport],
        output_dir=scenario_output,
        runs=runs,
        warmup_runs=warmup_runs,
        cargo_profile=cargo_profile,
        timeout_sec=timeout_sec,
        ca_cert=ca_cert,
    )
    try:
        proc = run_process(cmd, cwd=REPO_ROOT, timeout=timeout_sec + 60)
    except subprocess.TimeoutExpired as exc:
        return {
            "name": scenario.name,
            "transport": transport,
            "status": "failed",
            "output_dir": str(scenario_output),
            "command": cmd,
            "returncode": None,
            "error": f"runner timed out after {exc.timeout} seconds",
        }
    result: dict[str, Any] = {
        "name": scenario.name,
        "transport": transport,
        "status": "completed" if proc.returncode == 0 else "failed",
        "output_dir": str(scenario_output),
        "command": cmd,
        "returncode": proc.returncode,
    }
    if proc.stdout.strip():
        result["stdout"] = proc.stdout.strip()
    if proc.stderr.strip():
        result["stderr"] = proc.stderr.strip()

    summary_file = scenario_output / "summary.json"
    if summary_file.exists():
        try:
            summary = json.loads(summary_file.read_text(encoding="utf-8"))
            result["quality_status"] = summary.get("quality", {}).get("status")
        except json.JSONDecodeError:
            result["quality_status"] = "unreadable"
    return result


def write_summary(path: Path, summary: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def build_system_config(ports: BrokerPorts, paths_root: Path, *, include_websocket: bool) -> str:
    cert = paths_root / "config" / "certs" / "server.crt"
    key = paths_root / "config" / "certs" / "server.key"
    return mosquitto_config(
        tcp_port=ports.tcp,
        tls_port=ports.tls,
        websocket_port=ports.websocket,
        certfile=str(cert),
        keyfile=str(key),
        bind_host="127.0.0.1",
        include_websocket=include_websocket,
    )


def make_broker(
    *,
    backend: str,
    ports: BrokerPorts,
    paths: BrokerPaths,
    mosquitto_bin: str | None,
    image: str,
    active_transports: list[str],
) -> DockerBroker | SystemBroker:
    if backend == "docker":
        return DockerBroker(image=image, ports=ports, paths=paths)
    if backend == "system":
        binary = mosquitto_bin or shutil.which("mosquitto")
        if binary is None:
            raise FixtureError("mosquitto is required for --backend system")
        return SystemBroker(
            mosquitto_bin=binary,
            ports=ports,
            paths=paths,
            active_transports=active_transports,
        )
    raise FixtureError(f"unsupported backend: {backend}")


def command_validate(args: argparse.Namespace) -> int:
    if args.transport not in VALID_FIXTURE_TRANSPORTS:
        raise FixtureError(f"unsupported transport: {args.transport}")

    root = REPO_ROOT
    output_dir = Path(args.output_dir).resolve() if args.output_dir else (
        root / "benchmarks" / "results" / "broker-validation" / utc_timestamp()
    )
    output_dir.mkdir(parents=True, exist_ok=True)
    selected, skipped = select_scenarios(
        load_all_scenarios(root),
        transport=args.transport,
        requested=args.scenario,
        include_soak=args.include_soak,
    )
    if not selected:
        summary = {
            "schema_version": 1,
            "mode": "broker-validation",
            "status": "failed",
            "backend": args.backend,
            "image": docker_image_from_env() if args.backend == "docker" else None,
            "completed": [],
            "failed": [],
            "skipped": skipped,
            "scenarios": [],
            "counts": {"completed": 0, "failed": 0, "skipped": len(skipped)},
            "error": "no scenarios selected",
        }
        write_summary(output_dir / SUMMARY_FILENAME, summary)
        raise FixtureError("no scenarios selected")

    needs_websocket = any(scenario.transport == "websocket" for scenario in selected)
    active_transports = sorted({scenario.transport or "tcp" for scenario in selected})
    if args.backend == "system" and needs_websocket:
        binary = args.mosquitto_bin or shutil.which("mosquitto")
        if binary is None:
            raise FixtureError("mosquitto is required for --backend system")
        supported, message = probe_system_mosquitto_websockets(binary)
        if not supported:
            raise FixtureError(
                "system Mosquitto does not support websocket listeners; "
                f"use --backend docker or a websocket-enabled binary. Probe output: {message}"
            )

    temp = tempfile.TemporaryDirectory(prefix="rumqtt-bench-broker-")
    started_at = utc_timestamp()
    scenario_results: list[dict[str, Any]] = []
    broker: DockerBroker | SystemBroker | None = None
    summary_path = output_dir / SUMMARY_FILENAME
    ports = allocate_ports()
    urls = broker_urls(ports)
    try:
        temp_root = Path(temp.name)
        config = (
            container_mosquitto_config()
            if args.backend == "docker"
            else build_system_config(ports, temp_root, include_websocket=needs_websocket)
        )
        paths = prepare_broker_paths(temp_root, config_text=config)
        persistent_ca_cert = output_dir / "ca.crt"
        shutil.copyfile(paths.ca_cert, persistent_ca_cert)
        broker = make_broker(
            backend=args.backend,
            ports=ports,
            paths=paths,
            mosquitto_bin=args.mosquitto_bin,
            image=docker_image_from_env(),
            active_transports=active_transports,
        )
        broker.start()
        for scenario in selected:
            scenario_results.append(
                run_validation_scenario(
                    scenario=scenario,
                    urls=urls,
                    output_dir=output_dir,
                    ca_cert_path=persistent_ca_cert,
                    runs=args.runs,
                    warmup_runs=args.warmup_runs,
                    cargo_profile=args.cargo_profile,
                    timeout_sec=args.timeout_sec,
                )
            )
    finally:
        if broker is not None:
            broker.stop()
        temp.cleanup()

    completed = [result["name"] for result in scenario_results if result["status"] == "completed"]
    failed = [result["name"] for result in scenario_results if result["status"] == "failed"]
    metadata = broker.metadata() if broker is not None else {
        "backend": args.backend,
        "image": docker_image_from_env() if args.backend == "docker" else None,
    }
    summary = {
        "schema_version": 1,
        "mode": "broker-validation",
        "status": "failed" if failed else "completed",
        "started_at": started_at,
        "finished_at": utc_timestamp(),
        "backend": metadata["backend"],
        "image": metadata.get("image"),
        "mosquitto_binary": metadata.get("mosquitto_binary"),
        "container": metadata.get("container"),
        "ports": ports.as_dict(),
        "broker_urls": urls,
        "ca_cert": str(output_dir / "ca.crt"),
        "completed": completed,
        "failed": failed,
        "skipped": skipped,
        "counts": {
            "completed": len(completed),
            "failed": len(failed),
            "skipped": len(skipped),
        },
        "scenarios": scenario_results,
    }
    write_summary(summary_path, summary)
    if failed:
        raise FixtureError(f"{len(failed)} scenario(s) failed; summary written to {summary_path}")
    print(f"Broker validation complete: {summary_path}")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    validate = sub.add_parser("validate", help="Run broker-backed scenarios against a fixture broker")
    validate.add_argument("--backend", choices=["docker", "system"], default="docker")
    validate.add_argument("--transport", choices=sorted(VALID_FIXTURE_TRANSPORTS), default="all")
    validate.add_argument("--scenario", action="append", default=[])
    validate.add_argument("--runs", type=int, default=1)
    validate.add_argument("--warmup-runs", type=int, default=0)
    validate.add_argument("--cargo-profile", choices=sorted(runner.VALID_CARGO_PROFILES), default="dev")
    validate.add_argument("--timeout-sec", type=int, default=300)
    validate.add_argument("--output-dir")
    validate.add_argument("--mosquitto-bin")
    soak = validate.add_mutually_exclusive_group()
    soak.add_argument("--include-soak", action="store_true", dest="include_soak")
    soak.add_argument("--skip-soak", action="store_false", dest="include_soak")
    validate.set_defaults(include_soak=False)
    validate.set_defaults(func=command_validate)
    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    if hasattr(args, "runs") and args.runs <= 0:
        parser.error("--runs must be greater than zero")
    if hasattr(args, "warmup_runs") and args.warmup_runs < 0:
        parser.error("--warmup-runs must be non-negative")
    if hasattr(args, "timeout_sec") and args.timeout_sec <= 0:
        parser.error("--timeout-sec must be greater than zero")
    try:
        return int(args.func(args))
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
