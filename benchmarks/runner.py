#!/usr/bin/env python3
"""Scenario runner and branch-comparison tool for rumqtt benchmarks."""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import hashlib
import html
import json
import math
import os
import platform
import random
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path
from typing import Any


OUTPUT_SCHEMA_VERSION = 1
MATCHED_OUTPUT_SCHEMA_VERSION = 2
VALID_CARGO_PROFILES = {"dev", "release"}
VALID_TRANSPORTS = {"tcp", "tls", "websocket"}
MATCHED_TRANSPORTS = {"tcp", "tls"}
VALID_CARGO_FEATURES = {"alloc-metrics", "url", "websocket"}
QUALITY_FIELDS = {
    "min_success_rate",
    "min_measured_runs",
    "max_primary_cv_pct",
    "max_primary_mad_pct",
    "max_relative_ci_width_pct",
}
LOWER_IS_BETTER_BACKLOG_METRICS = {
    "common_publish_outstanding_at_deadline",
    "common_publish_outstanding_peak",
    "common_publish_outstanding_after_drain",
    "cycles_in_flight_at_deadline",
    "drain_successful_cycles",
}


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


def repo_root(cwd: Path | None = None) -> Path:
    proc = run_process(["git", "rev-parse", "--show-toplevel"], cwd=cwd, check=True)
    return Path(proc.stdout.strip())


def resolve_ref(root: Path, ref: str) -> str:
    proc = run_process(["git", "rev-parse", "--verify", f"{ref}^{{commit}}"], cwd=root)
    if proc.returncode != 0:
        raise RuntimeError(f"cannot resolve git ref: {ref}")
    return proc.stdout.strip()


def current_ref(root: Path) -> str:
    proc = run_process(["git", "rev-parse", "--abbrev-ref", "HEAD"], cwd=root, check=True)
    return proc.stdout.strip()


