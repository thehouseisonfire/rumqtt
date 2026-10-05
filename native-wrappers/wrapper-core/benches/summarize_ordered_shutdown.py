#!/usr/bin/env python3
"""Summarize timing repetitions separately from instrumented allocation samples."""
from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path
import statistics


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    groups: dict[tuple, list[dict]] = {}
    metadata = None
    for line in args.input.read_text().splitlines():
        row = json.loads(line)
        if "metadata" in row:
            metadata = row["metadata"]
            continue
        key = tuple(row[name] for name in ("protocol", "qos", "producers", "capacity", "inflight", "profile"))
        groups.setdefault(key, []).append(row)
    assert metadata is not None
    profiles = {key[-1] for key in groups}
    expected_profiles = {"disabled", "enabled-unused", "enabled-ordered"}
    if metadata.get("baseline"):
        expected_profiles.add("baseline-disabled")
    assert profiles == expected_profiles, "incomplete profile matrix"
    assert len(groups) == metadata["configurations"] * len(profiles), "incomplete configuration matrix"
    summaries = []
    for key, rows in sorted(groups.items()):
        timings = [row for row in rows if not row["allocation_tracking"]]
        allocations = [row for row in rows if row["allocation_tracking"]]
        assert len(timings) == metadata["repetitions"], f"incomplete timing repetitions: {key}"
        assert {row["repetition"] for row in timings} == set(range(metadata["repetitions"])), key
        assert len(allocations) == metadata["allocation_repetitions"], f"incomplete allocation samples: {key}"
        summary = dict(zip(("protocol", "qos", "producers", "capacity", "inflight", "profile"), key))
        summary["timing_samples"] = len(timings)
        for metric in ("admissions_per_second", "completed_per_second", "shutdown_ms"):
            values = [row[metric] for row in timings]
            for label, value in (("min", min(values)), ("median", statistics.median(values)), ("max", max(values))):
                summary[f"{metric}_{label}"] = round(value, 3)
        for metric in ("p50_us", "p95_us", "p99_us"):
            summary[f"{metric}_median"] = round(statistics.median(row[metric] for row in timings), 3)
        for metric in ("allocations_per_message", "allocated_bytes_per_message"):
            summary[metric] = round(statistics.median(row[metric] for row in allocations), 3)
        summaries.append(summary)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", newline="") as output:
        writer = csv.DictWriter(output, fieldnames=list(summaries[0]), lineterminator="\n")
        writer.writeheader()
        writer.writerows(summaries)
    print(f"wrote {len(summaries)} summaries to {args.output}")


if __name__ == "__main__":
    main()
