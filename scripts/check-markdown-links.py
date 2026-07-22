#!/usr/bin/env python3
"""Fail when a tracked Markdown file links to a missing local path."""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]
INLINE_LINK_RE = re.compile(
    r"!?\[[^\]]*\]\((?P<target><[^>]+>|[^\s)]+)(?:\s+['\"][^'\"]*['\"])?\)"
)
REFERENCE_LINK_RE = re.compile(
    r"^\s{0,3}\[[^\]]+\]:\s*(?P<target><[^>]+>|\S+)", re.MULTILINE
)
EXTERNAL_SCHEMES = {"http", "https", "mailto"}


def tracked_markdown_files() -> list[Path]:
    result = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "--", "*.md"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return [ROOT / line for line in result.stdout.splitlines() if line and (ROOT / line).exists()]


def local_target(raw_target: str) -> str | None:
    target = raw_target.removeprefix("<").removesuffix(">")
    parsed = urlsplit(target)
    if parsed.scheme.lower() in EXTERNAL_SCHEMES or target.startswith("#"):
        return None
    return unquote(parsed.path)


def main() -> int:
    failures: list[str] = []
    for markdown in tracked_markdown_files():
        content = markdown.read_text(encoding="utf-8")
        matches = [*INLINE_LINK_RE.finditer(content), *REFERENCE_LINK_RE.finditer(content)]
        for match in matches:
            raw_target = match.group("target")
            target = local_target(raw_target)
            if not target:
                continue
            resolved = (markdown.parent / target).resolve()
            if not resolved.exists():
                line = content.count("\n", 0, match.start()) + 1
                failures.append(f"{markdown.relative_to(ROOT)}:{line}: missing {raw_target}")

    if failures:
        print("Broken local Markdown links:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1

    print("All tracked Markdown links resolve locally.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
