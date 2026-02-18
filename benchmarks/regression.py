#!/usr/bin/env python3
"""Run and compare benchmark regressions across two git branches.

Default use:
  python3 benchmarks/regression.py --target-branch <branch>
"""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import html
import json
import math
import random
import shlex
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any


PERCENTILES = (50, 90, 99)
MODE_REQUIRED_KEYS: dict[str, tuple[str, ...]] = {
    "throughput": ("published", "received", "throughput_avg", "elapsed_secs"),
    "latency": ("messages", "avg_us", "p50_us", "p95_us", "p99_us"),
    "connections": ("connections_per_sec", "successful", "failed", "elapsed_secs"),
}
PRIMARY_METRIC_BY_MODE = {
    "throughput": "throughput_avg",
    "latency": "p95_us",
    "connections": "connections_per_sec",
}


def run_process(
    cmd: list[str],
    *,
    cwd: Path | None = None,
    check: bool = False,
    capture: bool = True,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=str(cwd) if cwd else None,
        check=check,
        capture_output=capture,
        text=True,
    )


def get_repo_root() -> Path:
    proc = run_process(["git", "rev-parse", "--show-toplevel"], check=True)
    return Path(proc.stdout.strip())


def get_current_branch(repo_root: Path) -> str:
    proc = run_process(["git", "rev-parse", "--abbrev-ref", "HEAD"], cwd=repo_root, check=True)
    branch = proc.stdout.strip()
    if not branch:
        raise RuntimeError("Unable to determine current branch")
    return branch


def resolve_ref(repo_root: Path, ref: str) -> str:
    proc = run_process(["git", "rev-parse", "--verify", f"{ref}^{{commit}}"], cwd=repo_root)
    if proc.returncode != 0:
        raise RuntimeError(f"Reference '{ref}' does not exist or cannot be resolved")
    return proc.stdout.strip()


def get_commit(repo_root: Path) -> str:
    proc = run_process(["git", "rev-parse", "HEAD"], cwd=repo_root)
    if proc.returncode != 0:
        return "unknown"
    return proc.stdout.strip() or "unknown"


def extract_json_objects(raw: str) -> list[dict[str, Any]]:
    decoder = json.JSONDecoder()
    idx = 0
    length = len(raw)
    out: list[dict[str, Any]] = []
    while idx < length:
        brace = raw.find("{", idx)
        if brace == -1:
            break
        try:
            obj, end = decoder.raw_decode(raw, brace)
        except json.JSONDecodeError:
            idx = brace + 1
            continue
        if isinstance(obj, dict):
            out.append(obj)
        idx = end
    return out


def extract_metrics(obj: dict[str, Any]) -> dict[str, float]:
    metrics: dict[str, float] = {}

    def collect(prefix: str, value: Any) -> None:
        if isinstance(value, bool):
            return
        if isinstance(value, (int, float)):
            metrics[prefix] = float(value)
            return
        if isinstance(value, dict):
            for k, v in value.items():
                key = f"{prefix}.{k}" if prefix else k
                collect(key, v)

    results = obj.get("results")
    if isinstance(results, dict):
        collect("", results)

    for key, value in obj.items():
        if "throughput" in key and isinstance(value, (int, float)):
            metrics[key] = float(value)
    return metrics


def choose_metric_object(
    objs: list[dict[str, Any]],
    *,
    expected_mode: str | None = None,
    strict_schema: bool = False,
) -> tuple[dict[str, Any] | None, dict[str, float], str | None]:
    candidates: list[tuple[dict[str, Any], dict[str, float], str]] = []
    fallback: list[tuple[dict[str, Any], dict[str, float]]] = []

    for obj in objs:
        metrics = extract_metrics(obj)
        if not metrics:
            continue
        mode = obj.get("mode")
        library = obj.get("library")
        results = obj.get("results")
        if isinstance(mode, str) and isinstance(library, str) and isinstance(results, dict):
            candidates.append((obj, metrics, mode))
        else:
            fallback.append((obj, metrics))

    if strict_schema:
        if not candidates:
            return None, {}, "no structured benchmark JSON payload found"
        filtered = [c for c in candidates if expected_mode is None or c[2] == expected_mode]
        if len(filtered) != 1:
            return None, {}, f"expected exactly one benchmark payload for mode '{expected_mode}', found {len(filtered)}"
        return filtered[0][0], filtered[0][1], None

    if candidates:
        return candidates[0][0], candidates[0][1], None
    if fallback:
        return fallback[0][0], fallback[0][1], None
    return None, {}, "no metrics payload found"


def validate_mode_metrics(mode: str, metrics: dict[str, float]) -> str | None:
    required = MODE_REQUIRED_KEYS.get(mode)
    if not required:
        return None
    missing = [key for key in required if key not in metrics]
    if missing:
        return f"missing required metrics for mode '{mode}': {', '.join(missing)}"
    return None


def compare_metric_sets(
    baseline_metrics: dict[str, dict[str, Any]],
    target_metrics: dict[str, dict[str, Any]],
) -> list[dict[str, Any]]:
    all_metrics = sorted(set(baseline_metrics.keys()) | set(target_metrics.keys()))
    comparisons: list[dict[str, Any]] = []
    for metric in all_metrics:
        for p in PERCENTILES:
            p_key = f"p{p}"
            base_val = baseline_metrics.get(metric, {}).get(p_key)
            tgt_val = target_metrics.get(metric, {}).get(p_key)
            delta_abs = None
            delta_pct = None
            if base_val is not None and tgt_val is not None:
                delta_abs = tgt_val - base_val
                if base_val != 0:
                    delta_pct = (delta_abs / base_val) * 100.0
            comparisons.append(
                {
                    "metric": metric,
                    "percentile": p_key,
                    "baseline": base_val,
                    "target": tgt_val,
                    "delta_abs": delta_abs,
                    "delta_pct": delta_pct,
                }
            )
    return comparisons


