#!/usr/bin/env python3
"""Fail closed when a Linux wheel exceeds its claimed auditwheel policy."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

POLICY_PATTERN = re.compile(r"^(manylinux|musllinux)_(\d+)_(\d+)_(.+)$")


@dataclass(frozen=True)
class LinuxPolicy:
    family: str
    version: tuple[int, int]
    architecture: str


def parse_policy(value: str) -> LinuxPolicy:
    match = POLICY_PATTERN.fullmatch(value)
    if match is None:
        raise ValueError(f"unsupported Linux wheel policy: {value}")
    family, major, minor, architecture = match.groups()
    return LinuxPolicy(family, (int(major), int(minor)), architecture)


def policy_is_compatible(computed: str, required: str) -> bool:
    required_policy = parse_policy(required)
    try:
        computed_policy = parse_policy(computed)
    except ValueError:
        # auditwheel uses linux_<arch> when external dependencies, symbols, or ISA requirements
        # prevent compatibility with every manylinux/musllinux policy.
        return False
    return (
        computed_policy.family == required_policy.family
        and computed_policy.architecture == required_policy.architecture
        and computed_policy.version <= required_policy.version
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("wheel", type=Path)
    parser.add_argument("--policy", required=True)
    arguments = parser.parse_args()

    result = subprocess.run(
        [sys.executable, "-m", "auditwheel", "show", "--json", str(arguments.wheel)],
        check=False,
        text=True,
        capture_output=True,
        timeout=60,
    )
    if result.returncode != 0:
        sys.stderr.write(result.stdout)
        sys.stderr.write(result.stderr)
        return result.returncode

    try:
        report = json.loads(result.stdout)
        computed_policy = report["overall_tag"]
        if not isinstance(computed_policy, str):
            raise TypeError("overall_tag is not a string")
    except (json.JSONDecodeError, KeyError, TypeError) as error:
        sys.stderr.write(result.stdout)
        sys.stderr.write(result.stderr)
        raise SystemExit(f"auditwheel returned an invalid JSON report: {error}") from error

    if not policy_is_compatible(computed_policy, arguments.policy):
        raise SystemExit(
            f"{arguments.wheel.name} is not compatible with required policy {arguments.policy}; "
            f"auditwheel reported {computed_policy}"
        )

    # overall_tag accounts for versioned libc symbols, ISA requirements, and the policy's external
    # shared-library allowlist. An older baseline is valid because it is more portable.
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
