#!/usr/bin/env python3
"""Dependency-free memory-stability acceptance calculations."""

from __future__ import annotations

import argparse
import csv
import json
import math
import statistics
import sys
from pathlib import Path
from typing import Any, Sequence

MIB = 1024 * 1024
BASELINE_ROUNDS = 5
ENDING_ROUNDS = 5
MIN_ROUNDS = BASELINE_ROUNDS + ENDING_ROUNDS
ROUND_SETTLE_MS = 250
ROUND_SAMPLE_COUNT = 5
ROUND_SAMPLE_WINDOW_MS = 850


class ReportError(ValueError):
    pass


def median(values: Sequence[int]) -> float:
    if not values:
        raise ReportError("cannot compute a median from empty input")
    return float(statistics.median(values))


def analyze_values(values: Sequence[int]) -> dict[str, float]:
    if len(values) < MIN_ROUNDS:
        raise ReportError(f"at least {MIN_ROUNDS} measured rounds are required")
    if any(not isinstance(value, int) or isinstance(value, bool) or value < 0 for value in values):
        raise ReportError("round memory values must be non-negative integers")

    baseline = median(values[:BASELINE_ROUNDS])
    ending = median(values[-ENDING_ROUNDS:])
    if baseline <= 0:
        raise ReportError("post-warm-up baseline must be positive")

    count = len(values)
    x_mean = (count + 1) / 2.0
    y_mean = sum(values) / count
    denominator = sum((index - x_mean) ** 2 for index in range(1, count + 1))
    slope = (
        sum(
            (index - x_mean) * (value - y_mean)
            for index, value in enumerate(values, start=1)
        )
        / denominator
    )
    growth = ending - baseline
    allowed_growth = max(float(MIB), baseline * 0.10)
    projected_trend = max(0.0, slope) * (count - 1)
    return {
        "baseline_bytes": baseline,
        "ending_bytes": ending,
        "growth_bytes": growth,
        "growth_percent": growth * 100.0 / baseline,
        "slope_bytes_per_round": slope,
        "projected_positive_trend_bytes": projected_trend,
        "allowed_growth_bytes": allowed_growth,
    }


def read_round_values(path: Path) -> tuple[list[int], list[dict[str, str]]]:
    try:
        with path.open(newline="", encoding="utf-8") as source:
            rows = list(csv.DictReader(source))
    except (OSError, csv.Error) as error:
        raise ReportError(f"cannot read rounds CSV: {error}") from error
    if not rows:
        raise ReportError("rounds CSV is empty")
    required = {"round", "median_bytes", "sample_count"}
    if not required.issubset(rows[0]):
        raise ReportError("rounds CSV is missing required columns")

    values: list[int] = []
    expected_round = 1
    for row in rows:
        try:
            round_number = int(row["round"])
            value = int(row["median_bytes"])
            sample_count = int(row["sample_count"])
        except (TypeError, ValueError) as error:
            raise ReportError("rounds CSV contains a malformed integer") from error
        if round_number != expected_round:
            raise ReportError("rounds CSV must contain consecutive rounds starting at 1")
        if sample_count < ROUND_SAMPLE_COUNT:
            raise ReportError(
                f"round {round_number} has {sample_count} samples; "
                f"{ROUND_SAMPLE_COUNT} are required"
            )
        if value < 0:
            raise ReportError("round memory values must be non-negative")
        values.append(value)
        expected_round += 1
    return values, rows


def extract_rounds(samples_path: Path, boundaries_path: Path, output_path: Path) -> None:
    try:
        with samples_path.open(newline="", encoding="utf-8") as source:
            samples = list(csv.DictReader(source))
        with boundaries_path.open(newline="", encoding="utf-8") as source:
            boundaries = list(csv.DictReader(source))
    except (OSError, csv.Error) as error:
        raise ReportError(f"cannot read sampling input: {error}") from error
    if not samples or not boundaries:
        raise ReportError("sampling input is empty")

    parsed_samples: list[tuple[int, int]] = []
    for sample in samples:
        try:
            parsed_samples.append(
                (int(sample["elapsed_ms"]), int(sample["memory_current_bytes"]))
            )
        except (KeyError, TypeError, ValueError) as error:
            raise ReportError("memory-current CSV is malformed") from error

    previous_client_elapsed: int | None = None
    measured_rows: list[dict[str, int | float]] = []
    expected_measured_round = 1
    for boundary in boundaries:
        try:
            kind = boundary["kind"]
            round_number = int(boundary["round"])
            detected = int(boundary["detected_elapsed_ms"])
            client_elapsed = int(boundary["client_elapsed_ms"])
        except (KeyError, TypeError, ValueError) as error:
            raise ReportError("boundary CSV is malformed") from error

        round_duration = (
            0 if previous_client_elapsed is None else client_elapsed - previous_client_elapsed
        )
        previous_client_elapsed = client_elapsed
        if kind != "measured":
            continue
        if round_number != expected_measured_round:
            raise ReportError("measured boundary rounds are missing or out of order")

        selected = [
            value
            for elapsed, value in parsed_samples
            if detected + ROUND_SETTLE_MS
            <= elapsed
            <= detected + ROUND_SAMPLE_WINDOW_MS
        ][:ROUND_SAMPLE_COUNT]
        if len(selected) < ROUND_SAMPLE_COUNT:
            raise ReportError(
                f"measured round {round_number} has only {len(selected)} settled samples"
            )
        measured_rows.append(
            {
                "round": round_number,
                "median_bytes": int(median(selected)),
                "sample_count": len(selected),
                "boundary_elapsed_ms": client_elapsed,
                "round_duration_ms": round_duration,
            }
        )
        expected_measured_round += 1

    if not measured_rows:
        raise ReportError("no measured round boundaries were recorded")
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("w", newline="", encoding="utf-8") as destination:
        writer = csv.DictWriter(destination, fieldnames=measured_rows[0].keys())
        writer.writeheader()
        writer.writerows(measured_rows)