def nearest_rank_percentile(values: list[float], percentile: int) -> float:
    if not values:
        raise ValueError("Cannot compute percentile of empty values")
    sorted_values = sorted(values)
    rank = math.ceil((percentile / 100.0) * len(sorted_values))
    idx = max(1, rank) - 1
    return sorted_values[idx]


def median(values: list[float]) -> float:
    return statistics.median(values)


def mad(values: list[float]) -> float:
    if not values:
        return 0.0
    med = median(values)
    return statistics.median(abs(v - med) for v in values)


def cv(values: list[float]) -> float:
    if not values:
        return 0.0
    mean_v = sum(values) / len(values)
    if mean_v == 0:
        return 0.0
    if len(values) < 2:
        return 0.0
    return statistics.pstdev(values) / abs(mean_v)


def bootstrap_relative_delta_ci(
    baseline: list[float],
    target: list[float],
    *,
    samples: int,
    confidence: float,
    rng: random.Random,
) -> tuple[float, float, float, float] | None:
    if not baseline or not target:
        return None
    base_med = median(baseline)
    tgt_med = median(target)
    if base_med == 0:
        return None
    point = ((tgt_med - base_med) / base_med) * 100.0

    deltas: list[float] = []
    for _ in range(samples):
        b = [baseline[rng.randrange(len(baseline))] for _ in range(len(baseline))]
        t = [target[rng.randrange(len(target))] for _ in range(len(target))]
        b_med = median(b)
        if b_med == 0:
            continue
        t_med = median(t)
        deltas.append(((t_med - b_med) / b_med) * 100.0)

    if not deltas:
        return None

    deltas.sort()
    alpha = 1.0 - confidence
    lo_idx = max(0, int(math.floor((alpha / 2.0) * (len(deltas) - 1))))
    hi_idx = min(len(deltas) - 1, int(math.ceil((1.0 - (alpha / 2.0)) * (len(deltas) - 1))))
    low = deltas[lo_idx]
    high = deltas[hi_idx]
    width = high - low
    return point, low, high, width


def metric_summary(values: list[float]) -> dict[str, Any]:
    sorted_values = sorted(values)
    summary: dict[str, Any] = {
        "count": len(sorted_values),
        "min": sorted_values[0],
        "max": sorted_values[-1],
        "mean": sum(sorted_values) / len(sorted_values),
        "median": median(sorted_values),
        "mad": mad(sorted_values),
        "cv": cv(sorted_values),
    }
    for p in PERCENTILES:
        summary[f"p{p}"] = nearest_rank_percentile(sorted_values, p)
    return summary


def fmt(value: float | None, digits: int = 6) -> str:
    if value is None:
        return "-"
    return f"{value:.{digits}f}"


def safe_name(name: str) -> str:
    return "".join(c if c.isalnum() or c in ("-", "_") else "_" for c in name)


def run_benchmark_once(worktree: Path, cmd: str, timeout_sec: int) -> tuple[int | None, bool, str, str, float]:
    start = time.monotonic()
    try:
        proc = subprocess.run(
            cmd,
            shell=True,
            cwd=str(worktree),
            text=True,
            capture_output=True,
            timeout=timeout_sec,
        )
        elapsed = time.monotonic() - start
        return proc.returncode, False, proc.stdout, proc.stderr, elapsed
    except subprocess.TimeoutExpired as exc:
        elapsed = time.monotonic() - start
        stdout = exc.stdout if isinstance(exc.stdout, str) else (exc.stdout or b"").decode(errors="replace")
        stderr = exc.stderr if isinstance(exc.stderr, str) else (exc.stderr or b"").decode(errors="replace")
        return None, True, stdout, stderr, elapsed


