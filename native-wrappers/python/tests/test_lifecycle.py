from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).parent


@pytest.mark.skipif("RUMQTTC_TEST_PORT" not in os.environ, reason="broker fixture is not running")
@pytest.mark.parametrize("mode", ["gc-cycle", "repetition", "module-teardown", "explicit-exit", "abrupt-exit"])
def test_interpreter_lifecycle_cases_are_bounded_and_quiet(mode: str, tmp_path: Path) -> None:
    result = subprocess.run(
        [sys.executable, str(ROOT / "lifecycle" / "process_cases.py"), mode],
        cwd=tmp_path,
        env=os.environ.copy(),
        text=True,
        capture_output=True,
        timeout=20,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert result.stderr == ""


@pytest.mark.skipif("RUMQTTC_TEST_PORT" not in os.environ, reason="broker fixture is not running")
@pytest.mark.parametrize("script", ["live_client.py", "loop_closing_client.py"])
def test_live_loop_shutdown_is_bounded_and_quiet(script: str, tmp_path: Path) -> None:
    result = subprocess.run(
        [sys.executable, str(ROOT / "lifecycle" / script)],
        cwd=tmp_path,
        env=os.environ.copy(),
        text=True,
        capture_output=True,
        timeout=10,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert result.stderr == ""
