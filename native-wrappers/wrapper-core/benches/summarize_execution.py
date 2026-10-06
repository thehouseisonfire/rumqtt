"""Preserve per-run resources, rates and latency distributions in a portable CSV."""

from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    rows = []
    for path in sorted(args.input.glob("*.json")):
        run = json.loads(path.read_text())
        if "case" not in run:
            continue
        row = {
            key: run[key]
            for key in [
                "consumer",
                "mode",
                "clients",
                "protocol",
                "qos",
                "scenario",
                "transport",
                "repetition",
                "status",
            ]
        }
        row["error"] = run.get("error", run.get("startup", {}).get("error", ""))
        row["started_clients"] = run.get("startup", {}).get("started_clients", run["clients"])
        row["startup_ns"] = run.get("startup", {}).get("startup_ns", "")
        for phase in ["connected", "idle", "after_teardown"]:
            for field in ["rss_bytes", "virtual_bytes", "threads", "cpu_seconds"]:
                row[f"{phase}_{field}"] = run.get(phase, {}).get(field, "")
        measurements = run.get("measurements", {})
        for field in ["elapsed_ns", "teardown_ns"]:
            row[field] = measurements.get(field, "")
        elapsed = measurements.get("elapsed_ns", 0)
        for name in ["admission_ns", "completion_ns", "event_ns", "loop_delay_ns"]:
            values = measurements.get(name, [])
            for field in ["samples", "p50_ns", "p95_ns", "p99_ns", "max_ns"]:
                row[f"{name}_{field}"] = run.get("distributions", {}).get(name, {}).get(field, "")
            if name != "loop_delay_ns":
                row[f"{name}_observations_per_second"] = len(values) * 1e9 / elapsed if elapsed else ""
        rows.append(row)
    if not rows:
        parser.error("No measurement cases found")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", newline="") as output:
        writer = csv.DictWriter(output, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


if __name__ == "__main__":
    main()