def write_text(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def quality_thresholds(args: argparse.Namespace) -> tuple[int, float, float, float, int, float]:
    if args.quality_mode == "strict":
        min_runs = args.min_runs if args.min_runs is not None else 20
        min_success_rate = args.min_success_rate if args.min_success_rate is not None else 0.98
        max_cv = args.max_cv if args.max_cv is not None else 0.08
        max_ci_width_pct = args.max_ci_width_pct if args.max_ci_width_pct is not None else 15.0
        bootstrap_samples = args.bootstrap_samples if args.bootstrap_samples is not None else 2000
        confidence = args.confidence if args.confidence is not None else 0.95
    else:
        min_runs = args.min_runs if args.min_runs is not None else 12
        min_success_rate = args.min_success_rate if args.min_success_rate is not None else 0.95
        max_cv = args.max_cv if args.max_cv is not None else 0.10
        max_ci_width_pct = args.max_ci_width_pct if args.max_ci_width_pct is not None else 20.0
        bootstrap_samples = args.bootstrap_samples if args.bootstrap_samples is not None else 1000
        confidence = args.confidence if args.confidence is not None else 0.95
    return min_runs, min_success_rate, max_cv, max_ci_width_pct, bootstrap_samples, confidence


def build_html_report(
    output_path: Path,
    *,
    metadata: dict[str, Any],
    per_branch: dict[str, dict[str, Any]],
    comparisons: list[dict[str, Any]],
    quality: dict[str, Any] | None = None,
) -> None:
    metrics = sorted(
        {
            metric
            for branch_data in per_branch.values()
            for metric in branch_data["metrics"].keys()
        }
    )

    html_parts: list[str] = []
    html_parts.append(
        """<!doctype html>
<html>
<head>
  <meta charset="utf-8" />
  <title>Benchmark Regression Report</title>
  <style>
    body { font-family: "Helvetica Neue", Arial, sans-serif; margin: 24px; color: #1d1d1f; }
    h1, h2, h3 { margin: 0 0 10px 0; }
    .meta { margin-bottom: 24px; padding: 12px; background: #f4f7fa; border-radius: 8px; }
    .quality-pass { margin-bottom: 16px; padding: 10px; border-radius: 8px; background: #e9f8ef; color: #0f8a39; }
    .quality-fail { margin-bottom: 16px; padding: 10px; border-radius: 8px; background: #fdecec; color: #b3261e; }
    .metric { margin: 22px 0; padding: 14px; border: 1px solid #e6e6e6; border-radius: 8px; }
    .grid { display: grid; grid-template-columns: 180px 80px 1fr 120px; gap: 8px; align-items: center; }
    .bar-wrap { background: #f0f0f0; height: 16px; border-radius: 999px; overflow: hidden; }
    .bar { height: 100%; }
    .branch-a { background: #2f80ed; }
    .branch-b { background: #eb5757; }
    table { border-collapse: collapse; width: 100%; margin-top: 10px; }
    th, td { border: 1px solid #ddd; padding: 8px; text-align: left; font-size: 14px; }
    th { background: #fafafa; }
    .delta-pos { color: #0f8a39; font-weight: 600; }
    .delta-neg { color: #b3261e; font-weight: 600; }
    code { background: #f7f7f7; padding: 2px 6px; border-radius: 4px; }
  </style>
</head>
<body>
"""
    )

    html_parts.append("<h1>Benchmark Regression Report</h1>")
    html_parts.append('<div class="meta">')
    html_parts.append(f"<p><strong>Timestamp:</strong> {html.escape(metadata['timestamp'])}</p>")
    html_parts.append(f"<p><strong>Command:</strong> <code>{html.escape(metadata['command'])}</code></p>")
    html_parts.append(
        f"<p><strong>Measured runs:</strong> {metadata['runs']} (warmup: {metadata['warmup']})</p>"
    )
    html_parts.append("<ul>")
    for branch_label in ("baseline", "target"):
        branch_data = per_branch[branch_label]
        html_parts.append(
            "<li>"
            f"<strong>{html.escape(branch_label)}</strong>: {html.escape(branch_data['name'])} "
            f"(sha: <code>{html.escape(branch_data['commit'])}</code>, "
            f"success: {branch_data['success_runs']}, failed: {branch_data['failed_runs']})"
            "</li>"
        )
    html_parts.append("</ul>")
    html_parts.append("</div>")

    if quality is not None:
        passed = quality.get("passed", False)
        cls = "quality-pass" if passed else "quality-fail"
        html_parts.append(f'<div class="{cls}">')
        html_parts.append(f"<strong>Quality:</strong> {'PASS' if passed else 'FAIL'}")
        reasons = quality.get("reasons", [])
        if reasons:
            html_parts.append("<ul>")
            for reason in reasons:
                html_parts.append(f"<li>{html.escape(reason)}</li>")
            html_parts.append("</ul>")
        html_parts.append("</div>")

    left_branch = "baseline"
    right_branch = "target"

    for metric in metrics:
        values: list[float] = []
        for branch in per_branch.values():
            stats = branch["metrics"].get(metric, {})
            for p in PERCENTILES:
                key = f"p{p}"
                if key in stats:
                    values.append(float(stats[key]))
        max_v = max(values) if values else 1.0

        html_parts.append(f'<div class="metric"><h3>{html.escape(metric)}</h3>')
        html_parts.append('<div class="grid">')
        html_parts.append("<strong>Branch</strong><strong>Stat</strong><strong>Bar</strong><strong>Value</strong>")
        for idx, branch_name in enumerate((left_branch, right_branch)):
            stats = per_branch[branch_name]["metrics"].get(metric, {})
            for p in PERCENTILES:
                stat_key = f"p{p}"
                val = stats.get(stat_key)
                width = 0.0 if val is None or max_v == 0 else (float(val) / max_v) * 100.0
                css = "branch-a" if idx == 0 else "branch-b"
                html_parts.append(f"<span>{html.escape(per_branch[branch_name]['name'])} ({branch_name})</span>")
                html_parts.append(f"<span>{stat_key}</span>")
                html_parts.append(
                    '<span class="bar-wrap">'
                    f'<span class="bar {css}" style="width:{width:.2f}%"></span>'
                    "</span>"
                )
                html_parts.append(f"<span>{fmt(float(val) if val is not None else None)}</span>")
        html_parts.append("</div></div>")

    html_parts.append("<h2>Percentile Comparison</h2>")
    html_parts.append(
        "<table><thead><tr>"
        "<th>Metric</th><th>Percentile</th><th>Baseline</th><th>Target</th>"
        "<th>Abs Delta</th><th>% Delta</th>"
        "</tr></thead><tbody>"
    )
    for row in comparisons:
        delta_pct = row.get("delta_pct")
        cls = ""
        if delta_pct is not None:
            cls = "delta-pos" if delta_pct >= 0 else "delta-neg"
        html_parts.append("<tr>")
        html_parts.append(f"<td>{html.escape(row['metric'])}</td>")
        html_parts.append(f"<td>{html.escape(row['percentile'])}</td>")
        html_parts.append(f"<td>{fmt(row.get('baseline'))}</td>")
        html_parts.append(f"<td>{fmt(row.get('target'))}</td>")
        html_parts.append(f"<td>{fmt(row.get('delta_abs'))}</td>")
        html_parts.append(f'<td class="{cls}">{fmt(delta_pct, 2) if delta_pct is not None else "-"}</td>')
        html_parts.append("</tr>")
    html_parts.append("</tbody></table>")

    html_parts.append("</body></html>")
    output_path.write_text("\n".join(html_parts), encoding="utf-8")


def process_command_series(
    *,
    label: str,
    name: str,
    cwd: Path,
    command: str,
    runs: int,
    warmup: int,
    timeout_sec: int,
    output_dir: Path,
) -> dict[str, Any]:
    # Legacy path used by git-branch comparison mode.
    branch_dir = output_dir / safe_name(f"{label}_{name}")
    runs_dir = branch_dir / "runs"
    parsed_dir = branch_dir / "parsed"
    runs_dir.mkdir(parents=True, exist_ok=True)
    parsed_dir.mkdir(parents=True, exist_ok=True)

    measured_values: dict[str, list[float]] = {}
    run_records: list[dict[str, Any]] = []
    total_runs = warmup + runs
    success_runs = 0
    failed_runs = 0

    for i in range(1, total_runs + 1):
        run_id = f"run_{i:03d}"
        is_warmup = i <= warmup
        returncode, timed_out, stdout, stderr, elapsed_sec = run_benchmark_once(cwd, command, timeout_sec)

        write_text(runs_dir / f"{run_id}.stdout.txt", stdout)
        write_text(runs_dir / f"{run_id}.stderr.txt", stderr)

        json_objects = extract_json_objects(stdout)
        metric_obj, metrics, parse_error = choose_metric_object(json_objects)
        ok = (not timed_out) and (returncode == 0) and bool(metrics)
        if not is_warmup:
            if ok:
                success_runs += 1
            else:
                failed_runs += 1

        run_record: dict[str, Any] = {
            "run_id": run_id,
            "branch_label": label,
            "is_warmup": is_warmup,
            "ok": ok,
            "timed_out": timed_out,
            "returncode": returncode,
            "elapsed_sec": elapsed_sec,
            "metrics": metrics,
            "error": None,
        }
        if timed_out:
            run_record["error"] = f"timeout after {timeout_sec}s"
        elif returncode != 0:
            run_record["error"] = f"non-zero exit code: {returncode}"
        elif not metrics:
            run_record["error"] = parse_error or "no metrics parsed from stdout"

        parsed_payload = {
            "run_id": run_id,
            "branch_label": label,
            "branch": name,
            "is_warmup": is_warmup,
            "raw_json_objects": json_objects,
            "selected_metric_object": metric_obj,
            "metrics": metrics,
            "timed_out": timed_out,
            "returncode": returncode,
            "elapsed_sec": elapsed_sec,
        }
        write_text(parsed_dir / f"{run_id}.json", json.dumps(parsed_payload, indent=2, sort_keys=True))

        run_records.append(run_record)
        if ok and not is_warmup:
            for key, value in metrics.items():
                measured_values.setdefault(key, []).append(value)

    metric_stats: dict[str, Any] = {}
    for metric_name, values in measured_values.items():
        if values:
            metric_stats[metric_name] = metric_summary(values)

    if success_runs == 0:
        raise RuntimeError(
            f"{label} '{name}' had zero successful measured runs. See {runs_dir} for logs."
        )

    return {
        "label": label,
        "name": name,
        "branch": name,
        "commit": "n/a",
        "success_runs": success_runs,
        "failed_runs": failed_runs,
        "metrics": metric_stats,
        "runs": run_records,
    }


def make_cross_command(
    *,
    side: str,
    mode: str,
    duration: int,
    inner_warmup: int,
    broker_url: str,
    payload_size: int,
    qos: int,
    publishers: int,
    subscribers: int,
    concurrency: int,
    ca_cert: str | None,
    client_id: str,
    run_id: str,
) -> str:
    if side == "baseline":
        cmd = [
            "cargo", "run", "--bin", "rumqttv5bench", "--release", "--",
            "--mode", mode,
            "--duration", str(duration),
            "--warmup", str(inner_warmup),
            "--url", broker_url,
            "--client-id", client_id,
            "--run-id", run_id,
        ]
    else:
        cmd = [
            "cargo", "run", "-p", "mqttv5-cli", "--release", "--", "bench",
            "--mode", mode,
            "--duration", str(duration),
            "--warmup", str(inner_warmup),
            "--url", broker_url,
            "--client-id", client_id,
        ]

    if mode != "connections":
        cmd.extend(["--payload-size", str(payload_size), "--qos", str(qos)])
    if mode == "throughput":
        cmd.extend(["--publishers", str(publishers), "--subscribers", str(subscribers)])
    if mode == "connections":
        cmd.extend(["--concurrency", str(concurrency)])
    if broker_url.startswith("mqtts://") and ca_cert:
        cmd.extend(["--ca-cert", ca_cert])

    return shlex.join(cmd)


def evaluate_scenario_quality(
    *,
    mode: str,
    per_branch: dict[str, dict[str, Any]],
    min_runs: int,
    min_success_rate: float,
    max_cv: float,
    max_ci_width_pct: float,
    bootstrap_samples: int,
    confidence: float,
    rng_seed: int,
) -> tuple[dict[str, Any], dict[str, Any]]:
    reasons: list[str] = []
    checks: dict[str, Any] = {}
    stats: dict[str, Any] = {"per_metric": {}, "comparisons": {}}
    passed = True

    for side in ("baseline", "target"):
        succ = per_branch[side]["success_runs"]
        fail = per_branch[side]["failed_runs"]
        total = succ + fail
        success_rate = (succ / total) if total else 0.0
        checks[f"{side}_success_runs"] = succ
        checks[f"{side}_success_rate"] = success_rate
        if succ < min_runs:
            passed = False
            reasons.append(f"{side}: successful runs {succ} below minimum {min_runs}")
        if success_rate < min_success_rate:
            passed = False
            reasons.append(f"{side}: success rate {success_rate:.3f} below minimum {min_success_rate:.3f}")

    primary_metric = PRIMARY_METRIC_BY_MODE.get(mode)
    checks["primary_metric"] = primary_metric

    for side in ("baseline", "target"):
        metric_stats = per_branch[side]["metrics"].get(primary_metric, {}) if primary_metric else {}
        side_cv = metric_stats.get("cv")
        checks[f"{side}_primary_cv"] = side_cv
        if side_cv is None:
            passed = False
            reasons.append(f"{side}: primary metric '{primary_metric}' missing")
        elif side_cv > max_cv:
            passed = False
            reasons.append(f"{side}: primary CV {side_cv:.4f} above threshold {max_cv:.4f}")

    baseline_runs = per_branch["baseline"]["runs"]
    target_runs = per_branch["target"]["runs"]
    metric_names = sorted(set(per_branch["baseline"]["metrics"].keys()) | set(per_branch["target"]["metrics"].keys()))

    for metric in metric_names:
        b_vals = [float(r["metrics"][metric]) for r in baseline_runs if (not r["is_warmup"]) and r["ok"] and metric in r["metrics"]]
        t_vals = [float(r["metrics"][metric]) for r in target_runs if (not r["is_warmup"]) and r["ok"] and metric in r["metrics"]]
        if b_vals:
            stats["per_metric"].setdefault(metric, {})["baseline"] = metric_summary(b_vals)
        if t_vals:
            stats["per_metric"].setdefault(metric, {})["target"] = metric_summary(t_vals)

        rng = random.Random(rng_seed + abs(hash(metric)) % 100000)
        ci = bootstrap_relative_delta_ci(
            b_vals,
            t_vals,
            samples=bootstrap_samples,
            confidence=confidence,
            rng=rng,
        )
        if ci is not None:
            point, low, high, width = ci
            stats["comparisons"][metric] = {
                "relative_delta_median_pct": point,
                "relative_delta_ci_low_pct": low,
                "relative_delta_ci_high_pct": high,
                "relative_delta_ci_width_pct": width,
            }
            if metric == primary_metric:
                checks["primary_ci_width_pct"] = width
                if width > max_ci_width_pct:
                    passed = False
                    reasons.append(
                        f"primary metric '{primary_metric}' CI width {width:.2f}% above threshold {max_ci_width_pct:.2f}%"
                    )
        else:
            stats["comparisons"][metric] = {"error": "insufficient data for bootstrap CI"}
            if metric == primary_metric:
                passed = False
                reasons.append(f"primary metric '{primary_metric}' has insufficient data for bootstrap CI")

    quality = {
        "passed": passed,
        "reasons": reasons,
        "checks": checks,
    }
    return quality, stats


def run_cross_library(args: argparse.Namespace) -> int:
    if not args.rumqtt_root or not args.mqttlib_root:
        raise RuntimeError("--cross-library requires --rumqtt-root and --mqttlib-root")
    if not args.broker_url_tcp:
        raise RuntimeError("--cross-library requires --broker-url-tcp")

    rumqtt_root = Path(args.rumqtt_root).expanduser().resolve()
    mqttlib_root = Path(args.mqttlib_root).expanduser().resolve()
    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%SZ")

    if args.output_dir:
        output_dir = Path(args.output_dir).expanduser().resolve()
    else:
        output_dir = (rumqtt_root / "benchmarks" / "results" / "cross-library" / timestamp).resolve()
    output_dir.mkdir(parents=True, exist_ok=True)

    min_runs, min_success_rate, max_cv, max_ci_width_pct, bootstrap_samples, confidence = quality_thresholds(args)

    scenario_runs: list[dict[str, Any]] = []
    any_quality_failures = False
    matrix = [
        {"mode": "throughput", "duration": 6, "payload_size": 64, "qos": 0, "publishers": 1, "subscribers": 1},
        {"mode": "throughput", "duration": 6, "payload_size": 64, "qos": 1, "publishers": 1, "subscribers": 1},
        {"mode": "latency", "duration": 6, "payload_size": 64, "qos": 1, "publishers": 1, "subscribers": 1},
        {"mode": "connections", "duration": 6, "payload_size": 0, "qos": 0, "publishers": 0, "subscribers": 0},
    ]

    transports = [("tcp", args.broker_url_tcp)]
    if args.broker_url_tls:
        transports.append(("tls", args.broker_url_tls))

    seed_prefix = args.seed_prefix or f"bench-{timestamp}"
    host_name = socket.gethostname()
    runtime_meta = {
        "timestamp": dt.datetime.now(dt.timezone.utc).isoformat(),
        "hostname": host_name,
        "python": sys.version,
        "quality_mode": args.quality_mode,
        "thresholds": {
            "min_runs": min_runs,
            "min_success_rate": min_success_rate,
            "max_cv": max_cv,
            "max_ci_width_pct": max_ci_width_pct,
            "bootstrap_samples": bootstrap_samples,
            "confidence": confidence,
        },
        "repos": {
            "rumqtt": {
                "path": str(rumqtt_root),
                "commit": get_commit(rumqtt_root),
            },
            "mqttlib": {
                "path": str(mqttlib_root),
                "commit": get_commit(mqttlib_root),
            },
        },
    }

    for transport_name, broker_url in transports:
        for scenario in matrix:
            mode = scenario["mode"]
            scenario_name = f"{mode}-{transport_name}-qos{scenario['qos']}-ps{scenario['payload_size']}"
            scenario_dir = output_dir / safe_name(scenario_name)
            scenario_dir.mkdir(parents=True, exist_ok=True)

            per_branch = {
                "baseline": {
                    "label": "baseline",
                    "name": "rumqtt-v5",
                    "branch": "rumqtt-v5",
                    "commit": runtime_meta["repos"]["rumqtt"]["commit"],
                    "success_runs": 0,
                    "failed_runs": 0,
                    "metrics": {},
                    "runs": [],
                },
                "target": {
                    "label": "target",
                    "name": "mqttv5-cli",
                    "branch": "mqttv5-cli",
                    "commit": runtime_meta["repos"]["mqttlib"]["commit"],
                    "success_runs": 0,
                    "failed_runs": 0,
                    "metrics": {},
                    "runs": [],
                },
            }
            measured_values: dict[str, dict[str, list[float]]] = {"baseline": {}, "target": {}}
            total_runs = args.warmup + args.runs
            run_order: list[dict[str, Any]] = []

            for i in range(1, total_runs + 1):
                run_id = f"run_{i:03d}"
                is_warmup = i <= args.warmup
                if args.alternate_order:
                    order = ["baseline", "target"] if i % 2 == 1 else ["target", "baseline"]
                else:
                    order = ["baseline", "target"]
                run_order.append({"run_id": run_id, "order": order})

                for side in order:
                    repo_root = rumqtt_root if side == "baseline" else mqttlib_root
                    client_id = f"{seed_prefix}-{safe_name(scenario_name)}-{side}-{run_id}"
                    inner_warmup = 2 if args.inner_warmup else 0
                    command = make_cross_command(
                        side=side,
                        mode=mode,
                        duration=scenario["duration"],
                        inner_warmup=inner_warmup,
                        broker_url=broker_url,
                        payload_size=scenario["payload_size"],
                        qos=scenario["qos"],
                        publishers=scenario["publishers"],
                        subscribers=scenario["subscribers"],
                        concurrency=10,
                        ca_cert=args.ca_cert,
                        client_id=client_id,
                        run_id=f"{safe_name(scenario_name)}-{side}-{run_id}",
                    )

                    branch_dir = scenario_dir / safe_name(f"{side}_{per_branch[side]['name']}")
                    runs_dir = branch_dir / "runs"
                    parsed_dir = branch_dir / "parsed"
                    runs_dir.mkdir(parents=True, exist_ok=True)
                    parsed_dir.mkdir(parents=True, exist_ok=True)

                    returncode, timed_out, stdout, stderr, elapsed_sec = run_benchmark_once(repo_root, command, args.timeout_sec)
                    write_text(runs_dir / f"{run_id}.stdout.txt", stdout)
                    write_text(runs_dir / f"{run_id}.stderr.txt", stderr)

                    json_objects = extract_json_objects(stdout)
                    metric_obj, metrics, parse_error = choose_metric_object(
                        json_objects,
                        expected_mode=mode,
                        strict_schema=True,
                    )
                    schema_error = validate_mode_metrics(mode, metrics) if metrics else None
                    ok = (not timed_out) and (returncode == 0) and bool(metrics) and (schema_error is None)

                    if not is_warmup:
                        if ok:
                            per_branch[side]["success_runs"] += 1
                        else:
                            per_branch[side]["failed_runs"] += 1

                    run_record: dict[str, Any] = {
                        "run_id": run_id,
                        "branch_label": side,
                        "is_warmup": is_warmup,
                        "ok": ok,
                        "timed_out": timed_out,
                        "returncode": returncode,
                        "elapsed_sec": elapsed_sec,
                        "metrics": metrics,
                        "command": command,
                        "error": None,
                    }

                    if timed_out:
                        run_record["error"] = f"timeout after {args.timeout_sec}s"
                    elif returncode != 0:
                        run_record["error"] = f"non-zero exit code: {returncode}"
                    elif parse_error:
                        run_record["error"] = parse_error
                    elif schema_error:
                        run_record["error"] = schema_error

                    parsed_payload = {
                        "run_id": run_id,
                        "branch_label": side,
                        "branch": per_branch[side]["name"],
                        "is_warmup": is_warmup,
                        "raw_json_objects": json_objects,
                        "selected_metric_object": metric_obj,
                        "metrics": metrics,
                        "timed_out": timed_out,
                        "returncode": returncode,
                        "elapsed_sec": elapsed_sec,
                        "parse_error": parse_error,
                        "schema_error": schema_error,
                    }
                    write_text(parsed_dir / f"{run_id}.json", json.dumps(parsed_payload, indent=2, sort_keys=True))

                    per_branch[side]["runs"].append(run_record)
                    if ok and not is_warmup:
                        for key, value in metrics.items():
                            measured_values[side].setdefault(key, []).append(value)

            for side in ("baseline", "target"):
                metric_stats: dict[str, Any] = {}
                for metric_name, values in measured_values[side].items():
                    if values:
                        metric_stats[metric_name] = metric_summary(values)
                per_branch[side]["metrics"] = metric_stats

            comparisons = compare_metric_sets(
                per_branch["baseline"]["metrics"], per_branch["target"]["metrics"]
            )

            quality, stats = evaluate_scenario_quality(
                mode=mode,
                per_branch=per_branch,
                min_runs=min_runs,
                min_success_rate=min_success_rate,
                max_cv=max_cv,
                max_ci_width_pct=max_ci_width_pct,
                bootstrap_samples=bootstrap_samples,
                confidence=confidence,
                rng_seed=abs(hash(f"{seed_prefix}-{scenario_name}")) % (2**31),
            )

            if not quality["passed"]:
                any_quality_failures = True

            report_meta = {
                "timestamp": dt.datetime.now(dt.timezone.utc).isoformat(),
                "command": f"cross-library scenario {scenario_name}",
                "runs": args.runs,
                "warmup": args.warmup,
            }
            build_html_report(
                scenario_dir / "compare.html",
                metadata=report_meta,
                per_branch=per_branch,
                comparisons=comparisons,
                quality=quality,
            )

            summary = {
                "scenario": scenario_name,
                "transport": transport_name,
                "mode": mode,
                "run_order": run_order,
                "results": per_branch,
                "comparison": comparisons,
                "quality": quality,
                "statistics": stats,
                "provenance": runtime_meta,
            }
            write_text(scenario_dir / "summary.json", json.dumps(summary, indent=2, sort_keys=True))
            scenario_runs.append(summary)

    top = {
        "scenarios": scenario_runs,
        "quality_passed": not any_quality_failures,
        "provenance": runtime_meta,
    }
    write_text(output_dir / "summary_cross.json", json.dumps(top, indent=2, sort_keys=True))
    print(f"Cross-library benchmark complete. Output directory: {output_dir}")
    print(f"Summary JSON: {output_dir / 'summary_cross.json'}")

    if any_quality_failures:
        print("One or more scenarios failed quality checks.")
        return 2
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Run benchmark regression comparison across two branches.")
    parser.add_argument("--target-branch", required=False, help="Branch/ref to compare against current branch.")
    parser.add_argument(
        "--cross-library",
        action="store_true",
        help="Run cross-library benchmark matrix (rumqtt-v5 vs mqttv5-cli) instead of git branch comparison.",
    )
    parser.add_argument("--rumqtt-root", default=None, help="Path to rumqtt repository root.")
    parser.add_argument("--mqttlib-root", default=None, help="Path to mqtt-lib repository root.")
    parser.add_argument("--broker-url-tcp", default=None, help="Broker URL for TCP benchmark scenarios.")
    parser.add_argument("--broker-url-tls", default=None, help="Broker URL for TLS benchmark scenarios.")
    parser.add_argument("--ca-cert", default=None, help="CA certificate path for TLS benchmark scenarios.")
    parser.add_argument(
        "--runs",
        type=int,
        default=12,
        help="Number of measured runs per branch (cross-library default: 12).",
    )
    parser.add_argument(
        "--warmup",
        type=int,
        default=1,
        help="Optional warmup runs per branch excluded from percentile stats.",
    )
    parser.add_argument(
        "--cmd",
        default="cargo run --bin v4parser --release",
        help="Benchmark command to execute inside each branch worktree.",
    )
    parser.add_argument(
        "--timeout-sec",
        type=int,
        default=300,
        help="Per-run timeout in seconds (default: 300).",
    )
    parser.add_argument(
        "--quality-mode",
        choices=("fast", "strict"),
        default="fast",
        help="Quality thresholds preset for cross-library mode.",
    )
    parser.add_argument("--min-runs", type=int, default=None, help="Minimum successful measured runs per side.")
    parser.add_argument("--min-success-rate", type=float, default=None, help="Minimum run success rate per side.")
    parser.add_argument("--max-cv", type=float, default=None, help="Maximum allowed CV on scenario primary metric.")
    parser.add_argument("--bootstrap-samples", type=int, default=None, help="Bootstrap samples for CI computation.")
    parser.add_argument("--confidence", type=float, default=None, help="Bootstrap confidence level (0,1).")
    parser.add_argument("--max-ci-width-pct", type=float, default=None, help="Max CI width for primary relative delta.")
    parser.add_argument("--seed-prefix", default=None, help="Prefix used to generate deterministic run ids/client ids.")
    parser.add_argument(
        "--alternate-order",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="Alternate baseline/target execution order each run.",
    )
    parser.add_argument(
        "--inner-warmup",
        action=argparse.BooleanOptionalAction,
        default=False,
        help="Enable benchmark binary warmup in addition to harness warmup.",
    )
    parser.add_argument(
        "--output-dir",
        default=None,
        help="Output directory. Default: benchmarks/results/regressions/<timestamp>.",
    )
    parser.add_argument(
        "--keep-worktrees",
        action="store_true",
        help="Keep temporary worktree directories after completion.",
    )

    args = parser.parse_args()
    if args.runs <= 0:
        parser.error("--runs must be > 0")
    if args.warmup < 0:
        parser.error("--warmup must be >= 0")
    if args.timeout_sec <= 0:
        parser.error("--timeout-sec must be > 0")
    if args.confidence is not None and not (0.0 < args.confidence < 1.0):
        parser.error("--confidence must be between 0 and 1")
    if not args.cross_library and not args.target_branch:
        parser.error("--target-branch is required unless --cross-library is set")
    if args.cross_library:
        return run_cross_library(args)

    repo_root = get_repo_root()
    baseline_branch = get_current_branch(repo_root)
    target_branch = args.target_branch

    baseline_ref = resolve_ref(repo_root, baseline_branch)
    target_ref = resolve_ref(repo_root, target_branch)

    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%SZ")
    if args.output_dir:
        output_dir = Path(args.output_dir).expanduser().resolve()
    else:
        output_dir = (repo_root / "benchmarks" / "results" / "regressions" / timestamp).resolve()
    output_dir.mkdir(parents=True, exist_ok=True)

    temp_root = Path(tempfile.mkdtemp(prefix="rumqtt-regression-"))
    worktrees: dict[str, Path] = {}
    branch_specs = [
        {"label": "baseline", "name": baseline_branch, "ref": baseline_ref},
        {"label": "target", "name": target_branch, "ref": target_ref},
    ]

    try:
        for spec in branch_specs:
            branch_label = spec["label"]
            branch_name = spec["name"]
            ref = spec["ref"]
            wt_path = temp_root / safe_name(f"{branch_label}_{branch_name}")
            proc = run_process(
                ["git", "worktree", "add", "--detach", str(wt_path), ref],
                cwd=repo_root,
                capture=True,
            )
            if proc.returncode != 0:
                raise RuntimeError(
                    f"Failed to create worktree for '{branch_name}': {proc.stderr.strip() or proc.stdout.strip()}"
                )
            worktrees[branch_label] = wt_path

        per_branch: dict[str, dict[str, Any]] = {}
        for spec in branch_specs:
            branch_label = spec["label"]
            branch_name = spec["name"]
            wt_path = worktrees[branch_label]
            branch_dir = output_dir / safe_name(f"{branch_label}_{branch_name}")
            runs_dir = branch_dir / "runs"
            parsed_dir = branch_dir / "parsed"
            runs_dir.mkdir(parents=True, exist_ok=True)
            parsed_dir.mkdir(parents=True, exist_ok=True)

            commit_proc = run_process(["git", "rev-parse", "HEAD"], cwd=wt_path, check=True)
            commit = commit_proc.stdout.strip()

            measured_values: dict[str, list[float]] = {}
            run_records: list[dict[str, Any]] = []
            total_runs = args.warmup + args.runs
            success_runs = 0
            failed_runs = 0

            for i in range(1, total_runs + 1):
                run_id = f"run_{i:03d}"
                is_warmup = i <= args.warmup
                returncode, timed_out, stdout, stderr, elapsed_sec = run_benchmark_once(
                    wt_path, args.cmd, args.timeout_sec
                )

                write_text(runs_dir / f"{run_id}.stdout.txt", stdout)
                write_text(runs_dir / f"{run_id}.stderr.txt", stderr)

                json_objects = extract_json_objects(stdout)
                metric_obj, metrics, parse_error = choose_metric_object(json_objects)

                ok = (not timed_out) and (returncode == 0) and bool(metrics)
                if not is_warmup:
                    if ok:
                        success_runs += 1
                    else:
                        failed_runs += 1

                run_record: dict[str, Any] = {
                    "run_id": run_id,
                    "branch_label": branch_label,
                    "is_warmup": is_warmup,
                    "ok": ok,
                    "timed_out": timed_out,
                    "returncode": returncode,
                    "elapsed_sec": elapsed_sec,
                    "metrics": metrics,
                    "error": None,
                }

                if timed_out:
                    run_record["error"] = f"timeout after {args.timeout_sec}s"
                elif returncode != 0:
                    run_record["error"] = f"non-zero exit code: {returncode}"
                elif not metrics:
                    run_record["error"] = parse_error or "no throughput metrics parsed from stdout"

                parsed_payload = {
                    "run_id": run_id,
                    "branch_label": branch_label,
                    "branch": branch_name,
                    "is_warmup": is_warmup,
                    "raw_json_objects": json_objects,
                    "selected_metric_object": metric_obj,
                    "metrics": metrics,
                    "timed_out": timed_out,
                    "returncode": returncode,
                    "elapsed_sec": elapsed_sec,
                }
                write_text(parsed_dir / f"{run_id}.json", json.dumps(parsed_payload, indent=2, sort_keys=True))

                run_records.append(run_record)
                if ok and not is_warmup:
                    for key, value in metrics.items():
                        measured_values.setdefault(key, []).append(value)

            metric_stats: dict[str, Any] = {}
            for metric_name, values in measured_values.items():
                if values:
                    metric_stats[metric_name] = metric_summary(values)

            if success_runs == 0:
                raise RuntimeError(
                    f"{branch_label} branch '{branch_name}' had zero successful measured runs. "
                    f"See {branch_dir / 'runs'} for logs."
                )

            per_branch[branch_label] = {
                "label": branch_label,
                "name": branch_name,
                "branch": branch_name,
                "commit": commit,
                "success_runs": success_runs,
                "failed_runs": failed_runs,
                "metrics": metric_stats,
                "runs": run_records,
            }

        comparisons = compare_metric_sets(per_branch["baseline"]["metrics"], per_branch["target"]["metrics"])

        summary = {
            "timestamp": timestamp,
            "command": args.cmd,
            "runs": args.runs,
            "warmup": args.warmup,
            "timeout_sec": args.timeout_sec,
            "branches": {
                "baseline": baseline_branch,
                "target": target_branch,
            },
            "results": per_branch,
            "comparison": comparisons,
        }
        write_text(output_dir / "summary.json", json.dumps(summary, indent=2, sort_keys=True))

        with (output_dir / "summary.csv").open("w", newline="", encoding="utf-8") as f:
            writer = csv.writer(f)
            writer.writerow(
                [
                    "section",
                    "branch",
                    "metric",
                    "stat",
                    "value",
                    "baseline_value",
                    "target_value",
                    "delta_abs",
                    "delta_pct",
                ]
            )

            for branch_label in ("baseline", "target"):
                branch_data = per_branch[branch_label]
                branch_title = f"{branch_data['name']} ({branch_label})"
                writer.writerow(["branch", branch_title, "_runs", "success", branch_data["success_runs"], "", "", "", ""])
                writer.writerow(["branch", branch_title, "_runs", "failed", branch_data["failed_runs"], "", "", "", ""])
                for metric_name, stats in branch_data["metrics"].items():
                    for stat_name in ("min", "max", "mean", "median", "mad", "cv", "p50", "p90", "p99"):
                        val = stats.get(stat_name)
                        writer.writerow(["branch", branch_title, metric_name, stat_name, val, "", "", "", ""])

            for row in comparisons:
                writer.writerow(
                    [
                        "comparison",
                        "",
                        row["metric"],
                        row["percentile"],
                        "",
                        row["baseline"],
                        row["target"],
                        row["delta_abs"],
                        row["delta_pct"],
                    ]
                )

        report_meta = {
            "timestamp": dt.datetime.now(dt.timezone.utc).isoformat(),
            "command": args.cmd,
            "runs": args.runs,
            "warmup": args.warmup,
        }
        build_html_report(
            output_dir / "compare.html",
            metadata=report_meta,
            per_branch=per_branch,
            comparisons=comparisons,
        )

        print(f"Regression complete. Output directory: {output_dir}")
        print(f"Baseline branch: {baseline_branch} ({per_branch['baseline']['commit']})")
        print(f"Target branch:   {target_branch} ({per_branch['target']['commit']})")
        print(f"Summary JSON:    {output_dir / 'summary.json'}")
        print(f"Summary CSV:     {output_dir / 'summary.csv'}")
        print(f"HTML report:     {output_dir / 'compare.html'}")
        return 0

    finally:
        if args.keep_worktrees:
            print(f"Keeping temporary worktrees at: {temp_root}")
        else:
            for wt in worktrees.values():
                run_process(["git", "worktree", "remove", "--force", str(wt)], cwd=repo_root, capture=True)
            shutil.rmtree(temp_root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
