from __future__ import annotations

import argparse
import re
import zipfile
from pathlib import Path

REQUIRED_PACKAGE_FILES = {
    "rumqttc/__init__.py",
    "rumqttc/_client.py",
    "rumqttc/_errors.py",
    "rumqttc/_events.py",
    "rumqttc/_types.py",
    "rumqttc/_native.pyi",
    "rumqttc/py.typed",
}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("wheel", type=Path)
    parser.add_argument("--python-tag", required=True)
    parser.add_argument("--platform-pattern", required=True)
    arguments = parser.parse_args()

    wheel = arguments.wheel
    filename = wheel.name
    expected_abi = f"{arguments.python_tag}-{arguments.python_tag}"
    assert f"-{expected_abi}-" in filename, f"wrong Python/ABI tag: {filename}"
    assert re.search(arguments.platform_pattern, filename), f"wrong platform tag: {filename}"

    with zipfile.ZipFile(wheel) as archive:
        names = set(archive.namelist())
        assert names >= REQUIRED_PACKAGE_FILES
        assert any(name.startswith("rumqttc/_native.") and name.endswith((".so", ".pyd")) for name in names)
        assert any(name.endswith(".dist-info/licenses/LICENSE-APACHE") for name in names)
        assert any(name.endswith(".dist-info/licenses/LICENSE-MIT") for name in names)
        assert not any(
            "__pycache__" in name or name.endswith((".pyc", ".rs", ".rlib")) or "/tests/" in name or "/target/" in name
            for name in names
        )
        wheel_metadata = next(name for name in names if name.endswith(".dist-info/WHEEL"))
        metadata = archive.read(wheel_metadata).decode()
        assert f"Tag: {expected_abi}-" in metadata


if __name__ == "__main__":
    main()
