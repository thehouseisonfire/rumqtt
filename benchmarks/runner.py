#!/usr/bin/env python3
"""Scenario runner and branch-comparison tool for rumqtt benchmarks."""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import html
import json
import math
import random
import statistics
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path
from typing import Any


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
    for key in ("name", "group", "command"):
        if not isinstance(scenario.get(key), str):
            raise RuntimeError(f"{path}: missing string field '{key}'")
    if scenario["group"] not in {"client", "codec"}:
        raise RuntimeError(f"{path}: group must be 'client' or 'codec'")
    commands = {
        "client": {"throughput", "latency", "connections"},
        "codec": {"encode", "decode", "roundtrip"},
    }
    if scenario["command"] not in commands[scenario["group"]]:
        raise RuntimeError(f"{path}: unsupported command for group '{scenario['group']}'")
    if "args" in scenario and not isinstance(scenario["args"], dict):
        raise RuntimeError(f"{path}: args must be a table")


def scenario_command(
    scenario: dict[str, Any],
    *,
    run_id: str,
    broker_url: str | None,
    ca_cert: str | None,
) -> list[str]:
    cmd = [
        "cargo",
        "run",
        "-p",
        "benchmarks",
        "--bin",
        "rumqtt-bench",
        "--",
        scenario["group"],
        scenario["command"],
    ]
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


def read_benchmark_json(stdout: str) -> dict[str, Any]:
    try:
        data = json.loads(stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"benchmark stdout was not JSON: {exc}") from exc
    if not isinstance(data, dict) or not isinstance(data.get("metrics"), dict):
        raise RuntimeError("benchmark JSON must be an object with a metrics object")
    return data


def run_once(
    *,
    root: Path,
    scenario: dict[str, Any],
    run_id: str,
    broker_url: str | None,
    ca_cert: str | None,
    timeout: int,
) -> dict[str, Any]:
    cmd = scenario_command(
        scenario,
        run_id=run_id,
        broker_url=broker_url,
        ca_cert=ca_cert,
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
    try:
        payload = read_benchmark_json(proc.stdout)
    except RuntimeError as exc:
        result["ok"] = False
        result["error"] = str(exc)
        result["stdout"] = proc.stdout
        return result

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


def percentile(values: list[float], pct: int) -> float:
    sorted_values = sorted(values)
    rank = math.ceil((pct / 100.0) * len(sorted_values))
    return sorted_values[max(rank, 1) - 1]


def metric_summary(values: list[float]) -> dict[str, float | int]:
    if not values:
        return {"count": 0}
    return {
        "count": len(values),
        "min": min(values),
        "max": max(values),
        "mean": sum(values) / len(values),
        "median": median(values),
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
) -> dict[str, Any]:
    if not baseline or not target:
        return {"error": "missing baseline or target values"}
    base_median = median(baseline)
    target_median = median(target)
    if base_median == 0:
        return {"error": "baseline median is zero"}

    deltas = []
    for _ in range(samples):
        b = [baseline[rng.randrange(len(baseline))] for _ in baseline]
        t = [target[rng.randrange(len(target))] for _ in target]
        b_median = median(b)
        if b_median != 0:
            deltas.append(((median(t) - b_median) / b_median) * 100.0)
    if not deltas:
        return {"error": "no bootstrap samples"}

    deltas.sort()
    alpha = 1.0 - confidence
    lo_idx = max(0, int(math.floor((alpha / 2.0) * (len(deltas) - 1))))
    hi_idx = min(len(deltas) - 1, int(math.ceil((1.0 - (alpha / 2.0)) * (len(deltas) - 1))))
    low = deltas[lo_idx]
    high = deltas[hi_idx]
    point = ((target_median - base_median) / base_median) * 100.0
    classification = "inconclusive"
    if low > 0:
        classification = "increase"
    elif high < 0:
        classification = "decrease"
    return {
        "baseline_median": base_median,
        "target_median": target_median,
        "relative_delta_pct": point,
        "relative_delta_ci_low_pct": low,
        "relative_delta_ci_high_pct": high,
        "classification": classification,
    }


def compare_summaries(
    baseline_runs: list[dict[str, Any]],
    target_runs: list[dict[str, Any]],
    *,
    bootstrap_samples: int,
    confidence: float,
) -> dict[str, Any]:
    baseline_ok = [run for run in baseline_runs if run.get("ok")]
    target_ok = [run for run in target_runs if run.get("ok")]
    metric_names = sorted(
        {key for run in baseline_ok for key in run.get("metrics", {})}
        | {key for run in target_ok for key in run.get("metrics", {})}
    )
    return {
        metric: bootstrap_delta(
            [run["metrics"][metric] for run in baseline_ok if metric in run["metrics"]],
            [run["metrics"][metric] for run in target_ok if metric in run["metrics"]],
            samples=bootstrap_samples,
            confidence=confidence,
            rng=random.Random(metric),
        )
        for metric in metric_names
    }


def write_report(output_dir: Path, summary: dict[str, Any]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8"
    )

    with (output_dir / "summary.csv").open("w", newline="", encoding="utf-8") as handle:
        writer = csv.writer(handle)
        writer.writerow(["section", "metric", "field", "value"])
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

    html_rows = []
    for metric, fields in summary.get("comparison", {}).items():
        html_rows.append(
            "<tr>"
            f"<td>{html.escape(metric)}</td>"
            f"<td>{fields.get('baseline_median', '-')}</td>"
            f"<td>{fields.get('target_median', '-')}</td>"
            f"<td>{fields.get('relative_delta_pct', '-')}</td>"
            f"<td>{html.escape(str(fields.get('classification', '-')))}</td>"
            "</tr>"
        )
    html_body = "\n".join(html_rows) or "<tr><td colspan='5'>No comparison data</td></tr>"
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
  <table>
    <thead>
      <tr><th>Metric</th><th>Baseline median</th><th>Target median</th><th>Delta %</th><th>Class</th></tr>
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
            timeout=args.timeout_sec,
        )
        run["is_warmup"] = index < args.warmup_runs
        runs.append(run)
    measured = [run for run in runs if not run["is_warmup"]]
    summary = {
        "scenario": scenario["name"],
        "scenario_path": str(scenario_path),
        "mode": "run",
        "runs": runs,
        "summary": summarize_runs(measured),
    }
    write_report(output_dir, summary)
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
                    timeout=args.timeout_sec,
                )
                run["is_warmup"] = index < args.warmup_runs
                runs[side].append(run)

        baseline_measured = [run for run in runs["baseline"] if not run["is_warmup"]]
        target_measured = [run for run in runs["target"] if not run["is_warmup"]]
        summary = {
            "scenario": scenario["name"],
            "scenario_path": str(scenario_path),
            "mode": "compare",
            "baseline_ref": baseline_ref,
            "target_ref": target_ref,
            "baseline": summarize_runs(baseline_measured),
            "target": summarize_runs(target_measured),
            "comparison": compare_summaries(
                baseline_measured,
                target_measured,
                bootstrap_samples=args.bootstrap_samples,
                confidence=args.confidence,
            ),
            "runs": runs,
        }
        write_report(output_dir, summary)
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
