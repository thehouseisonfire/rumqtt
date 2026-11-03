#!/usr/bin/env python3
"""Validate complete TODO51 samples and report paired timing/allocation costs."""

import argparse
import json
import random
import statistics
from pathlib import Path


def read(path):
    rows = [json.loads(line) for line in Path(path).read_text().splitlines()]
    result = {}
    for r in rows:
        key = (r["protocol"], r["size"], r["phase"], r["sample"], r["mode"], r["instrumented"])
        if key in result or r["iterations"] <= 0 or r["ns"] <= 0:
            raise ValueError(f"invalid/duplicate sample: {key}")
        result[key] = r
    phases = {r["phase"] for r in rows}
    if phases not in ({"build", "rejected", "lifecycle"}, {"admitted", "deferred"}):
        raise ValueError("unknown phase matrix")
    expected = {
        (p, n, phase, sample, mode, alloc)
        for p in [1, 2]
        for n in [64, 1024, 16384, 262144, 1048576]
        for phase in phases
        for sample in range(21)
        for mode in ["copy", "owned"]
        for alloc in [False, True]
    }
    if result.keys() != expected:
        raise ValueError("incomplete/unexpected measurement matrix")
    return result


def ratios(data, baseline, p, n, phase, mode="owned"):
    return [
        (data[p, n, phase, i, mode, False]["ns"] / data[p, n, phase, i, mode, False]["iterations"])
        / (baseline[p, n, phase, i, "copy", False]["ns"] / baseline[p, n, phase, i, "copy", False]["iterations"])
        for i in range(21)
    ]


def interval(values):
    rng = random.Random(51)
    samples = sorted(statistics.median(rng.choices(values, k=len(values))) for _ in range(10000))
    return samples[249], samples[9749]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("results")
    parser.add_argument("--baseline")
    args = parser.parse_args()
    data = read(args.results)
    baseline = read(args.baseline) if args.baseline else data
    benefit = {1: False, 2: False}
    copying = True
    for p in [1, 2]:
        for n in [64, 1024, 16384, 262144, 1048576]:
            for phase in sorted({k[2] for k in data}):
                times = ratios(data, data, p, n, phase)
                lo, hi = interval(times)
                allocated = {}
                for mode in ["copy", "owned"]:
                    allocated[mode] = statistics.median(
                        data[p, n, phase, i, mode, True]["allocated_bytes"]
                        / data[p, n, phase, i, mode, True]["iterations"]
                        for i in range(21)
                    )
                avoided = (allocated["copy"] - allocated["owned"]) / n
                # The public gate uses real native rejected admission, not only a
                # constructor microbenchmark or a claim about successful delivery.
                if n >= 262144 and phase in ["rejected", "admitted", "deferred"] and (hi <= 0.9 or avoided >= 0.9):
                    benefit[p] = True
                copy_ci = interval(ratios(data, baseline, p, n, phase, "copy"))
                if args.baseline and copy_ci[1] > 1.05:
                    copying = False
                print(
                    f"v{p + 3} {n:7} {phase:9} owned/copy={statistics.median(times):.3f} "
                    f"95%=[{lo:.3f},{hi:.3f}] bytes={allocated['copy']:.0f}/{allocated['owned']:.0f} "
                    f"copy/baseline95%=[{copy_ci[0]:.3f},{copy_ci[1]:.3f}]"
                )
    print(f"benefit={benefit} copying_regression_gate={copying}")
    return 0 if all(benefit.values()) and copying else 1


if __name__ == "__main__":
    raise SystemExit(main())