def classify_run(args: argparse.Namespace) -> dict[str, Any]:
    analysis: dict[str, float] | None = None
    analysis_error: str | None = None
    rows: list[dict[str, str]] = []
    try:
        values, rows = read_round_values(Path(args.rounds))
        analysis = analyze_values(values)
    except ReportError as error:
        analysis_error = str(error)

    memory_failure = (
        args.oom_killed
        or args.exit_code == 137
        or args.oom_events > 0
        or args.oom_kill_events > 0
        or args.max_events > 0
        or args.peak_bytes > args.memory_limit_bytes
    )
    functional_failure = (
        not args.scenario_success
        or not args.restart_success
        or args.exit_code != 0
        or args.completed_messages != args.expected_messages
        or args.completed_reconnects != args.expected_reconnects
        or args.completed_cycles != args.expected_cycles
        or args.idle_boundaries != args.expected_idle_boundaries
    )

    if memory_failure:
        result = "Fail — memory"
    elif functional_failure:
        result = "Fail — functional"
    elif analysis_error is not None:
        result = "Inconclusive"
    elif (
        analysis["growth_bytes"] > analysis["allowed_growth_bytes"]
        or analysis["projected_positive_trend_bytes"]
        > analysis["allowed_growth_bytes"]
    ):
        result = "Fail — growth"
    else:
        result = "Pass"

    throughput = (
        args.completed_messages * 1000.0 / args.duration_ms
        if args.duration_ms > 0
        else None
    )
    payload: dict[str, Any] = {
        "client": args.protocol,
        "run": args.run,
        "profile": args.profile,
        "result": result,
        "analysis_error": analysis_error,
        "metrics": analysis,
        "peak_bytes": args.peak_bytes,
        "final_memory_bytes": args.final_bytes,
        "memory_limit_bytes": args.memory_limit_bytes,
        "oom_killed": args.oom_killed,
        "memory_events": {
            "max": args.max_events,
            "oom": args.oom_events,
            "oom_kill": args.oom_kill_events,
        },
        "exit_code": args.exit_code,
        "duration_ms": args.duration_ms,
        "completed_messages": args.completed_messages,
        "expected_messages": args.expected_messages,
        "messages_per_second": throughput,
        "completed_reconnects": args.completed_reconnects,
        "expected_reconnects": args.expected_reconnects,
        "completed_subscribe_unsubscribe_cycles": args.completed_cycles,
        "expected_subscribe_unsubscribe_cycles": args.expected_cycles,
        "idle_boundaries": args.idle_boundaries,
        "expected_idle_boundaries": args.expected_idle_boundaries,
        "rounds": rows,
    }
    return payload


def write_run_report(payload: dict[str, Any], json_path: Path, text_path: Path) -> None:
    json_path.parent.mkdir(parents=True, exist_ok=True)
    json_path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    metrics = payload["metrics"] or {}
    line = (
        f"{payload['client']} run={payload['run']} {payload['result']} "
        f"baseline={format_number(metrics.get('baseline_bytes'))} "
        f"ending={format_number(metrics.get('ending_bytes'))} "
        f"growth={format_number(metrics.get('growth_bytes'))} "
        f"growth_pct={format_number(metrics.get('growth_percent'))} "
        f"projected_trend={format_number(metrics.get('projected_positive_trend_bytes'))} "
        f"peak={payload['peak_bytes']} final={payload['final_memory_bytes']} "
        f"oom_killed={str(payload['oom_killed']).lower()} "
        f"messages_per_second={format_number(payload['messages_per_second'])}\n"
    )
    text_path.write_text(line, encoding="utf-8")


def format_number(value: Any) -> str:
    if value is None:
        return "unavailable"
    if isinstance(value, float):
        return f"{value:.3f}"
    return str(value)


