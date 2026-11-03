from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).parent


@pytest.mark.skipif("RUMQTTC_TEST_PORT" not in os.environ, reason="broker fixture is not running")
@pytest.mark.parametrize(
    ("mode", "timeout"),
    [
        ("gc-cycle", 20),
        # Three warmups, 60 client lifecycles, and two 10s cleanup windows must fit
        # before the process watchdog fires, including startup and interpreter exit.
        ("repetition", 60),
        ("module-teardown", 20),
        ("explicit-exit", 20),
        ("abrupt-exit", 20),
    ],
    ids=["gc-cycle", "repetition", "module-teardown", "explicit-exit", "abrupt-exit"],
)
def test_interpreter_lifecycle_cases_are_bounded_and_quiet(mode: str, timeout: int, tmp_path: Path) -> None:
    result = subprocess.run(
        [sys.executable, str(ROOT / "lifecycle" / "process_cases.py"), mode],
        cwd=tmp_path,
        env=os.environ.copy(),
        text=True,
        capture_output=True,
        timeout=timeout,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert result.stderr == ""


@pytest.mark.skipif("RUMQTTC_TEST_PORT" not in os.environ, reason="broker fixture is not running")
@pytest.mark.parametrize("script", ["live_client.py", "loop_closing_client.py", "thread_loop_races.py"])
def test_live_loop_shutdown_is_bounded_and_quiet(script: str, tmp_path: Path) -> None:
    result = subprocess.run(
        [sys.executable, str(ROOT / "lifecycle" / script)],
        cwd=tmp_path,
        env=os.environ.copy(),
        text=True,
        capture_output=True,
        timeout=20,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert result.stderr == ""
