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
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any


PERCENTILES = (50, 90, 99)


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
    for key, value in obj.items():
        if "throughput" in key and isinstance(value, (int, float)):
            metrics[key] = float(value)
    return metrics


def choose_metric_object(objs: list[dict[str, Any]]) -> tuple[dict[str, Any] | None, dict[str, float]]:
    for obj in objs:
        metrics = extract_metrics(obj)
        if metrics:
            return obj, metrics
    return None, {}


def nearest_rank_percentile(values: list[float], percentile: int) -> float:
    if not values:
        raise ValueError("Cannot compute percentile of empty values")
    sorted_values = sorted(values)
    rank = math.ceil((percentile / 100.0) * len(sorted_values))
    idx = max(1, rank) - 1
    return sorted_values[idx]


def metric_summary(values: list[float]) -> dict[str, Any]:
    sorted_values = sorted(values)
    summary: dict[str, Any] = {
        "count": len(sorted_values),
        "min": sorted_values[0],
        "max": sorted_values[-1],
        "mean": sum(sorted_values) / len(sorted_values),
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


def build_html_report(
    output_path: Path,
    *,
    metadata: dict[str, Any],
    per_branch: dict[str, dict[str, Any]],
    comparisons: list[dict[str, Any]],
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


def main() -> int:
    parser = argparse.ArgumentParser(description="Run benchmark regression comparison across two branches.")
    parser.add_argument("--target-branch", required=True, help="Branch/ref to compare against current branch.")
    parser.add_argument(
        "--runs",
        type=int,
        default=100,
        help="Number of measured runs per branch (default: 100).",
    )
    parser.add_argument(
        "--warmup",
        type=int,
        default=0,
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
                metric_obj, metrics = choose_metric_object(json_objects)

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
                    run_record["error"] = "no throughput metrics parsed from stdout"

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

        baseline_data = per_branch["baseline"]
        target_data = per_branch["target"]
        all_metrics = sorted(set(baseline_data["metrics"].keys()) | set(target_data["metrics"].keys()))
        comparisons: list[dict[str, Any]] = []
        for metric in all_metrics:
            for p in PERCENTILES:
                p_key = f"p{p}"
                base_val = baseline_data["metrics"].get(metric, {}).get(p_key)
                tgt_val = target_data["metrics"].get(metric, {}).get(p_key)
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
                    for stat_name in ("min", "max", "mean", "p50", "p90", "p99"):
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
