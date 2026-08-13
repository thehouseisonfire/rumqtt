#!/usr/bin/env python3
"""Require explicit success/failure NULL-error markers for the C API."""

from __future__ import annotations

import pathlib
import re
import sys

root = pathlib.Path(__file__).resolve().parents[2]
header = (root / "include" / "rumqttc.h").read_text(encoding="utf-8")
source = (pathlib.Path(__file__).parent / "error_out_contract.c").read_text(encoding="utf-8")
functions = set(
    re.findall(
        r"RUMQTTC_API\s+[^;]*?\b(rumqttc_[a-z0-9_]+)\s*\([^;]*?rumqttc_error_t\s*\*\*\s*error_out\b[^;]*\);",
        header,
        flags=re.DOTALL,
    )
)
missing: list[str] = []
for function in sorted(functions):
    for outcome in ("SUCCESS", "FAILURE"):
        if f"ERROR_OUT_{outcome}: {function}" not in source:
            missing.append(f"{function}: missing {outcome.lower()} NULL-error coverage")
if missing:
    print("\n".join(missing), file=sys.stderr)
    raise SystemExit(1)
print(f"covered {len(functions)} optional error outputs on success and failure")
