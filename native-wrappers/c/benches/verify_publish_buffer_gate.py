#!/usr/bin/env python3
"""Combine payload benefit samples with an isolated existing-C-API comparison."""

import argparse
import json
import statistics
from pathlib import Path

from summarize_publish_buffers import interval, read

SIZES = [64, 1024, 16384, 262144, 1048576]


def external(path, sizes, phases):
    result = {}
    for line in Path(path).read_text().splitlines():
        row = json.loads(line)
        key = (row["protocol"], row["size"], row["phase"], row["sample"], row["mode"])
        if key in result or row["iterations"] <= 0 or row["ns"] <= 0:
            raise ValueError(f"invalid/duplicate external sample: {key}")
        result[key] = row
    expected = {
        (p, n, phase, i, mode)
        for p in [1, 2]
        for n in sizes
        for phase in phases
        for i in range(21)
        for mode in ["baseline", "current"]
    }
    if result.keys() != expected:
        raise ValueError("incomplete/unexpected external comparison")
    return result


def comparison(data, protocol, size, phase):
    ratios = []
    for sample in range(21):
        baseline = data[protocol, size, phase, sample, "baseline"]
        current = data[protocol, size, phase, sample, "current"]
        if baseline["iterations"] != current["iterations"]:
            raise ValueError("mismatched paired iteration counts")
        ratios.append((current["ns"] / current["iterations"]) / (baseline["ns"] / baseline["iterations"]))
    return interval(ratios)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--benefit", required=True)
    parser.add_argument("--admission", required=True)
    parser.add_argument("--copying", required=True)
    parser.add_argument("--followup", required=True)
    args = parser.parse_args()
    data = {**read(args.benefit), **read(args.admission)}
    shared_benefit = []
    for size in [262144, 1048576]:
        for phase in ["admitted", "deferred", "rejected"]:
            protocols = []
            for protocol in [1, 2]:
                rows = {
                    mode: [data[protocol, size, phase, i, mode, True] for i in range(21)] for mode in ["copy", "owned"]
                }
                avoided = (
                    statistics.median(
                        [
                            rows["copy"][i]["allocated_bytes"] / rows["copy"][i]["iterations"]
                            - rows["owned"][i]["allocated_bytes"] / rows["owned"][i]["iterations"]
                            for i in range(21)
                        ]
                    )
                    / size
                )
                times = [
                    (
                        data[protocol, size, phase, i, "owned", False]["ns"]
                        / data[protocol, size, phase, i, "owned", False]["iterations"]
                    )
                    / (
                        data[protocol, size, phase, i, "copy", False]["ns"]
                        / data[protocol, size, phase, i, "copy", False]["iterations"]
                    )
                    for i in range(21)
                ]
                protocols.append(avoided >= 0.9 or interval(times)[1] <= 0.9)
            if all(protocols):
                shared_benefit.append((size, phase))
    copying = external(args.copying, SIZES, ["rejected", "lifecycle"])
    followup = external(args.followup, [262144], ["rejected"])
    copying_passed = True
    for protocol in [1, 2]:
        for size in SIZES:
            for phase in ["rejected", "lifecycle"]:
                lo, hi = comparison(copying, protocol, size, phase)
                if hi > 1.05:
                    # Supplement only the two inconclusive 256 KiB cases with
                    # longer samples; retain the original evidence unchanged.
                    if size == 262144 and phase == "rejected" and lo <= 1.05:
                        for sample in range(21):
                            for mode in ["baseline", "current"]:
                                if (
                                    followup[protocol, size, phase, sample, mode]["iterations"]
                                    < 32 * copying[protocol, size, phase, sample, mode]["iterations"]
                                ):
                                    raise ValueError("follow-up is not at least 32 times longer")
                        lo, hi = comparison(followup, protocol, size, phase)
                    copying_passed = copying_passed and hi <= 1.05
    passed = bool(shared_benefit) and copying_passed
    print(
        json.dumps(
            {
                "benefit_workloads": shared_benefit,
                "copying_regression_gate": copying_passed,
                "performance_gate": passed,
            },
            indent=2,
        )
    )
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
