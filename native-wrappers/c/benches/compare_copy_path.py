#!/usr/bin/env python3
"""Use one compiled external C producer against two libraries, alternating order."""

import argparse
import json
import os
import random
import socket
import statistics
import subprocess
import threading
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--producer", required=True)
    parser.add_argument("--baseline", required=True)
    parser.add_argument("--current", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--size", type=int, choices=[64, 1024, 16384, 262144, 1048576])
    parser.add_argument("--phase", choices=["rejected", "lifecycle"])
    parser.add_argument("--iterations", type=int)
    args = parser.parse_args()
    if hasattr(os, "sched_getaffinity"):
        os.sched_setaffinity(0, {min(os.sched_getaffinity(0))})
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen()
    listener.settimeout(0.1)
    stopped = threading.Event()

    def peer(stream):
        with stream:
            while stream.recv(65536):
                pass

    def accept():
        while not stopped.is_set():
            try:
                stream, _ = listener.accept()
            except TimeoutError:
                continue
            threading.Thread(target=peer, args=(stream,), daemon=True).start()

    accept_thread = threading.Thread(target=accept)
    accept_thread.start()
    rows = []
    try:
        with Path(args.output).open("w") as output:
            for protocol in [1, 2]:
                for size, iterations in [(64, 65536), (1024, 16384), (16384, 4096), (262144, 256), (1048576, 128)]:
                    if args.size and size != args.size:
                        continue
                    iterations = args.iterations or iterations
                    for phase in ["rejected", "lifecycle"]:
                        if args.phase and phase != args.phase:
                            continue
                        for sample in range(21):
                            for mode in ["baseline", "current"] if sample % 2 == 0 else ["current", "baseline"]:
                                command = [
                                    args.producer,
                                    getattr(args, mode),
                                    str(listener.getsockname()[1]),
                                    str(protocol),
                                    str(size),
                                    phase,
                                    str(iterations),
                                    str(sample),
                                ]
                                row = json.loads(subprocess.check_output(command, text=True, timeout=15))
                                row["mode"] = mode
                                rows.append(row)
                                output.write(json.dumps(row) + "\n")
                                output.flush()
    finally:
        stopped.set()
        accept_thread.join()
        listener.close()
    passed = True
    for protocol in [1, 2]:
        for size in [64, 1024, 16384, 262144, 1048576]:
            for phase in ["rejected", "lifecycle"]:
                if (args.size and size != args.size) or (args.phase and phase != args.phase):
                    continue
                selected = {
                    (r["mode"], r["sample"]): r["ns"] / r["iterations"]
                    for r in rows
                    if (r["protocol"], r["size"], r["phase"]) == (protocol, size, phase)
                }
                ratios = [selected["current", i] / selected["baseline", i] for i in range(21)]
                rng = random.Random(51)
                boot = sorted(statistics.median(rng.choices(ratios, k=21)) for _ in range(10000))
                lo, hi = boot[249], boot[9749]
                passed = passed and hi <= 1.05
                print(
                    f"v{protocol + 3} {size:7} {phase:9} current/baseline={statistics.median(ratios):.3f} "
                    f"95%=[{lo:.3f},{hi:.3f}]"
                )
    print(f"copying_regression_gate={passed}")
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
