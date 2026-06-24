#!/usr/bin/env python3
"""Sort requirement entries in MQTT spec JSON files by natural ID order.

IDs follow the pattern ``MQTT-x.x.x-y`` where ``x.x.x`` is the spec section
number and ``y`` is the requirement ordinal within that section.  Plain
lexicographic sorting breaks on multi-digit segments (e.g. ``3.1.2-9`` sorts
after ``3.1.2-10``), so this script splits each ID into alternating text and
integer tokens for natural numeric comparison.

Usage::

    # Sort all known requirement files in-place (default)
    python docs/spec/generate_mqtt_specs.py

    # Sort specific files
    python docs/spec/generate_mqtt_specs.py path/to/foo.json path/to/bar.json

The script is idempotent: running it on an already-sorted file is a no-op.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Any

_DEFAULT_FILES = [
    "docs/spec/mqtt-v3.1.1.requirements.correct.json",
    "docs/spec/mqtt-v3.1.1.requirements.fix-attempt.json",
    "docs/spec/mqtt-v3.1.1.requirements.json",
    "docs/spec/mqtt-v5.0.requirements.correct.json",
    "docs/spec/mqtt-v5.0.requirements.json",
]

_ID_TOKEN_RE = re.compile(r"(\d+)")


def _id_sort_key(req: dict[str, Any]) -> list[int | str]:
    """Return a key that naturally sorts ``MQTT-x.x.x-y`` identifiers."""
    raw: str = req["id"]
    # Strip the constant "MQTT-" prefix so the first token is numeric.
    stripped = raw.removeprefix("MQTT-")
    tokens: list[int | str] = []
    for token in _ID_TOKEN_RE.split(stripped):
        if token.isdigit():
            tokens.append(int(token))
        elif token:  # keep separators as tiebreakers
            tokens.append(token)
    return tokens


def sort_requirements(path: Path) -> bool:
    """Sort *path* in-place.  Return ``True`` if the file was modified."""
    data = json.loads(path.read_text(encoding="utf-8"))
    requirements = data.get("requirements")
    if not isinstance(requirements, list):
        print(f"  skip {path}: no 'requirements' array", file=sys.stderr)
        return False

    sorted_reqs = sorted(requirements, key=_id_sort_key)
    if sorted_reqs == requirements:
        print(f"  {path}: already sorted ({len(requirements)} entries)")
        return False

    data["requirements"] = sorted_reqs
    path.write_text(
        json.dumps(data, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    print(f"  {path}: sorted {len(requirements)} entries")
    return True


def main(argv: list[str] | None = None) -> int:
    files = argv if argv else _DEFAULT_FILES
    modified = 0
    for raw_path in files:
        p = Path(raw_path)
        if not p.is_absolute():
            p = Path.cwd() / p
        if not p.exists():
            print(f"  skip {p}: not found", file=sys.stderr)
            continue
        if sort_requirements(p):
            modified += 1
    return 0 if modified >= 0 else 1  # always succeed; errors go to stderr


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
