from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

from tests.installed import audit_linux_wheel
from tests.installed.audit_linux_wheel import parse_policy, policy_is_compatible


@pytest.mark.parametrize(
    ("computed", "required"),
    [
        ("manylinux_2_17_x86_64", "manylinux_2_17_x86_64"),
        ("manylinux_2_12_x86_64", "manylinux_2_17_x86_64"),
        ("musllinux_1_1_x86_64", "musllinux_1_2_x86_64"),
    ],
)
def test_equal_and_older_linux_policies_are_compatible(computed: str, required: str) -> None:
    assert policy_is_compatible(computed, required)


@pytest.mark.parametrize(
    ("computed", "required"),
    [
        ("manylinux_2_28_x86_64", "manylinux_2_17_x86_64"),
        ("manylinux_2_17_aarch64", "manylinux_2_17_x86_64"),
        ("musllinux_1_2_x86_64", "manylinux_2_17_x86_64"),
        ("linux_x86_64", "manylinux_2_17_x86_64"),
    ],
)
def test_newer_or_incompatible_linux_policies_are_rejected(computed: str, required: str) -> None:
    assert not policy_is_compatible(computed, required)


def test_required_policy_must_be_canonical() -> None:
    with pytest.raises(ValueError, match="unsupported Linux wheel policy"):
        parse_policy("manylinux2014_x86_64")


def test_audit_uses_json_and_accepts_a_more_portable_policy(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    wheel = tmp_path / "rumqttc.whl"
    wheel.touch()
    commands: list[list[str]] = []

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        commands.append(command)
        return subprocess.CompletedProcess(
            command,
            0,
            stdout=json.dumps({"overall_tag": "manylinux_2_12_x86_64"}),
            stderr="",
        )

    monkeypatch.setattr(audit_linux_wheel.subprocess, "run", run)
    monkeypatch.setattr(
        sys,
        "argv",
        [str(audit_linux_wheel.__file__), str(wheel), "--policy", "manylinux_2_17_x86_64"],
    )

    assert audit_linux_wheel.main() == 0
    assert commands == [[sys.executable, "-m", "auditwheel", "show", "--json", str(wheel)]]