def scenario_file_hash(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def parse_git_dirty(porcelain: str) -> bool:
    return bool(porcelain.strip())


def cargo_lock_sha256(root: Path) -> str | None:
    path = root / "Cargo.lock"
    return hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None


def resolved_packages(metadata: dict[str, Any]) -> dict[str, dict[str, Any]]:
    packages = metadata.get("packages")
    if not isinstance(packages, list):
        return {}
    resolved: dict[str, dict[str, Any]] = {}
    for package in packages:
        if not isinstance(package, dict) or package.get("name") not in {
            "rumqttc-v5-next",
            "mqtt5",
        }:
            continue
        resolved[str(package["name"])] = {
            "version": package.get("version"),
            "source": package.get("source"),
            "manifest_path": package.get("manifest_path"),
        }
    return resolved


def collect_matched_provenance(
    root: Path, *, cargo_profile: str, cargo_features: list[str]
) -> dict[str, Any]:
    commit_proc = run_process(["git", "rev-parse", "HEAD"], cwd=root)
    dirty_proc = run_process(
        ["git", "status", "--porcelain=v1", "--untracked-files=normal"], cwd=root
    )
    metadata_proc = run_process(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=root,
    )
    metadata: dict[str, Any] = {}
    if metadata_proc.returncode == 0:
        try:
            parsed = json.loads(metadata_proc.stdout)
            if isinstance(parsed, dict):
                metadata = parsed
        except json.JSONDecodeError:
            pass
    packages = resolved_packages(metadata)
    return {
        "workspace_commit": commit_proc.stdout.strip() if commit_proc.returncode == 0 else None,
        "working_tree_dirty": (
            parse_git_dirty(dirty_proc.stdout) if dirty_proc.returncode == 0 else None
        ),
        "cargo_lock_sha256": cargo_lock_sha256(root),
        "cargo_profile": cargo_profile,
        "cargo_features": sorted(set(cargo_features)),
        "rumqttc-v5-next": packages.get("rumqttc-v5-next"),
        "mqtt5": packages.get("mqtt5"),
    }


def certificate_metadata(ca_cert: str | None) -> dict[str, str] | None:
    if ca_cert is None:
        return None
    path = Path(ca_cert).expanduser().resolve()
    if not path.is_file():
        raise RuntimeError(f"CA certificate does not exist or is not a file: {path}")
    return {
        "path": str(path),
        "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
    }


def rustc_version(root: Path) -> str | None:
    proc = run_process(["rustc", "--version"], cwd=root)
    if proc.returncode != 0:
        return None
    return proc.stdout.strip()


def fallback_environment(root: Path) -> dict[str, Any]:
    return {
        "rustc": rustc_version(root),
        "os": platform.system().lower(),
        "arch": platform.machine(),
        "cpu_count": os.cpu_count() or 1,
        "cargo_lock_sha256": cargo_lock_sha256(root),
    }


def load_scenario(root: Path, scenario: str) -> tuple[Path, dict[str, Any]]:
    path = Path(scenario)
    if not path.suffix:
        candidates = [
            root / "benchmarks" / "scenarios" / f"{scenario}.toml",
            root / "session-store-file" / "benchmarks" / "scenarios" / f"{scenario}.toml",
        ]
        path = next((candidate for candidate in candidates if candidate.exists()), candidates[0])
    elif not path.is_absolute():
        path = root / path

    if not path.exists():
        raise RuntimeError(f"scenario not found: {path}")

    with path.open("rb") as handle:
        data = tomllib.load(handle)
    validate_scenario(path, data)
    return path, data


def validate_scenario(path: Path, scenario: dict[str, Any]) -> None:
    for key in ("name", "group", "command", "description", "primary_metric"):
        if not isinstance(scenario.get(key), str):
            raise RuntimeError(f"{path}: missing string field '{key}'")
        if not scenario[key].strip():
            raise RuntimeError(f"{path}: field '{key}' must not be empty")
    for key in ("higher_is_better", "requires_broker"):
        if not isinstance(scenario.get(key), bool):
            raise RuntimeError(f"{path}: missing boolean field '{key}'")
    if scenario["group"] not in {"client", "matched", "codec", "options", "persistence"}:
        raise RuntimeError(f"{path}: unsupported benchmark group")
    commands = {
        "client": {"throughput", "latency", "connections"},
        "matched": {"throughput", "latency", "connections"},
        "codec": {"encode", "decode", "roundtrip"},
        "options": {"parse-url"},
        "persistence": {"envelope", "codec", "file-store", "coordination", "growth", "mqtt"},
    }
    if scenario["command"] not in commands[scenario["group"]]:
        raise RuntimeError(f"{path}: unsupported command for group '{scenario['group']}'")
    if "args" in scenario and not isinstance(scenario["args"], dict):
        raise RuntimeError(f"{path}: args must be a table")
    validate_transport(path, scenario)
    validate_cargo_features(path, scenario.get("cargo_features"))
    expected_requires_broker = scenario["group"] in {"client", "matched"} or (
        scenario["group"] == "persistence" and scenario["command"] == "mqtt"
    )
    if scenario["requires_broker"] != expected_requires_broker:
        expected = "true" if expected_requires_broker else "false"
        raise RuntimeError(f"{path}: requires_broker must be {expected} for {scenario['group']} scenarios")
    validate_quality(path, scenario.get("quality"))
    if scenario["group"] == "matched":
        validate_matched_args(path, scenario)


def validate_matched_args(path: Path, scenario: dict[str, Any]) -> None:
    args = scenario.get("args")
    if not isinstance(args, dict):
        raise RuntimeError(f"{path}: matched scenarios require an args table")
    common = {
        "duration_sec",
        "keepalive_sec",
    }
    message = {
        "warmup_sec",
        "drain_sec",
        "payload_size",
        "qos",
        "topic",
        "window",
        "receive_maximum",
        "operation_timeout_sec",
    }
    required = common | (
        {
            "concurrency",
            "connect_timeout_sec",
            "disconnect_timeout_sec",
            "drain_sec",
        }
        if scenario["command"] == "connections"
        else message
    )
    if scenario["command"] == "throughput":
        required |= {"publishers", "subscribers"}
    elif scenario["command"] == "latency":
        required.add("rate")
    missing = sorted(required - set(args))
    if missing:
        raise RuntimeError(f"{path}: matched args missing fields: {', '.join(missing)}")
    for key in required - {"topic", "qos", "payload_size", "warmup_sec"}:
        value = args[key]
        if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
            raise RuntimeError(f"{path}: args.{key} must be a positive integer")
    for key in {"payload_size", "warmup_sec"} & required:
        value = args[key]
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise RuntimeError(f"{path}: args.{key} must be a non-negative integer")
    if "qos" in required and args["qos"] not in {0, 1}:
        raise RuntimeError(f"{path}: args.qos must be 0 or 1")
    if "topic" in required and (
        not isinstance(args["topic"], str) or not args["topic"].strip()
    ):
        raise RuntimeError(f"{path}: args.topic must be a non-empty string")


def validate_transport(path: Path, scenario: dict[str, Any]) -> None:
    transport = scenario.get("transport")
    if transport is None:
        return
    if scenario["group"] not in {"client", "matched"} and not (
        scenario["group"] == "persistence" and scenario["command"] == "mqtt"
    ):
        raise RuntimeError(f"{path}: transport is only supported for client, matched, or persistence MQTT scenarios")
    if not isinstance(transport, str) or transport not in VALID_TRANSPORTS:
        allowed = ", ".join(sorted(VALID_TRANSPORTS))
        raise RuntimeError(f"{path}: transport must be one of: {allowed}")
    if scenario["group"] == "matched" and transport not in MATCHED_TRANSPORTS:
        allowed = ", ".join(sorted(MATCHED_TRANSPORTS))
        raise RuntimeError(f"{path}: matched transport must be one of: {allowed}")


def validate_cargo_features(path: Path, features: Any) -> None:
    if features is None:
        return
    if not isinstance(features, list):
        raise RuntimeError(f"{path}: cargo_features must be an array")
    for feature in features:
        if not isinstance(feature, str) or not feature:
            raise RuntimeError(f"{path}: cargo_features entries must be non-empty strings")
        if feature not in VALID_CARGO_FEATURES:
            allowed = ", ".join(sorted(VALID_CARGO_FEATURES))
            raise RuntimeError(f"{path}: unsupported cargo feature '{feature}', expected one of: {allowed}")


def validate_quality(path: Path, quality: Any) -> None:
    if not isinstance(quality, dict):
        raise RuntimeError(f"{path}: missing quality table")
    missing = sorted(QUALITY_FIELDS - set(quality))
    if missing:
        raise RuntimeError(f"{path}: quality table missing fields: {', '.join(missing)}")
    extra = sorted(set(quality) - QUALITY_FIELDS)
    if extra:
        raise RuntimeError(f"{path}: quality table has unsupported fields: {', '.join(extra)}")
    for key in (
        "min_success_rate",
        "max_primary_cv_pct",
        "max_primary_mad_pct",
        "max_relative_ci_width_pct",
    ):
        value = quality.get(key)
        if isinstance(value, bool) or not isinstance(value, int | float) or not math.isfinite(value):
            raise RuntimeError(f"{path}: quality.{key} must be a finite number")
        if key == "min_success_rate" and not 0.0 <= float(value) <= 1.0:
            raise RuntimeError(f"{path}: quality.{key} must be between 0 and 1")
        if key != "min_success_rate" and float(value) < 0.0:
            raise RuntimeError(f"{path}: quality.{key} must be non-negative")
    min_runs = quality.get("min_measured_runs")
    if isinstance(min_runs, bool) or not isinstance(min_runs, int) or min_runs <= 0:
        raise RuntimeError(f"{path}: quality.min_measured_runs must be a positive integer")


def scenario_metadata(scenario: dict[str, Any]) -> dict[str, Any]:
    metadata = {
        "name": scenario["name"],
        "description": scenario["description"],
        "primary_metric": scenario["primary_metric"],
        "higher_is_better": scenario["higher_is_better"],
        "requires_broker": scenario["requires_broker"],
        "quality": dict(scenario["quality"]),
        "group": scenario["group"],
        "command": scenario["command"],
    }
    if "transport" in scenario:
        metadata["transport"] = scenario["transport"]
    if "cargo_features" in scenario:
        metadata["cargo_features"] = list(scenario["cargo_features"])
    return metadata


def validate_broker_requirement(scenario: dict[str, Any], broker_url: str | None) -> None:
    if scenario["requires_broker"] and broker_url is None:
        example_scheme = {
            "tcp": "mqtt",
            "tls": "mqtts",
            "websocket": "ws",
        }.get(scenario.get("transport"), "mqtt")
        raise RuntimeError(
            f"{scenario['name']} requires an external broker; "
            f"pass --broker-url {example_scheme}://host:port"
        )
    if broker_url is None or "transport" not in scenario:
        return
    scheme = broker_url.split(":", 1)[0].lower()
    if scenario["group"] == "matched":
        expected_schemes = {
            "tcp": {"mqtt"},
            "tls": {"mqtts"},
        }[scenario["transport"]]
    else:
        expected_schemes = {
            "tcp": {"mqtt"},
            "tls": {"mqtts", "ssl"},
            "websocket": {"ws"},
        }[scenario["transport"]]
    if scheme not in expected_schemes:
        expected = ", ".join(f"{value}://" for value in sorted(expected_schemes))
        raise RuntimeError(
            f"{scenario['name']} expects {scenario['transport']} broker transport; "
            f"use one of: {expected}"
        )


def scenario_command(
    scenario: dict[str, Any],
    *,
    run_id: str,
    broker_url: str | None,
    ca_cert: str | None,
    cargo_profile: str = "release",
) -> list[str]:
    if cargo_profile not in VALID_CARGO_PROFILES:
        raise RuntimeError(f"unsupported cargo profile: {cargo_profile}")

    cmd = ["cargo", "run"]
    if scenario["group"] == "persistence":
        cmd.extend(["--manifest-path", "session-store-file/Cargo.toml"])
    if cargo_profile == "release":
        cmd.append("--release")
    cargo_features = sorted(set(scenario.get("cargo_features", [])))
    if cargo_features:
        cmd.extend(["--features", ",".join(cargo_features)])
    package, binary = (
        ("session-store-file-benchmarks", "rumqtt-session-store-file-bench")
        if scenario["group"] == "persistence"
        else ("benchmarks", "rumqtt-bench")
    )
    cmd.extend([
        "-p",
        package,
        "--bin",
        binary,
        "--",
        scenario["group"],
        scenario["command"],
    ])
    args = dict(scenario.get("args", {}))
    args["run-id"] = run_id
    if broker_url is not None and (scenario["group"] == "client" or scenario["command"] == "mqtt"):
        args["broker-url"] = broker_url
    if ca_cert is not None and scenario["group"] == "client":
        args["ca-cert"] = ca_cert

    for key in sorted(args):
        value = args[key]
        flag = f"--{key.replace('_', '-')}"
        if isinstance(value, bool):
            if value:
                cmd.append(flag)
            continue
        cmd.extend([flag, str(value)])
    return cmd


def matched_command(
    scenario: dict[str, Any],
    *,
    client: str,
    run_id: str,
    broker_url: str,
    ca_cert: str | None = None,
    cargo_profile: str = "release",
) -> list[str]:
    if scenario["group"] != "matched":
        raise RuntimeError("library comparison requires a matched scenario")
    cmd = ["cargo", "run"]
    if cargo_profile == "release":
        cmd.append("--release")
    features = sorted(set(scenario.get("cargo_features", [])))
    if features:
        cmd.extend(["--features", ",".join(features)])
    cmd.extend([
        "-p", "benchmarks", "--bin", "rumqtt-library-bench", "--",
        "--client", client, "--run-id", run_id, scenario["command"],
    ])
    args = dict(scenario.get("args", {}))
    args["broker-url"] = broker_url
    if ca_cert is not None:
        args["ca-cert"] = ca_cert
    for key in sorted(args):
        value = args[key]
        flag = f"--{key.replace('_', '-')}"
        if isinstance(value, bool):
            if value:
                cmd.append(flag)
        else:
            cmd.extend([flag, str(value)])
    return cmd


def validate_external_scenario(scenario: dict[str, Any]) -> None:
    args = scenario.get("args", {})
    if (
        scenario["group"] != "client"
        or scenario["command"] not in {"throughput", "latency", "connections"}
        or args.get("protocol") != "v5"
    ):
        raise RuntimeError(
            "external mqttv5 comparison requires an MQTT v5 client throughput, latency, "
            "or connections scenario"
        )
    if scenario.get("transport") not in {"tcp", "tls"}:
        raise RuntimeError("external mqttv5 comparison supports only TCP and TLS scenarios")
    if scenario["command"] == "latency" and args.get("rate", 1000) != 1000:
        raise RuntimeError(
            "mqttv5-cli latency currently uses a fixed 1000 msg/s rate; "
            "select a scenario with rate = 1000"
        )


def external_comparison_scenario(scenario: dict[str, Any]) -> dict[str, Any]:
    comparable = dict(scenario)
    comparable["args"] = dict(scenario.get("args", {}))
    if scenario["command"] == "throughput":
        topic = str(comparable["args"].get("topic", "bench/rumqtt"))
        comparable["args"]["filter"] = f"{topic}/#"
    return comparable


def resolve_external_binary(external_bin: str) -> str:
    path = shutil.which(external_bin)
    if path is None:
        raise RuntimeError(f"cannot find mqttv5 executable: {external_bin}")
    return str(Path(path).resolve())


def external_version(external_bin: str) -> str:
    proc = run_process([external_bin, "--version"], timeout=30)
    if proc.returncode != 0:
        raise RuntimeError(f"failed to query mqttv5 version: {proc.stderr.strip()}")
    version = proc.stdout.strip()
    if not version:
        raise RuntimeError("mqttv5 --version returned empty output")
    return version


def external_command(
    scenario: dict[str, Any],
    *,
    external_bin: str,
    broker_url: str,
    ca_cert: str | None,
    run_id: str,
) -> list[str]:
    validate_external_scenario(scenario)
    args = scenario.get("args", {})
    command = scenario["command"]
    cmd = [
        external_bin,
        "bench",
        "--mode",
        command,
        "--duration",
        str(args.get("duration_sec", 10)),
        "--warmup",
        str(args.get("warmup_sec", 0)),
        "--url",
        broker_url,
        "--client-id",
        run_id,
    ]
    if command != "connections":
        cmd.extend(
            [
                "--payload-size",
                str(args.get("payload_size", 64)),
                "--qos",
                str(args.get("qos", 1)),
                "--topic",
                str(args.get("topic", "bench/rumqtt")),
            ]
        )
        filter_value = args.get("filter")
        if command == "throughput":
            topic = str(args.get("topic", "bench/rumqtt"))
            filter_value = filter_value or f"{topic}/#"
        if filter_value:
            cmd.extend(["--filter", str(filter_value)])
    if command == "throughput":
        cmd.extend(
            [
                "--publishers",
                str(args.get("publishers", 1)),
                "--subscribers",
                str(args.get("subscribers", 1)),
            ]
        )
    if command == "connections":
        cmd.extend(["--concurrency", str(args.get("concurrency", 10))])
    if ca_cert is not None:
        cmd.extend(["--ca-cert", ca_cert])
    return cmd


def normalize_external_payload(
    data: dict[str, Any],
    *,
    scenario: dict[str, Any],
    run_id: str,
    started_at_unix: int,
    finished_at_unix: int,
    external_bin: str,
    version: str,
) -> dict[str, Any]:
    mode = scenario["command"]
    if data.get("mode") != mode or not isinstance(data.get("config"), dict):
        raise RuntimeError(f"mqttv5 output must contain mode={mode!r} and a config object")
    results = data.get("results")
    if not isinstance(results, dict):
        raise RuntimeError("mqttv5 output must contain a results object")

    metric_maps = {
        "throughput": {
            "published": "published",
            "received": "received",
            "elapsed_secs": "elapsed_sec",
            "throughput_avg": "throughput_msg_sec",
        },
        "latency": {
            "messages": "messages",
            "min_us": "min_us",
            "max_us": "max_us",
            "avg_us": "avg_us",
            "p50_us": "p50_us",
            "p95_us": "p95_us",
            "p99_us": "p99_us",
        },
        "connections": {
            "successful": "successful",
            "failed": "failed",
            "elapsed_secs": "elapsed_sec",
            "connections_per_sec": "connections_sec",
            "avg_connect_us": "avg_connect_us",
            "p50_connect_us": "p50_connect_us",
            "p95_connect_us": "p95_connect_us",
            "p99_connect_us": "p99_connect_us",
        },
    }
    metrics = {}
    for source, target in metric_maps[mode].items():
        value = results.get(source)
        if isinstance(value, bool) or not isinstance(value, int | float) or not math.isfinite(value):
            raise RuntimeError(f"mqttv5 results.{source} must be a finite number")
        metrics[target] = float(value)
    sample_key = {
        "throughput": "received_per_sec",
        "latency": "latency_us",
        "connections": "connections_per_sec",
    }[mode]
    raw_samples = results.get("samples", [])
    if not isinstance(raw_samples, list) or any(
        isinstance(value, bool)
        or not isinstance(value, int | float)
        or not math.isfinite(value)
        for value in raw_samples
    ):
        raise RuntimeError("mqttv5 results.samples must be an array of finite numbers")
    payload = {
        "schema_version": OUTPUT_SCHEMA_VERSION,
        "run_id": run_id,
        "scenario": f"external-mqttv5-{mode}",
        "started_at_unix": started_at_unix,
        "finished_at_unix": finished_at_unix,
        "config": data["config"],
        "metrics": metrics,
        "samples": {sample_key: [float(value) for value in raw_samples]},
        "environment": {
            **fallback_environment(repo_root()),
            "external_tool": "mqttv5-cli",
            "external_version": version,
            "external_binary": external_bin,
        },
    }
    validate_benchmark_payload(payload, scenario)
    return payload


def read_external_json(stdout: str) -> dict[str, Any]:
    decoder = json.JSONDecoder()
    for offset, character in enumerate(stdout):
        if character != "{":
            continue
        try:
            data, end = decoder.raw_decode(stdout, offset)
        except json.JSONDecodeError:
            continue
        if stdout[end:].strip():
            continue
        if not isinstance(data, dict):
            raise RuntimeError("mqttv5 stdout JSON must be an object")
        return data
    raise RuntimeError("mqttv5 stdout did not end with a JSON object")


def run_external_once(
    *,
    root: Path,
    scenario: dict[str, Any],
    external_bin: str,
    version: str,
    run_id: str,
    broker_url: str,
    ca_cert: str | None,
    timeout: int,
) -> dict[str, Any]:
    cmd = external_command(
        scenario,
        external_bin=external_bin,
        broker_url=broker_url,
        ca_cert=ca_cert,
        run_id=run_id,
    )
    started_at = int(time.time())
    proc = run_process(cmd, cwd=root, timeout=timeout)
    finished_at = int(time.time())
    result: dict[str, Any] = {
        "run_id": run_id,
        "command": cmd,
        "returncode": proc.returncode,
        "stderr": proc.stderr,
    }
    if proc.returncode != 0:
        result["ok"] = False
        result["error"] = proc.stderr.strip() or proc.stdout.strip()
        return result
    try:
        raw = read_external_json(proc.stdout)
        payload = normalize_external_payload(
            raw,
            scenario=scenario,
            run_id=run_id,
            started_at_unix=started_at,
            finished_at_unix=finished_at,
            external_bin=external_bin,
            version=version,
        )
    except (json.JSONDecodeError, RuntimeError) as exc:
        result["ok"] = False
        result["error"] = str(exc)
        result["stdout"] = proc.stdout
        return result
    result["ok"] = True
    result["payload"] = payload
    result["metrics"] = dict(payload["metrics"])
    return result


def numeric_metric(metrics: dict[str, Any], metric: str) -> float:
    value = metrics.get(metric)
    if isinstance(value, bool) or not isinstance(value, int | float) or not math.isfinite(value):
        raise RuntimeError(f"benchmark metric '{metric}' must be a finite number")
    return float(value)


def validate_benchmark_payload(data: dict[str, Any], scenario: dict[str, Any]) -> None:
    expected_version = MATCHED_OUTPUT_SCHEMA_VERSION if scenario["group"] == "matched" else OUTPUT_SCHEMA_VERSION
    if data.get("schema_version") != expected_version:
        raise RuntimeError(
            f"benchmark JSON schema_version must be {expected_version}, got {data.get('schema_version')!r}"
        )
    for key in ("run_id", "scenario"):
        if not isinstance(data.get(key), str) or not data[key]:
            raise RuntimeError(f"benchmark JSON field '{key}' must be a non-empty string")
    for key in ("started_at_unix", "finished_at_unix"):
        if isinstance(data.get(key), bool) or not isinstance(data.get(key), int):
            raise RuntimeError(f"benchmark JSON field '{key}' must be an integer")
    for key in ("config", "metrics", "samples", "environment"):
        if not isinstance(data.get(key), dict):
            raise RuntimeError(f"benchmark JSON field '{key}' must be an object")
    if expected_version == MATCHED_OUTPUT_SCHEMA_VERSION:
        for key in ("client",):
            if data.get(key) not in {"rumqttc", "mqtt5"}:
                raise RuntimeError(f"benchmark JSON field '{key}' must identify a matched client")
        for key in ("effective_config", "quality"):
            if not isinstance(data.get(key), dict):
                raise RuntimeError(f"benchmark JSON field '{key}' must be an object")
        if not isinstance(data["quality"].get("valid"), bool):
            raise RuntimeError("benchmark JSON quality.valid must be a boolean")

    metrics = data["metrics"]
    for metric in metrics:
        numeric_metric(metrics, metric)
    numeric_metric(metrics, scenario["primary_metric"])

    for sample_name, values in data["samples"].items():
        if not isinstance(sample_name, str):
            raise RuntimeError("benchmark sample names must be strings")
        if not isinstance(values, list):
            raise RuntimeError(f"benchmark samples.{sample_name} must be an array")
        for value in values:
            if isinstance(value, bool) or not isinstance(value, int | float) or not math.isfinite(value):
                raise RuntimeError(f"benchmark samples.{sample_name} must contain only finite numbers")


def read_benchmark_json(stdout: str, scenario: dict[str, Any]) -> dict[str, Any]:
    try:
        data = json.loads(stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"benchmark stdout was not JSON: {exc}") from exc
    if not isinstance(data, dict):
        raise RuntimeError("benchmark JSON must be an object")
    validate_benchmark_payload(data, scenario)
    return data


def run_once(
    *,
    root: Path,
    scenario: dict[str, Any],
    run_id: str,
    broker_url: str | None,
    ca_cert: str | None,
    cargo_profile: str,
    timeout: int,
) -> dict[str, Any]:
    cmd = scenario_command(
        scenario,
        run_id=run_id,
        broker_url=broker_url,
        ca_cert=ca_cert,
        cargo_profile=cargo_profile,
    )
    proc = run_process(cmd, cwd=root, timeout=timeout)
    result: dict[str, Any] = {
        "run_id": run_id,
        "command": cmd,
        "returncode": proc.returncode,
        "stderr": proc.stderr,
    }
    if proc.returncode != 0:
        result["ok"] = False
        result["error"] = proc.stderr.strip() or proc.stdout.strip()
        return result
    payload = read_benchmark_json(proc.stdout, scenario)

    result["ok"] = True
    result["payload"] = payload
    result["metrics"] = {
        key: float(value)
        for key, value in payload["metrics"].items()
        if isinstance(value, int | float)
    }
    return result


def run_matched_once(
    *,
    root: Path,
    scenario: dict[str, Any],
    client: str,
    run_id: str,
    broker_url: str,
    ca_cert: str | None,
    cargo_profile: str,
    timeout: int,
) -> dict[str, Any]:
    cmd = matched_command(
        scenario,
        client=client,
        run_id=run_id,
        broker_url=broker_url,
        ca_cert=ca_cert,
        cargo_profile=cargo_profile,
    )
    try:
        proc = run_process(cmd, cwd=root, timeout=timeout)
    except subprocess.TimeoutExpired as exc:
        return {
            "run_id": run_id,
            "client": client,
            "command": cmd,
            "returncode": None,
            "ok": False,
            "error": f"benchmark timed out after {exc.timeout} seconds",
        }
    result: dict[str, Any] = {
        "run_id": run_id,
        "client": client,
        "command": cmd,
        "returncode": proc.returncode,
        "stdout": proc.stdout,
        "stderr": proc.stderr,
    }
    if proc.returncode != 0:
        result["ok"] = False
        result["error"] = proc.stderr.strip() or proc.stdout.strip()
        return result
    try:
        payload = read_benchmark_json(proc.stdout, scenario)
    except RuntimeError as exc:
        result["ok"] = False
        result["error"] = str(exc)
        return result
    result["payload"] = payload
    result["metrics"] = {
        key: float(value)
        for key, value in payload["metrics"].items()
        if isinstance(value, int | float) and not isinstance(value, bool)
    }
    result["ok"] = bool(payload["quality"]["valid"])
    if not result["ok"]:
        result["error"] = "benchmark quality.valid is false"
    return result


def median(values: list[float]) -> float:
    return statistics.median(values)


def mean(values: list[float]) -> float:
    return sum(values) / len(values)


def sample_stddev(values: list[float]) -> float:
    if len(values) < 2:
        return 0.0
    return statistics.stdev(values)


def mad(values: list[float]) -> float:
    if not values:
        return 0.0
    center = median(values)
    return median([abs(value - center) for value in values])


def percent(numerator: float, denominator: float) -> float:
    if denominator == 0.0:
        return 0.0
    return (numerator / denominator) * 100.0


def percentile(values: list[float], pct: int) -> float:
    sorted_values = sorted(values)
    rank = math.ceil((pct / 100.0) * len(sorted_values))
    return sorted_values[max(rank, 1) - 1]


def metric_summary(values: list[float]) -> dict[str, float | int]:
    if not values:
        return {"count": 0}
    values_median = median(values)
    values_mean = mean(values)
    values_mad = mad(values)
    values_stddev = sample_stddev(values)
    return {
        "count": len(values),
        "min": min(values),
        "max": max(values),
        "mean": values_mean,
        "median": values_median,
        "mad": values_mad,
        "mad_pct": percent(values_mad, values_median),
        "stddev": values_stddev,
        "cv_pct": percent(values_stddev, values_mean),
        "p50": percentile(values, 50),
        "p90": percentile(values, 90),
        "p99": percentile(values, 99),
    }


def summarize_runs(runs: list[dict[str, Any]]) -> dict[str, Any]:
    successful = [run for run in runs if run.get("ok")]
    metric_names = sorted({key for run in successful for key in run.get("metrics", {})})
    return {
        "total_runs": len(runs),
        "successful_runs": len(successful),
        "success_rate": len(successful) / len(runs) if runs else 0.0,
        "metrics": {
            metric: metric_summary(
                [run["metrics"][metric] for run in successful if metric in run["metrics"]]
            )
            for metric in metric_names
        },
    }


def bootstrap_delta(
    baseline: list[float],
    target: list[float],
    *,
    samples: int,
    confidence: float,
    rng: random.Random,
    higher_is_better: bool,
    equivalence_band_pct: float = 0.0,
) -> dict[str, Any]:
    pairs = [
        (baseline_value, target_value)
        for baseline_value, target_value in zip(baseline, target, strict=False)
        if baseline_value != 0.0
    ]
    if not pairs:
        return {"error": "missing paired baseline or target values"}
    base_median = median(baseline)
    target_median = median(target)
    if base_median == 0.0:
        return {"error": "baseline median is zero"}
    paired_deltas = [
        ((target_value - baseline_value) / baseline_value) * 100.0
        for baseline_value, target_value in pairs
    ]

    deltas = []
    for _ in range(samples):
        sampled = [paired_deltas[rng.randrange(len(paired_deltas))] for _ in paired_deltas]
        deltas.append(median(sampled))
    if not deltas:
        return {"error": "no bootstrap samples"}

    deltas.sort()
    alpha = 1.0 - confidence
    lo_idx = max(0, int(math.floor((alpha / 2.0) * (len(deltas) - 1))))
    hi_idx = min(len(deltas) - 1, int(math.ceil((1.0 - (alpha / 2.0)) * (len(deltas) - 1))))
    low = deltas[lo_idx]
    high = deltas[hi_idx]
    point = median(paired_deltas)
    width = high - low
    classification = "inconclusive"
    inconclusive_reason = "ci_crosses_zero"
    if low >= -equivalence_band_pct and high <= equivalence_band_pct:
        classification = "equivalent"
        inconclusive_reason = None
    elif higher_is_better:
        if low > equivalence_band_pct:
            classification = "improvement"
            inconclusive_reason = None
        elif high < -equivalence_band_pct:
            classification = "regression"
            inconclusive_reason = None
    elif high < -equivalence_band_pct:
        classification = "improvement"
        inconclusive_reason = None
    elif low > equivalence_band_pct:
        classification = "regression"
        inconclusive_reason = None
    return {
        "baseline_median": base_median,
        "target_median": target_median,
        "paired_sample_count": len(paired_deltas),
        "relative_delta_pct": point,
        "relative_delta_ci_low_pct": low,
        "relative_delta_ci_high_pct": high,
        "relative_delta_ci_width_pct": width,
        "higher_is_better": higher_is_better,
        "inconclusive_reason": inconclusive_reason,
        "classification": classification,
    }


def metric_higher_is_better(scenario: dict[str, Any], metric: str) -> bool:
    if metric == scenario["primary_metric"]:
        return scenario["higher_is_better"]
    lower_is_better = (
        metric == "elapsed_sec"
        or metric == "failed"
        or metric in LOWER_IS_BETTER_BACKLOG_METRICS
        or metric.startswith("connect_")
        or metric.endswith("_us")
        or (metric.startswith("rss_") and metric.endswith("_bytes"))
        or metric.endswith("_growth_bytes")
        or metric.endswith("_collapse_pct")
        or metric in {"min", "max", "mean", "p50", "p90", "p95", "p99"}
    )
    return not lower_is_better


def compare_summaries(
    baseline_runs: list[dict[str, Any]],
    target_runs: list[dict[str, Any]],
    *,
    scenario: dict[str, Any],
    bootstrap_samples: int,
    confidence: float,
    equivalence_band_pct: float = 0.0,
) -> dict[str, Any]:
    baseline_ok = [run for run in baseline_runs if run.get("ok")]
    target_ok = [run for run in target_runs if run.get("ok")]
    metric_names = sorted(
        {key for run in baseline_ok for key in run.get("metrics", {})}
        | {key for run in target_ok for key in run.get("metrics", {})}
    )
    comparison = {}
    max_ci_width = float(scenario["quality"]["max_relative_ci_width_pct"])
    for metric in metric_names:
        metric_pairs = [
            (baseline_run["metrics"][metric], target_run["metrics"][metric])
            for baseline_run, target_run in zip(baseline_ok, target_ok, strict=False)
            if metric in baseline_run.get("metrics", {}) and metric in target_run.get("metrics", {})
        ]
        fields = bootstrap_delta(
            [baseline_value for baseline_value, _ in metric_pairs],
            [target_value for _, target_value in metric_pairs],
            samples=bootstrap_samples,
            confidence=confidence,
            rng=random.Random(metric),
            higher_is_better=metric_higher_is_better(scenario, metric),
            equivalence_band_pct=equivalence_band_pct,
        )
        if (
            metric == scenario["primary_metric"]
            and "relative_delta_ci_width_pct" in fields
            and fields["relative_delta_ci_width_pct"] > max_ci_width
        ):
            fields["classification"] = "inconclusive"
            fields["inconclusive_reason"] = "ci_width_exceeds_quality_gate"
        comparison[metric] = fields
    return comparison


def gate_result(
    *,
    name: str,
    observed: float | int | None,
    threshold: float | int,
    passes: bool,
    severity: str,
) -> dict[str, Any]:
    return {
        "name": name,
        "observed": observed,
        "threshold": threshold,
        "status": "pass" if passes else severity,
    }


def aggregate_quality(gates: list[dict[str, Any]]) -> dict[str, Any]:
    statuses = [gate["status"] for gate in gates]
    if "fail" in statuses:
        status = "fail"
    elif "warn" in statuses:
        status = "warn"
    else:
        status = "pass"
    return {"status": status, "gates": gates}


def evaluate_run_quality(scenario: dict[str, Any], summary: dict[str, Any]) -> dict[str, Any]:
    quality = scenario["quality"]
    primary = scenario["primary_metric"]
    primary_summary = summary.get("metrics", {}).get(primary, {})
    success_rate = float(summary.get("success_rate", 0.0))
    measured_runs = int(summary.get("total_runs", 0))
    primary_cv = primary_summary.get("cv_pct")
    primary_mad = primary_summary.get("mad_pct")
    gates = [
        gate_result(
            name="min_success_rate",
            observed=success_rate,
            threshold=quality["min_success_rate"],
            passes=success_rate >= quality["min_success_rate"],
            severity="fail",
        ),
        gate_result(
            name="min_measured_runs",
            observed=measured_runs,
            threshold=quality["min_measured_runs"],
            passes=measured_runs >= quality["min_measured_runs"],
            severity="fail",
        ),
        gate_result(
            name="max_primary_cv_pct",
            observed=primary_cv,
            threshold=quality["max_primary_cv_pct"],
            passes=primary_cv is not None and primary_cv <= quality["max_primary_cv_pct"],
            severity="warn",
        ),
        gate_result(
            name="max_primary_mad_pct",
            observed=primary_mad,
            threshold=quality["max_primary_mad_pct"],
            passes=primary_mad is not None and primary_mad <= quality["max_primary_mad_pct"],
            severity="warn",
        ),
    ]
    return aggregate_quality(gates)


def evaluate_compare_quality(
    scenario: dict[str, Any],
    baseline: dict[str, Any],
    target: dict[str, Any],
    comparison: dict[str, Any],
) -> dict[str, Any]:
    quality = scenario["quality"]
    primary = scenario["primary_metric"]
    baseline_primary = baseline.get("metrics", {}).get(primary, {})
    target_primary = target.get("metrics", {}).get(primary, {})
    primary_comparison = comparison.get(primary, {})
    paired_runs = int(primary_comparison.get("paired_sample_count", 0))
    ci_width = primary_comparison.get("relative_delta_ci_width_pct")
    gates = [
        gate_result(
            name="baseline_min_success_rate",
            observed=baseline.get("success_rate", 0.0),
            threshold=quality["min_success_rate"],
            passes=baseline.get("success_rate", 0.0) >= quality["min_success_rate"],
            severity="fail",
        ),
        gate_result(
            name="target_min_success_rate",
            observed=target.get("success_rate", 0.0),
            threshold=quality["min_success_rate"],
            passes=target.get("success_rate", 0.0) >= quality["min_success_rate"],
            severity="fail",
        ),
        gate_result(
            name="min_paired_runs",
            observed=paired_runs,
            threshold=quality["min_measured_runs"],
            passes=paired_runs >= quality["min_measured_runs"],
            severity="fail",
        ),
        gate_result(
            name="baseline_max_primary_cv_pct",
            observed=baseline_primary.get("cv_pct"),
            threshold=quality["max_primary_cv_pct"],
            passes=baseline_primary.get("cv_pct") is not None
            and baseline_primary["cv_pct"] <= quality["max_primary_cv_pct"],
            severity="warn",
        ),
        gate_result(
            name="target_max_primary_cv_pct",
            observed=target_primary.get("cv_pct"),
            threshold=quality["max_primary_cv_pct"],
            passes=target_primary.get("cv_pct") is not None
            and target_primary["cv_pct"] <= quality["max_primary_cv_pct"],
            severity="warn",
        ),
        gate_result(
            name="baseline_max_primary_mad_pct",
            observed=baseline_primary.get("mad_pct"),
            threshold=quality["max_primary_mad_pct"],
            passes=baseline_primary.get("mad_pct") is not None
            and baseline_primary["mad_pct"] <= quality["max_primary_mad_pct"],
            severity="warn",
        ),
        gate_result(
            name="target_max_primary_mad_pct",
            observed=target_primary.get("mad_pct"),
            threshold=quality["max_primary_mad_pct"],
            passes=target_primary.get("mad_pct") is not None
            and target_primary["mad_pct"] <= quality["max_primary_mad_pct"],
            severity="warn",
        ),
        gate_result(
            name="max_relative_ci_width_pct",
            observed=ci_width,
            threshold=quality["max_relative_ci_width_pct"],
            passes=ci_width is not None and ci_width <= quality["max_relative_ci_width_pct"],
            severity="warn",
        ),
    ]
    return aggregate_quality(gates)


def first_payload_environment(runs: list[dict[str, Any]]) -> dict[str, Any] | None:
    for run in runs:
        payload = run.get("payload")
        if isinstance(payload, dict) and isinstance(payload.get("environment"), dict):
            return payload["environment"]
    return None


def summary_environment(root: Path, runs: list[dict[str, Any]]) -> dict[str, Any]:
    environment = fallback_environment(root)
    payload_environment = first_payload_environment(runs)
    if payload_environment is not None:
        environment.update(
            {
                key: value
                for key, value in payload_environment.items()
                if value is not None
            }
        )
    return environment


def command_template(
    scenario: dict[str, Any],
    *,
    broker_url: str | None,
    ca_cert: str | None,
    cargo_profile: str,
) -> list[str]:
    return scenario_command(
        scenario,
        run_id="<run-id>",
        broker_url=broker_url,
        ca_cert=ca_cert,
        cargo_profile=cargo_profile,
    )


def write_raw_run(raw_dir: Path, run: dict[str, Any], index: int) -> str:
    raw_dir.mkdir(parents=True, exist_ok=True)
    path = raw_dir / f"run_{index:03d}.json"
    path.write_text(json.dumps(run, indent=2, sort_keys=True), encoding="utf-8")
    return str(path.relative_to(raw_dir.parents[1]))


def strip_raw_payload(run: dict[str, Any]) -> dict[str, Any]:
    return {
        key: value
        for key, value in run.items()
        if key not in {"payload", "stdout"} and (key != "stderr" or value)
    }


def persist_raw_runs(output_dir: Path, summary: dict[str, Any]) -> None:
    if summary.get("mode") in {"compare", "compare-external", "compare-libraries"}:
        runs_by_side = summary.get("runs", {})
        if isinstance(runs_by_side, dict):
            for side, runs in runs_by_side.items():
                if not isinstance(runs, list):
                    continue
                raw_dir = output_dir / "raw" / side
                for index, run in enumerate(runs, start=1):
                    run["raw_path"] = write_raw_run(raw_dir, run, index)
            summary["runs"] = {
                side: [strip_raw_payload(run) for run in runs]
                for side, runs in runs_by_side.items()
                if isinstance(runs, list)
            }
        return

    runs = summary.get("runs")
    if isinstance(runs, list):
        raw_dir = output_dir / "raw" / "current"
        for index, run in enumerate(runs, start=1):
            run["raw_path"] = write_raw_run(raw_dir, run, index)
        summary["runs"] = [strip_raw_payload(run) for run in runs]


def write_report(output_dir: Path, summary: dict[str, Any]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    persist_raw_runs(output_dir, summary)
    (output_dir / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8"
    )

    with (output_dir / "summary.csv").open("w", newline="", encoding="utf-8") as handle:
        writer = csv.writer(handle)
        writer.writerow(["section", "metric", "field", "value"])
        metadata = summary.get("scenario_metadata", {})
        if isinstance(metadata, dict):
            for field, value in metadata.items():
                writer.writerow(["metadata", "", field, value])
        for section in ("summary", "baseline", "target"):
            data = summary.get(section)
            if not isinstance(data, dict):
                continue
            metrics = data.get("metrics") if section != "summary" else data
            if not isinstance(metrics, dict):
                continue
            for metric, fields in metrics.items():
                if isinstance(fields, dict):
                    for field, value in fields.items():
                        writer.writerow([section, metric, field, value])
        for metric, fields in summary.get("comparison", {}).items():
            for field, value in fields.items():
                writer.writerow(["comparison", metric, field, value])
        quality = summary.get("quality", {})
        if isinstance(quality, dict):
            writer.writerow(["quality", "", "status", quality.get("status")])
            for gate in quality.get("gates", []):
                if isinstance(gate, dict):
                    gate_name = str(gate.get("name", ""))
                    for field, value in gate.items():
                        if field != "name":
                            writer.writerow(["quality", gate_name, field, value])

    html_rows = []
    primary_metric = ""
    metadata = summary.get("scenario_metadata", {})
    if isinstance(metadata, dict):
        primary_metric = str(metadata.get("primary_metric", ""))
    for metric, fields in summary.get("comparison", {}).items():
        direction = "higher" if fields.get("higher_is_better") else "lower"
        primary = "yes" if metric == primary_metric else ""
        html_rows.append(
            "<tr>"
            f"<td>{html.escape(metric)}</td>"
            f"<td>{primary}</td>"
            f"<td>{direction}</td>"
            f"<td>{fields.get('baseline_median', '-')}</td>"
            f"<td>{fields.get('target_median', '-')}</td>"
            f"<td>{fields.get('relative_delta_pct', '-')}</td>"
            f"<td>{fields.get('relative_delta_ci_width_pct', '-')}</td>"
            f"<td>{fields.get('paired_sample_count', '-')}</td>"
            f"<td>{html.escape(str(fields.get('classification', '-')))}</td>"
            "</tr>"
        )
    html_body = "\n".join(html_rows) or "<tr><td colspan='9'>No comparison data</td></tr>"
    description = ""
    if isinstance(metadata, dict):
        description = str(metadata.get("description", ""))
    quality = summary.get("quality", {})
    quality_status = quality.get("status", "-") if isinstance(quality, dict) else "-"
    quality_rows = []
    if isinstance(quality, dict):
        for gate in quality.get("gates", []):
            if isinstance(gate, dict):
                quality_rows.append(
                    "<tr>"
                    f"<td>{html.escape(str(gate.get('name', '-')))}</td>"
                    f"<td>{gate.get('observed', '-')}</td>"
                    f"<td>{gate.get('threshold', '-')}</td>"
                    f"<td>{html.escape(str(gate.get('status', '-')))}</td>"
                    "</tr>"
                )
    quality_body = "\n".join(quality_rows) or "<tr><td colspan='4'>No quality gates</td></tr>"
    summary_rows = []
    for section in ("summary", "baseline", "target"):
        section_summary = summary.get(section)
        if not isinstance(section_summary, dict):
            continue
        metrics = section_summary.get("metrics")
        if not isinstance(metrics, dict):
            continue
        success_rate = section_summary.get("success_rate", "-")
        for metric, fields in metrics.items():
            if isinstance(fields, dict):
                primary = "yes" if metric == primary_metric else ""
                summary_rows.append(
                    "<tr>"
                    f"<td>{section}</td>"
                    f"<td>{html.escape(metric)}</td>"
                    f"<td>{primary}</td>"
                    f"<td>{fields.get('count', '-')}</td>"
                    f"<td>{success_rate}</td>"
                    f"<td>{fields.get('median', '-')}</td>"
                    f"<td>{fields.get('mad_pct', '-')}</td>"
                    f"<td>{fields.get('cv_pct', '-')}</td>"
                    "</tr>"
                )
    summary_body = "\n".join(summary_rows) or "<tr><td colspan='8'>No summary data</td></tr>"
    (output_dir / "report.html").write_text(
        f"""<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <title>rumqtt benchmark report</title>
  <style>
    body {{ font-family: system-ui, sans-serif; margin: 24px; }}
    table {{ border-collapse: collapse; width: 100%; }}
    th, td {{ border: 1px solid #ddd; padding: 6px 8px; text-align: left; }}
    th {{ background: #f4f4f4; }}
  </style>
</head>
<body>
  <h1>{html.escape(summary.get("scenario", "rumqtt benchmark"))}</h1>
  <p>{html.escape(description)}</p>
  <h2>Quality: {html.escape(str(quality_status))}</h2>
  <table>
    <thead>
      <tr><th>Gate</th><th>Observed</th><th>Threshold</th><th>Status</th></tr>
    </thead>
    <tbody>{quality_body}</tbody>
  </table>
  <h2>Run summaries</h2>
  <table>
    <thead>
      <tr>
        <th>Section</th><th>Metric</th><th>Primary</th><th>Samples</th>
        <th>Success rate</th><th>Median</th><th>MAD %</th><th>CV %</th>
      </tr>
    </thead>
    <tbody>{summary_body}</tbody>
  </table>
  <h2>Comparison</h2>
  <table>
    <thead>
      <tr>
        <th>Metric</th><th>Primary</th><th>Better</th><th>Baseline median</th>
        <th>Target median</th><th>Delta %</th><th>CI width %</th><th>Pairs</th><th>Class</th>
      </tr>
    </thead>
    <tbody>{html_body}</tbody>
  </table>
</body>
</html>
""",
        encoding="utf-8",
    )


def timestamp() -> str:
    return dt.datetime.now(dt.UTC).strftime("%Y%m%d-%H%M%SZ")


def default_output_dir(root: Path, kind: str, scenario: dict[str, Any]) -> Path:
    benchmark_root = (
        root / "session-store-file" / "benchmarks"
        if scenario["group"] == "persistence"
        else root / "benchmarks"
    )
    return benchmark_root / "results" / kind / timestamp()


def command_run(args: argparse.Namespace) -> None:
    root = repo_root()
    scenario_path, scenario = load_scenario(root, args.scenario)
    validate_broker_requirement(scenario, args.broker_url)
    output_dir = (
        Path(args.output_dir).resolve()
        if args.output_dir
        else default_output_dir(root, "runs", scenario)
    )
    runs = []
    total = args.warmup_runs + args.runs
    for index in range(total):
        run_id = f"{scenario['name']}-{timestamp()}-{index}"
        run = run_once(
            root=root,
            scenario=scenario,
            run_id=run_id,
            broker_url=args.broker_url,
            ca_cert=args.ca_cert,
            cargo_profile=args.cargo_profile,
            timeout=args.timeout_sec,
        )
        run["is_warmup"] = index < args.warmup_runs
        run["run_index"] = index
        run["git_ref"] = resolve_ref(root, "HEAD")
        runs.append(run)
    measured = [run for run in runs if not run["is_warmup"]]
    measured_summary = summarize_runs(measured)
    summary = {
        "scenario": scenario["name"],
        "scenario_metadata": scenario_metadata(scenario),
        "scenario_file": {
            "path": str(scenario_path),
            "sha256": scenario_file_hash(scenario_path),
        },
        "mode": "run",
        "git": {
            "current_ref": current_ref(root),
            "current_commit": resolve_ref(root, "HEAD"),
        },
        "command": command_template(
            scenario,
            broker_url=args.broker_url,
            ca_cert=args.ca_cert,
            cargo_profile=args.cargo_profile,
        ),
        "cargo_profile": args.cargo_profile,
        "environment": summary_environment(root, runs),
        "runs": runs,
        "summary": measured_summary,
        "quality": evaluate_run_quality(scenario, measured_summary),
    }
    write_report(output_dir, summary)
    failed = [run for run in runs if not run.get("ok")]
    if failed:
        raise RuntimeError(f"{len(failed)} benchmark run(s) failed; report written to {output_dir}")
    print(f"Benchmark run complete: {output_dir}")


def add_worktree(root: Path, temp_root: Path, label: str, ref: str) -> Path:
    path = temp_root / label
    run_process(["git", "worktree", "add", "--detach", str(path), ref], cwd=root, check=True)
    return path


def remove_worktree(root: Path, path: Path) -> None:
    run_process(["git", "worktree", "remove", "--force", str(path)], cwd=root)


def command_compare(args: argparse.Namespace) -> None:
    root = repo_root()
    scenario_path, scenario = load_scenario(root, args.scenario)
    validate_broker_requirement(scenario, args.broker_url)
    baseline_ref = resolve_ref(root, args.baseline_ref or current_ref(root))
    target_ref = resolve_ref(root, args.target_ref)
    output_dir = (
        Path(args.output_dir).resolve()
        if args.output_dir
        else default_output_dir(root, "comparisons", scenario)
    )
    temp_root = Path(tempfile.mkdtemp(prefix="rumqtt-bench-compare-"))
    worktrees: dict[str, Path] = {}
    try:
        worktrees["baseline"] = add_worktree(root, temp_root, "baseline", baseline_ref)
        worktrees["target"] = add_worktree(root, temp_root, "target", target_ref)
        runs = {"baseline": [], "target": []}
        total = args.warmup_runs + args.runs
        for index in range(total):
            order = ["baseline", "target"]
            if args.alternate_order and index % 2 == 1:
                order.reverse()
            for side in order:
                run_id = f"{scenario['name']}-{side}-{index}"
                run = run_once(
                    root=worktrees[side],
                    scenario=scenario,
                    run_id=run_id,
                    broker_url=args.broker_url,
                    ca_cert=args.ca_cert,
                    cargo_profile=args.cargo_profile,
                    timeout=args.timeout_sec,
                )
                run["is_warmup"] = index < args.warmup_runs
                run["run_index"] = index
                run["side"] = side
                run["git_ref"] = baseline_ref if side == "baseline" else target_ref
                runs[side].append(run)

        baseline_measured = [run for run in runs["baseline"] if not run["is_warmup"]]
        target_measured = [run for run in runs["target"] if not run["is_warmup"]]
        baseline_summary = summarize_runs(baseline_measured)
        target_summary = summarize_runs(target_measured)
        comparison = compare_summaries(
            baseline_measured,
            target_measured,
            scenario=scenario,
            bootstrap_samples=args.bootstrap_samples,
            confidence=args.confidence,
        )
        summary = {
            "scenario": scenario["name"],
            "scenario_metadata": scenario_metadata(scenario),
            "scenario_file": {
                "path": str(scenario_path),
                "sha256": scenario_file_hash(scenario_path),
            },
            "mode": "compare",
            "git": {
                "baseline_ref": baseline_ref,
                "target_ref": target_ref,
            },
            "baseline_ref": baseline_ref,
            "target_ref": target_ref,
            "command": command_template(
                scenario,
                broker_url=args.broker_url,
                ca_cert=args.ca_cert,
                cargo_profile=args.cargo_profile,
            ),
            "cargo_profile": args.cargo_profile,
            "environment": {
                "baseline": summary_environment(worktrees["baseline"], runs["baseline"]),
                "target": summary_environment(worktrees["target"], runs["target"]),
            },
            "baseline": baseline_summary,
            "target": target_summary,
            "comparison": comparison,
            "quality": evaluate_compare_quality(scenario, baseline_summary, target_summary, comparison),
            "runs": runs,
        }
        write_report(output_dir, summary)
        failed = [run for side_runs in runs.values() for run in side_runs if not run.get("ok")]
        if failed:
            raise RuntimeError(f"{len(failed)} benchmark run(s) failed; report written to {output_dir}")
        print(f"Benchmark comparison complete: {output_dir}")
    finally:
        if not args.keep_worktrees:
            for path in worktrees.values():
                remove_worktree(root, path)


def command_compare_external(args: argparse.Namespace) -> None:
    root = repo_root()
    scenario_path, original_scenario = load_scenario(root, args.scenario)
    validate_external_scenario(original_scenario)
    validate_broker_requirement(original_scenario, args.broker_url)
    scenario = external_comparison_scenario(original_scenario)
    external_bin = resolve_external_binary(args.external_bin)
    version = external_version(external_bin)
    output_dir = (
        Path(args.output_dir).resolve()
        if args.output_dir
        else default_output_dir(root, "external-comparisons", scenario)
    )
    runs = {"baseline": [], "target": []}
    total = args.warmup_runs + args.runs
    for index in range(total):
        order = ["baseline", "target"]
        if args.alternate_order and index % 2 == 1:
            order.reverse()
        for side in order:
            run_id = f"{scenario['name']}-{side}-{index}"
            if side == "baseline":
                run = run_once(
                    root=root,
                    scenario=scenario,
                    run_id=run_id,
                    broker_url=args.broker_url,
                    ca_cert=args.ca_cert,
                    cargo_profile=args.cargo_profile,
                    timeout=args.timeout_sec,
                )
            else:
                run = run_external_once(
                    root=root,
                    scenario=scenario,
                    external_bin=external_bin,
                    version=version,
                    run_id=run_id,
                    broker_url=args.broker_url,
                    ca_cert=args.ca_cert,
                    timeout=args.timeout_sec,
                )
            run["is_warmup"] = index < args.warmup_runs
            run["run_index"] = index
            run["side"] = side
            runs[side].append(run)

    baseline_measured = [run for run in runs["baseline"] if not run["is_warmup"]]
    target_measured = [run for run in runs["target"] if not run["is_warmup"]]
    baseline_summary = summarize_runs(baseline_measured)
    target_summary = summarize_runs(target_measured)
    comparison = compare_summaries(
        baseline_measured,
        target_measured,
        scenario=scenario,
        bootstrap_samples=args.bootstrap_samples,
        confidence=args.confidence,
    )
    current_commit = resolve_ref(root, "HEAD")
    summary = {
        "scenario": scenario["name"],
        "scenario_metadata": scenario_metadata(scenario),
        "scenario_file": {
            "path": str(scenario_path),
            "sha256": scenario_file_hash(scenario_path),
        },
        "mode": "compare-external",
        "baseline_ref": current_commit,
        "target_ref": version,
        "git": {"baseline_ref": current_commit},
        "external": {"binary": external_bin, "version": version},
        "command": {
            "baseline": command_template(
                scenario,
                broker_url=args.broker_url,
                ca_cert=args.ca_cert,
                cargo_profile=args.cargo_profile,
            ),
            "target": external_command(
                scenario,
                external_bin=external_bin,
                broker_url=args.broker_url,
                ca_cert=args.ca_cert,
                run_id="<run-id>",
            ),
        },
        "cargo_profile": args.cargo_profile,
        "environment": {
            "baseline": summary_environment(root, runs["baseline"]),
            "target": first_payload_environment(runs["target"]) or fallback_environment(root),
        },
        "baseline": baseline_summary,
        "target": target_summary,
        "comparison": comparison,
        "quality": evaluate_compare_quality(
            scenario, baseline_summary, target_summary, comparison
        ),
        "runs": runs,
    }
    write_report(output_dir, summary)
    failed = [run for side_runs in runs.values() for run in side_runs if not run.get("ok")]
    if failed:
        raise RuntimeError(f"{len(failed)} benchmark run(s) failed; report written to {output_dir}")
    print(f"External benchmark comparison complete: {output_dir}")


def command_compare_libraries(args: argparse.Namespace) -> None:
    root = repo_root()
    scenario_path, scenario = load_scenario(root, args.scenario)
    if scenario["group"] != "matched":
        raise RuntimeError("compare-libraries requires a scenario with group = 'matched'")
    validate_broker_requirement(scenario, args.broker_url)
    ca_certificate = certificate_metadata(args.ca_cert)
    if ca_certificate is not None and scenario.get("transport") != "tls":
        raise RuntimeError("--ca-cert is only valid for matched TLS scenarios")
    ca_cert = ca_certificate["path"] if ca_certificate is not None else None
    output_dir = (
        Path(args.output_dir).resolve()
        if args.output_dir
        else default_output_dir(root, "library-comparisons", scenario)
    )
    runs: dict[str, list[dict[str, Any]]] = {"rumqttc": [], "mqtt5": []}
    total = args.warmup_runs + args.runs
    for index in range(total):
        order = ["rumqttc", "mqtt5"]
        if args.alternate_order and index % 2 == 1:
            order.reverse()
        for client in order:
            run = run_matched_once(
                root=root,
                scenario=scenario,
                client=client,
                run_id=f"{scenario['name']}-{client}-{index}",
                broker_url=args.broker_url,
                ca_cert=ca_cert,
                cargo_profile=args.cargo_profile,
                timeout=args.timeout_sec,
            )
            run["is_warmup"] = index < args.warmup_runs
            run["run_index"] = index
            runs[client].append(run)

    measured = {
        client: [run for run in client_runs if not run["is_warmup"]]
        for client, client_runs in runs.items()
    }
    by_index = {
        client: {run["run_index"]: run for run in client_runs}
        for client, client_runs in measured.items()
    }
    paired_indices = sorted(set(by_index["rumqttc"]) & set(by_index["mqtt5"]))
    paired_rumqttc = []
    paired_mqtt5 = []
    for index in paired_indices:
        rumqttc_run = by_index["rumqttc"][index]
        mqtt5_run = by_index["mqtt5"][index]
        if rumqttc_run.get("ok") and mqtt5_run.get("ok"):
            paired_rumqttc.append(rumqttc_run)
            paired_mqtt5.append(mqtt5_run)

    baseline_summary = summarize_runs(measured["rumqttc"])
    target_summary = summarize_runs(measured["mqtt5"])
    comparison = compare_summaries(
        paired_rumqttc,
        paired_mqtt5,
        scenario=scenario,
        bootstrap_samples=args.bootstrap_samples,
        confidence=args.confidence,
        equivalence_band_pct=args.equivalence_band_pct,
    )
    quality = evaluate_compare_quality(
        scenario, baseline_summary, target_summary, comparison
    )
    if quality["status"] != "pass":
        primary = comparison.get(scenario["primary_metric"])
        if isinstance(primary, dict) and "classification" in primary:
            primary["classification"] = "inconclusive"
            primary["inconclusive_reason"] = "quality_gates_failed"
    provenance = collect_matched_provenance(
        root,
        cargo_profile=args.cargo_profile,
        cargo_features=list(scenario.get("cargo_features", [])),
    )
    resolved_rumqttc = provenance.get("rumqttc-v5-next") or {}
    resolved_mqtt5 = provenance.get("mqtt5") or {}
    mqtt5_version = resolved_mqtt5.get("version") or "unavailable"
    summary = {
        "scenario": scenario["name"],
        "scenario_metadata": scenario_metadata(scenario),
        "scenario_file": {
            "path": str(scenario_path),
            "sha256": scenario_file_hash(scenario_path),
        },
        "mode": "compare-libraries",
        "baseline_ref": "rumqttc-v5-next",
        "target_ref": f"mqtt5={mqtt5_version}",
        "git": {"commit": resolve_ref(root, "HEAD")},
        "libraries": {
            "baseline": {
                "name": "rumqttc-v5-next",
                "version": resolved_rumqttc.get("version"),
                "source": resolved_rumqttc.get("source") or "workspace",
                "commit": provenance.get("workspace_commit"),
            },
            "target": {
                "name": "mqtt5",
                "version": resolved_mqtt5.get("version"),
                "source": resolved_mqtt5.get("source"),
            },
        },
        "command": {
            client: matched_command(
                scenario,
                client=client,
                run_id="<run-id>",
                broker_url=args.broker_url,
                ca_cert=ca_cert,
                cargo_profile=args.cargo_profile,
            )
            for client in ("rumqttc", "mqtt5")
        },
        "cargo_profile": args.cargo_profile,
        "provenance": provenance,
        "ca_certificate": ca_certificate,
        "equivalence_band_pct": args.equivalence_band_pct,
        "environment": {
            "baseline": summary_environment(root, runs["rumqttc"]),
            "target": summary_environment(root, runs["mqtt5"]),
        },
        "baseline": baseline_summary,
        "target": target_summary,
        "comparison": comparison,
        "quality": quality,
        "valid_paired_runs": len(paired_rumqttc),
        "runs": runs,
    }
    write_report(output_dir, summary)
    failed = [run for client_runs in measured.values() for run in client_runs if not run.get("ok")]
    if failed:
        raise RuntimeError(f"{len(failed)} measured run(s) invalid; report written to {output_dir}")
    print(f"Matched library comparison complete: {output_dir}")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    run = sub.add_parser("run", help="Run one scenario repeatedly in the current worktree")
    run.add_argument("--scenario", required=True)
    run.add_argument("--runs", type=int, default=5)
    run.add_argument("--warmup-runs", type=int, default=1)
    run.add_argument("--broker-url")
    run.add_argument("--ca-cert")
    run.add_argument("--cargo-profile", choices=sorted(VALID_CARGO_PROFILES), default="release")
    run.add_argument("--timeout-sec", type=int, default=300)
    run.add_argument("--output-dir")
    run.set_defaults(func=command_run)

    compare = sub.add_parser("compare", help="Compare one scenario across two git refs")
    compare.add_argument("--scenario", required=True)
    compare.add_argument("--baseline-ref")
    compare.add_argument("--target-ref", required=True)
    compare.add_argument("--runs", type=int, default=12)
    compare.add_argument("--warmup-runs", type=int, default=1)
    compare.add_argument("--broker-url")
    compare.add_argument("--ca-cert")
    compare.add_argument("--cargo-profile", choices=sorted(VALID_CARGO_PROFILES), default="release")
    compare.add_argument("--timeout-sec", type=int, default=300)
    compare.add_argument("--bootstrap-samples", type=int, default=1000)
    compare.add_argument("--confidence", type=float, default=0.95)
    compare.add_argument("--alternate-order", action=argparse.BooleanOptionalAction, default=True)
    compare.add_argument("--keep-worktrees", action="store_true")
    compare.add_argument("--output-dir")
    compare.set_defaults(func=command_compare)

    external = sub.add_parser(
        "compare-external", help="Compare a rumqtt MQTT v5 scenario against mqttv5-cli"
    )
    external.add_argument("--scenario", required=True)
    external.add_argument("--external-bin", default="mqttv5")
    external.add_argument("--runs", type=int, default=12)
    external.add_argument("--warmup-runs", type=int, default=1)
    external.add_argument("--broker-url", required=True)
    external.add_argument("--ca-cert")
    external.add_argument(
        "--cargo-profile", choices=sorted(VALID_CARGO_PROFILES), default="release"
    )
    external.add_argument("--timeout-sec", type=int, default=300)
    external.add_argument("--bootstrap-samples", type=int, default=1000)
    external.add_argument("--confidence", type=float, default=0.95)
    external.add_argument(
        "--alternate-order", action=argparse.BooleanOptionalAction, default=True
    )
    external.add_argument("--output-dir")
    external.set_defaults(func=command_compare_external)

    libraries = sub.add_parser(
        "compare-libraries", help="Compare workspace rumqttc-v5-next with mqtt5=0.38.0"
    )
    libraries.add_argument("--scenario", required=True)
    libraries.add_argument("--runs", type=int, default=12)
    libraries.add_argument("--warmup-runs", type=int, default=1)
    libraries.add_argument("--broker-url", required=True)
    libraries.add_argument("--ca-cert")
    libraries.add_argument(
        "--cargo-profile", choices=sorted(VALID_CARGO_PROFILES), default="release"
    )
    libraries.add_argument("--timeout-sec", type=int, default=300)
    libraries.add_argument("--bootstrap-samples", type=int, default=1000)
    libraries.add_argument("--confidence", type=float, default=0.95)
    libraries.add_argument("--equivalence-band-pct", type=float, default=5.0)
    libraries.add_argument(
        "--alternate-order", action=argparse.BooleanOptionalAction, default=True
    )
    libraries.add_argument("--output-dir")
    libraries.set_defaults(func=command_compare_libraries)
    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    if args.runs <= 0:
        parser.error("--runs must be greater than zero")
    if args.warmup_runs < 0:
        parser.error("--warmup-runs must be non-negative")
    if args.timeout_sec <= 0:
        parser.error("--timeout-sec must be greater than zero")
    if hasattr(args, "confidence") and not 0.0 < args.confidence < 1.0:
        parser.error("--confidence must be between 0 and 1")
    if hasattr(args, "equivalence_band_pct") and args.equivalence_band_pct < 0.0:
        parser.error("--equivalence-band-pct must be non-negative")
    try:
        args.func(args)
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
