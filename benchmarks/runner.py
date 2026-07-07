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
import statistics
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path
from typing import Any


OUTPUT_SCHEMA_VERSION = 1
VALID_CARGO_PROFILES = {"dev", "release"}
VALID_TRANSPORTS = {"tcp", "tls", "websocket"}
VALID_CARGO_FEATURES = {"url", "websocket"}
QUALITY_FIELDS = {
    "min_success_rate",
    "min_measured_runs",
    "max_primary_cv_pct",
    "max_primary_mad_pct",
    "max_relative_ci_width_pct",
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
    }


def load_scenario(root: Path, scenario: str) -> tuple[Path, dict[str, Any]]:
    path = Path(scenario)
    if not path.suffix:
        path = root / "benchmarks" / "scenarios" / f"{scenario}.toml"
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
    if scenario["group"] not in {"client", "codec", "options"}:
        raise RuntimeError(f"{path}: group must be 'client', 'codec', or 'options'")
    commands = {
        "client": {"throughput", "latency", "connections"},
        "codec": {"encode", "decode", "roundtrip"},
        "options": {"parse-url"},
    }
    if scenario["command"] not in commands[scenario["group"]]:
        raise RuntimeError(f"{path}: unsupported command for group '{scenario['group']}'")
    if "args" in scenario and not isinstance(scenario["args"], dict):
        raise RuntimeError(f"{path}: args must be a table")
    validate_transport(path, scenario)
    validate_cargo_features(path, scenario.get("cargo_features"))
    expected_requires_broker = scenario["group"] == "client"
    if scenario["requires_broker"] != expected_requires_broker:
        expected = "true" if expected_requires_broker else "false"
        raise RuntimeError(f"{path}: requires_broker must be {expected} for {scenario['group']} scenarios")
    validate_quality(path, scenario.get("quality"))


def validate_transport(path: Path, scenario: dict[str, Any]) -> None:
    transport = scenario.get("transport")
    if transport is None:
        return
    if scenario["group"] != "client":
        raise RuntimeError(f"{path}: transport is only supported for client scenarios")
    if not isinstance(transport, str) or transport not in VALID_TRANSPORTS:
        allowed = ", ".join(sorted(VALID_TRANSPORTS))
        raise RuntimeError(f"{path}: transport must be one of: {allowed}")


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
    if cargo_profile == "release":
        cmd.append("--release")
    cargo_features = sorted(set(scenario.get("cargo_features", [])))
    if cargo_features:
        cmd.extend(["--features", ",".join(cargo_features)])
    cmd.extend([
        "-p",
        "benchmarks",
        "--bin",
        "rumqtt-bench",
        "--",
        scenario["group"],
        scenario["command"],
    ])
    args = dict(scenario.get("args", {}))
    args["run-id"] = run_id
    if broker_url is not None and scenario["group"] == "client":
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


def numeric_metric(metrics: dict[str, Any], metric: str) -> float:
    value = metrics.get(metric)
    if isinstance(value, bool) or not isinstance(value, int | float) or not math.isfinite(value):
        raise RuntimeError(f"benchmark metric '{metric}' must be a finite number")
    return float(value)


def validate_benchmark_payload(data: dict[str, Any], scenario: dict[str, Any]) -> None:
    if data.get("schema_version") != OUTPUT_SCHEMA_VERSION:
        raise RuntimeError(
            f"benchmark JSON schema_version must be {OUTPUT_SCHEMA_VERSION}, got {data.get('schema_version')!r}"
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
    if higher_is_better:
        if low > 0:
            classification = "improvement"
            inconclusive_reason = None
        elif high < 0:
            classification = "regression"
            inconclusive_reason = None
    elif high < 0:
        classification = "improvement"
        inconclusive_reason = None
    elif low > 0:
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
                "rustc": payload_environment.get("rustc"),
                "os": payload_environment.get("os"),
                "arch": payload_environment.get("arch"),
                "cpu_count": payload_environment.get("cpu_count"),
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
        if key not in {"payload", "stderr"} or key == "stderr" and value
    }


def persist_raw_runs(output_dir: Path, summary: dict[str, Any]) -> None:
    if summary.get("mode") == "compare":
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


def default_output_dir(root: Path, kind: str) -> Path:
    return root / "benchmarks" / "results" / kind / timestamp()


def command_run(args: argparse.Namespace) -> None:
    root = repo_root()
    scenario_path, scenario = load_scenario(root, args.scenario)
    validate_broker_requirement(scenario, args.broker_url)
    output_dir = Path(args.output_dir).resolve() if args.output_dir else default_output_dir(root, "runs")
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
        Path(args.output_dir).resolve() if args.output_dir else default_output_dir(root, "comparisons")
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
    try:
        args.func(args)
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