def aggregate(paths: Sequence[Path], json_path: Path, csv_path: Path, text_path: Path) -> bool:
    if not paths:
        raise ReportError("no run reports were supplied")
    runs = [json.loads(path.read_text(encoding="utf-8")) for path in paths]
    clients: dict[str, list[dict[str, Any]]] = {}
    for run in runs:
        clients.setdefault(run["client"], []).append(run)

    overall: dict[str, Any] = {}
    for client, client_runs in clients.items():
        passing = sum(run["result"] == "Pass" for run in client_runs)
        overall[client] = {
            "result": "Pass" if passing == len(client_runs) else "Fail",
            "passing_runs": passing,
            "total_runs": len(client_runs),
        }
    payload = {"runs": runs, "overall": overall}
    json_path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    fields = [
        "client",
        "run",
        "profile",
        "result",
        "baseline_bytes",
        "ending_bytes",
        "growth_bytes",
        "growth_percent",
        "projected_positive_trend_bytes",
        "allowed_growth_bytes",
        "peak_bytes",
        "final_memory_bytes",
        "oom_killed",
        "oom_events",
        "oom_kill_events",
        "duration_ms",
        "completed_messages",
        "messages_per_second",
        "completed_reconnects",
        "completed_subscribe_unsubscribe_cycles",
    ]
    with csv_path.open("w", newline="", encoding="utf-8") as destination:
        writer = csv.DictWriter(destination, fieldnames=fields)
        writer.writeheader()
        for run in runs:
            metrics = run["metrics"] or {}
            writer.writerow(
                {
                    **{field: run.get(field) for field in fields},
                    **{field: metrics.get(field) for field in fields if field in metrics},
                    "oom_events": run["memory_events"]["oom"],
                    "oom_kill_events": run["memory_events"]["oom_kill"],
                }
            )

    lines = [
        (
            f"{run['client']} run={run['run']} {run['result']} "
            f"growth={format_number((run['metrics'] or {}).get('growth_bytes'))} "
            f"trend={format_number((run['metrics'] or {}).get('projected_positive_trend_bytes'))} "
            f"peak={run['peak_bytes']} messages/s={format_number(run['messages_per_second'])}"
        )
        for run in runs
    ]
    lines.extend(
        f"{client} overall={item['result']} passing_runs={item['passing_runs']}/{item['total_runs']}"
        for client, item in sorted(overall.items())
    )
    text_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return all(item["result"] == "Pass" for item in overall.values())


def bool_value(value: str) -> bool:
    lowered = value.lower()
    if lowered in {"true", "1", "yes"}:
        return True
    if lowered in {"false", "0", "no"}:
        return False
    raise argparse.ArgumentTypeError("expected true or false")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    commands = root.add_subparsers(dest="command", required=True)

    rounds = commands.add_parser("extract-rounds")
    rounds.add_argument("--samples", required=True)
    rounds.add_argument("--boundaries", required=True)
    rounds.add_argument("--output", required=True)

    run = commands.add_parser("analyze-run")
    run.add_argument("--rounds", required=True)
    run.add_argument("--output-json", required=True)
    run.add_argument("--output-text", required=True)
    run.add_argument("--protocol", required=True, choices=["v4", "v5"])
    run.add_argument("--run", required=True, type=int)
    run.add_argument("--profile", required=True, choices=["official", "diagnostic"])
    for name in (
        "peak-bytes",
        "final-bytes",
        "memory-limit-bytes",
        "exit-code",
        "duration-ms",
        "completed-messages",
        "expected-messages",
        "completed-reconnects",
        "expected-reconnects",
        "completed-cycles",
        "expected-cycles",
        "idle-boundaries",
        "expected-idle-boundaries",
        "max-events",
        "oom-events",
        "oom-kill-events",
    ):
        run.add_argument(f"--{name}", required=True, type=int)
    run.add_argument("--oom-killed", required=True, type=bool_value)
    run.add_argument("--scenario-success", required=True, type=bool_value)
    run.add_argument("--restart-success", required=True, type=bool_value)

    aggregate_parser = commands.add_parser("aggregate")
    aggregate_parser.add_argument("--output-json", required=True)
    aggregate_parser.add_argument("--output-csv", required=True)
    aggregate_parser.add_argument("--output-text", required=True)
    aggregate_parser.add_argument("reports", nargs="+")
    return root


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        if args.command == "extract-rounds":
            extract_rounds(Path(args.samples), Path(args.boundaries), Path(args.output))
            return 0
        if args.command == "analyze-run":
            payload = classify_run(args)
            write_run_report(
                payload, Path(args.output_json), Path(args.output_text)
            )
            return 0 if payload["result"] == "Pass" else 1
        passed = aggregate(
            [Path(path) for path in args.reports],
            Path(args.output_json),
            Path(args.output_csv),
            Path(args.output_text),
        )
        return 0 if passed else 1
    except (ReportError, OSError, csv.Error, json.JSONDecodeError) as error:
        print(f"memory-stability report error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
