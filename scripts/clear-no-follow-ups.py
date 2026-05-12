#!/usr/bin/env python3
import json
from pathlib import Path

files = [
    Path("docs/spec/mqtt-v5.0.requirements.json"),
    Path("docs/spec/mqtt-v3.1.1.requirements.json"),
]

changed = 0


def normalize(obj):
    global changed

    if isinstance(obj, dict):
        if isinstance(obj.get("follow_up"), str) and obj["follow_up"].startswith(
            "No follow"
        ):
            obj["follow_up"] = None
            changed += 1

        for value in obj.values():
            normalize(value)

    elif isinstance(obj, list):
        for item in obj:
            normalize(item)


for path in files:
    data = json.loads(path.read_text(encoding="utf-8"))
    normalize(data)
    path.write_text(
        json.dumps(data, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )

print(f"Replaced {changed} follow_up values with null.")
