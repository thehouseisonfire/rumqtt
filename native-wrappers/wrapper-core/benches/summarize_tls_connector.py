#!/usr/bin/env python3
"""Validate the private TLS prototype matrix and apply the agreed delivery gate."""

from __future__ import annotations

import argparse
import csv
import itertools
import math
import pathlib
import re
import statistics
import sys


def summarize(source: pathlib.Path, repetitions: int, allocations: bool = False) -> list[dict[str, object]]:
    content = source.read_text()
    if not re.search(r"^test result: ok\. 1 passed; 0 failed;", content, re.MULTILINE):
        raise ValueError("the measurement process did not report successful completion")
    lines = [line.removeprefix("TLS_PROOF,") for line in content.splitlines()
             if line.startswith("TLS_PROOF,")]
    if not lines:
        raise ValueError("no prototype measurements found")
    rows = list(csv.DictReader(lines))
    groups: dict[tuple[str, str, str, str], dict[tuple[int, str], dict[str, str]]] = {}
    for row in rows:
        if None in row or None in row.values():
            raise ValueError("malformed measurement row")
        instrumented = int(row["allocation_calls"]) > 0
        if instrumented != allocations:
            raise ValueError("allocation instrumentation does not match the selected summary mode")
        key = tuple(row[field] for field in ("execution", "mqtt", "nested", "payload"))
        run = int(row["repetition"]), row["external"]
        group = groups.setdefault(key, {})
        if run in group:
            raise ValueError(f"duplicate sample: {key}, {run}")
        group[run] = row
        for field in ("messages_per_second", "p99_us"):
            if not math.isfinite(float(row[field])) or float(row[field]) <= 0:
                raise ValueError(f"invalid {field}: {key}, {run}")
        for field, limit in (("peak_bytes", 8 * 1024 * 1024), ("peak_operations", 256), ("peak_chains", 64)):
            if not 0 <= int(row[field]) <= limit:
                raise ValueError(f"invalid resource usage: {field}: {key}, {run}")
        for field in ("copied_bytes", "allocation_calls", "allocation_bytes"):
            if int(row[field]) < 0:
                raise ValueError(f"negative measurement: {field}: {key}, {run}")
        expected_chains = (2 if row["nested"] == "true" else 1) if row["external"] == "true" else 0
        if int(row["peak_chains"]) != expected_chains:
            raise ValueError(f"sample did not exercise the selected TLS composition: {key}, {run}")
    expected_groups = set(itertools.product(("dedicated", "shared"), ("4", "5"),
                                           ("false", "true"), ("1024", "16384")))
    if set(groups) != expected_groups:
        raise ValueError(f"incomplete/unexpected matrix: {set(groups) ^ expected_groups}")
    expected_runs = set(itertools.product(range(repetitions), ("false", "true")))
    result = []
    for key, group in sorted(groups.items()):
        if set(group) != expected_runs:
            raise ValueError(f"incomplete/unexpected repetitions for {key}: {set(group) ^ expected_runs}")
        native = [row for (_, external), row in group.items() if external == "false"]
        external = [row for (_, external), row in group.items() if external == "true"]
        native_rate = statistics.median(float(row["messages_per_second"]) for row in native)
        external_rate = statistics.median(float(row["messages_per_second"]) for row in external)
        native_p99 = statistics.median(float(row["p99_us"]) for row in native)
        external_p99 = statistics.median(float(row["p99_us"]) for row in external)
        throughput_ratio = external_rate / native_rate
        latency_ratio = external_p99 / native_p99
        result.append(dict(zip(("execution", "mqtt", "nested", "payload"), key)) | {
            "native_messages_per_second": round(native_rate, 3),
            "external_messages_per_second": round(external_rate, 3),
            "throughput_ratio": round(throughput_ratio, 4),
            "native_p99_us": round(native_p99, 3),
            "external_p99_us": round(external_p99, 3),
            "p99_ratio": round(latency_ratio, 4),
            "peak_bytes": max(int(row["peak_bytes"]) for row in external),
            "peak_operations": max(int(row["peak_operations"]) for row in external),
            "peak_chains": max(int(row["peak_chains"]) for row in external),
            "native_allocation_calls": statistics.median(int(row["allocation_calls"]) for row in native),
            "external_allocation_calls": statistics.median(int(row["allocation_calls"]) for row in external),
            "native_allocated_bytes": statistics.median(int(row["allocation_bytes"]) for row in native),
            "external_allocated_bytes": statistics.median(int(row["allocation_bytes"]) for row in external),
            "gate_passed": None if allocations else throughput_ratio >= 0.75 and latency_ratio <= 1.5,
        })
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=pathlib.Path)
    parser.add_argument("--repetitions", type=int, default=7)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--allocation-samples", action="store_true")
    args = parser.parse_args()
    if args.repetitions < 1:
        parser.error("repetitions must be positive")
    try:
        rows = summarize(args.source, args.repetitions, args.allocation_samples)
    except (ValueError, KeyError) as error:
        parser.error(str(error))
    stream = args.output.open("w", newline="") if args.output else sys.stdout
    try:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    finally:
        if args.output:
            stream.close()
    return 0 if args.allocation_samples or all(row["gate_passed"] for row in rows) else 1


if __name__ == "__main__":
    raise SystemExit(main())
