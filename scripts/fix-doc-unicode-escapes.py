#!/usr/bin/env python3
from __future__ import annotations

import argparse
import re
from pathlib import Path

UNICODE_ESCAPE = re.compile(
    r"""
    \\u([0-9a-fA-F]{4})      # first \uXXXX
    (?:
      \\u([0-9a-fA-F]{4})    # optional second \uXXXX for surrogate pairs
    )?
    """,
    re.VERBOSE,
)


DOC_FILES = [
    Path("docs/spec/mqtt-v3.1.1.requirements.json"),
    Path("docs/spec/mqtt-v5.0.requirements.json"),
]


def decode_match(match: re.Match[str]) -> str:
    first = int(match.group(1), 16)
    second_raw = match.group(2)

    # Handle JSON-style surrogate pairs, e.g. \uD83D\uDE00
    if 0xD800 <= first <= 0xDBFF and second_raw is not None:
        second = int(second_raw, 16)
        if 0xDC00 <= second <= 0xDFFF:
            codepoint = 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00)
            return chr(codepoint)

    # If this is not a valid surrogate pair, only decode the first escape.
    return chr(first)


def fix_file(path: Path, check: bool) -> bool:
    original = path.read_text(encoding="utf-8")
    fixed = UNICODE_ESCAPE.sub(decode_match, original)

    if fixed == original:
        return False

    if check:
        print(f"would update {path}")
    else:
        path.write_text(fixed, encoding="utf-8")
        print(f"updated {path}")

    return True


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Decode JSON \\uXXXX escapes in documentation requirement files only."
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Print files that would change without modifying them.",
    )
    args = parser.parse_args()

    changed = False

    for path in DOC_FILES:
        if not path.exists():
            raise FileNotFoundError(path)

        changed |= fix_file(path, args.check)

    if args.check and changed:
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
