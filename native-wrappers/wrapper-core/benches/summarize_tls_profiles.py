#!/usr/bin/env python3
"""Summarize whole-process TLS profiles; sample buckets are not isolated causal costs."""

from __future__ import annotations

import argparse
import collections
import csv
import hashlib
import itertools
import json
import math
import pathlib
import re
import subprocess


def samples(path: pathlib.Path) -> list[tuple[int, str, str]]:
    content = path.read_text()
    if "# Total Lost Samples: 0\n" not in content:
        raise ValueError(f"lost-sample accounting is absent or nonzero: {path}")
    result = []
    for line in content.splitlines():
        if re.match(r"^\s*[\d.]+%\s*;", line):
            fields = [field.strip() for field in line.split(";")]
            # Preserve semicolons inside Rust type names; the last field is IPC.
            result.append((int(fields[1]), ";".join(fields[2:-2]).removeprefix("[.] "), fields[-2]))
    if sum(count for count, _, _ in result) < 100:
        raise ValueError(f"too few CPU samples: {path}")
    return result


def bucket(symbol: str, dso: str) -> str:
    if re.search(r"memcpy|memmove|memset|bzero|copy_nonoverlapping", symbol):
        return "memory_copy_fill"
    if re.search(r"malloc|calloc|realloc|cfree|_int_free|(?:^|::)free$|alloc::alloc", symbol):
        return "allocator"
    if "tls_connector_proof::" in symbol and not any(
        name in symbol for name in ("::sample", "::puback", "::mqtt_packet", "::profile_external", "::Fixture")
    ):
        return "bridge"
    if re.search(r"tokio::(?:runtime|sync|task)|flume::|futures_|parking_lot|pthread_|__lll_lock|futex", symbol):
        return "runtime_channels_locks"
    if "syscall" in symbol:
        return "syscall_stubs"
    if re.search(r"libssl|libcrypto", dso) or re.search(r"aes|ghash|chacha|poly1305|rustls|aws_lc|proof_tls_", symbol):
        return "tls_crypto"
    if symbol.startswith("0x") or symbol == "[unknown]":
        return "unresolved"
    return "other"


def validate_workload(path: pathlib.Path, run: dict, metadata: dict) -> None:
    content = path.read_text()
    if not re.search(r"^test result: ok\. 1 passed; 0 failed;", content, re.MULTILINE):
        raise ValueError(f"profiling process did not complete successfully: {path}")
    rows = list(csv.DictReader(line.removeprefix("TLS_PROFILE,") for line in content.splitlines()
                              if line.startswith("TLS_PROFILE,")))
    environment = run["environment"]
    expected = {name: environment[f"RUMQTTC_TLS_PROOF_PROFILE_{name.upper()}"]
                for name in ("execution", "mqtt", "nested", "payload", "external")}
    expected["messages"] = str(metadata["messages"])
    if len(rows) != 1 or any(rows[0].get(name) != value for name, value in expected.items()):
        raise ValueError(f"profiling workload does not match the recorded scenario: {path}")
    for name in ("messages_per_second", "p99_us"):
        value = float(rows[0][name])
        if not math.isfinite(value) or value <= 0:
            raise ValueError(f"invalid profiling metric: {path}: {name}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=pathlib.Path)
    parser.add_argument("--data", type=pathlib.Path,
                        help="read perf.data build IDs and retain text sidecars; omit to reuse retained sidecars")
    parser.add_argument("--libc-debug", type=pathlib.Path, required=True)
    args = parser.parse_args()
    metadata = json.loads((args.source / "environment.json").read_text())
    expected = {f"{mode}-{case}" for mode, case in itertools.product(
        ("native", "threaded", "inline"), ("single-1024", "nested-1024", "nested-16384"))}
    labels = [run["label"] for run in metadata["runs"]]
    if len(labels) != len(expected) or set(labels) != expected:
        raise ValueError("incomplete or duplicate profiling matrix")
    notes = subprocess.run(["readelf", "-n", str(args.libc_debug)],
                           text=True, capture_output=True, check=True).stdout
    build_id = re.search(r"Build ID: ([0-9a-f]+)", notes).group(1)
    profiles = {label: samples(args.source / f"{label}-report.txt") for label in labels}
    for run in metadata["runs"]:
        for phase in ("stat", "record"):
            validate_workload(args.source / f"{run['label']}-{phase}-workload.txt", run, metadata)
    for label in labels:
        sidecar = args.source / f"{label}-buildids.txt"
        if args.data:
            ids = subprocess.check_output(["perf", "buildid-list", "-i", str(args.data / f"{label}.data")], text=True)
            sidecar.write_text(ids)
        else:
            ids = sidecar.read_text()
        if not any(line.split()[0] == build_id and line.endswith("/libc.so.6") for line in ids.splitlines()):
            raise ValueError(f"libc debug symbols do not match the recorded build ID: {label}")
    addresses = sorted({symbol for rows in profiles.values() for _, symbol, dso in rows
                        if dso == "libc.so.6" and symbol.startswith("0x")})
    resolved = subprocess.check_output(
        ["addr2line", "-f", "-e", str(args.libc_debug), *addresses], text=True).splitlines()
    if len(resolved) != len(addresses) * 2:
        raise ValueError("incomplete libc symbol resolution")
    symbols = {address: resolved[index * 2] for index, address in enumerate(addresses)}
    with (args.source / "libc-symbols.csv").open("w", newline="") as stream:
        writer = csv.writer(stream, lineterminator="\n")
        writer.writerow(("address", "symbol", "source"))
        writer.writerows((address, symbols[address], resolved[index * 2 + 1])
                         for index, address in enumerate(addresses))
    summary = []
    buckets = ("bridge", "runtime_channels_locks", "allocator", "memory_copy_fill",
               "syscall_stubs", "tls_crypto", "unresolved", "other")
    for run in metadata["runs"]:
        label = run["label"]
        counts = collections.Counter()
        for count, symbol, dso in profiles[label]:
            if dso == "libc.so.6" and symbols.get(symbol, "??") != "??":
                symbol = symbols[symbol]
            counts[bucket(symbol, dso)] += count
        total = sum(counts.values())
        usage = run["stat_process_usage_including_perf"]
        messages = metadata["messages"]
        summary.append({
            "profile": label,
            "process_cpu_us_per_message": round((usage["ru_utime"] + usage["ru_stime"]) * 1e6 / messages, 3),
            "voluntary_switches_per_message": round(usage["ru_nvcsw"] / messages, 5),
            "involuntary_switches_per_message": round(usage["ru_nivcsw"] / messages, 5),
            "cpu_samples": total,
        } | {f"{name}_sample_pct": round(counts[name] * 100 / total, 3) for name in buckets})
    with (args.source / "summary.csv").open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(summary[0]), lineterminator="\n")
        writer.writeheader()
        writer.writerows(summary)
    analysis = {
        "summary_script_sha256": hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),
        "libc_build_id": build_id,
        "libc_debug_sha256": hashlib.sha256(args.libc_debug.read_bytes()).hexdigest(),
        "libc_debug_path": str(args.libc_debug.resolve()),
        "limitations": (
            "Flat sample labels include inlined work; CPU and switches include perf, setup, peer and client. "
            "Buckets do not isolate causal costs. Profile timings do not establish the acceptance gate."
        ),
    }
    (args.source / "analysis.json").write_text(json.dumps(analysis, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
