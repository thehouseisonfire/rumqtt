#!/usr/bin/env python3
"""Prove declaration coverage and actual core-to-C retention independently.

Mutate only an isolated copy under target/. A baseline test must execute and
pass before its conversion is changed. No tracked checkout file is edited.
"""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def run(command: list[str], cwd: Path, environment: dict[str, str]) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(command, cwd=cwd, env=environment, text=True, capture_output=True)
    if result.returncode:
        print(result.stdout)
        print(result.stderr)
    return result


def main() -> None:
    root = Path(__file__).resolve().parents[3]
    target = root / "native-wrappers" / "target"
    target.mkdir(exist_ok=True)
    environment = dict(os.environ)
    # Keep mutant build outputs separate from ordinary libraries and tests.
    environment["CARGO_TARGET_DIR"] = str(target / "parity-conversion-build")
    files = (
        subprocess.run(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
            cwd=root,
            check=True,
            capture_output=True,
        )
        .stdout.decode()
        .split("\0")
    )
    with tempfile.TemporaryDirectory(prefix="parity-conversion-", dir=target) as directory:
        scratch = Path(directory)
        for relative in filter(None, files):
            source = root / relative
            if source.is_file():
                destination = scratch / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, destination)
        executable = target / "debug" / ("rumqttc-parity-audit.exe" if os.name == "nt" else "rumqttc-parity-audit")
        audit = [str(executable), "check", "--root", str(scratch)]
        assert run(audit, scratch, environment).returncode == 0, "baseline inventory failed"
        test_name = "publish_budget_snapshot_empty_restore_reconnect_and_terminal_reads"
        command = [
            "cargo",
            "test",
            "--locked",
            "--offline",
            "--manifest-path",
            "native-wrappers/Cargo.toml",
            "-p",
            "rumqttc-c-next",
            "--no-default-features",
            "--lib",
            test_name,
            "--",
            "--nocapture",
        ]
        baseline = run(command, scratch, environment)
        assert baseline.returncode == 0 and "running 1 test" in baseline.stdout, (
            "baseline mapping test did not pass and execute"
        )
        source_path = scratch / "native-wrappers/c/src/ffi.rs"
        source = source_path.read_text(encoding="utf-8")
        assignment = "out.recovery_pending = u8::from(snapshot.recovery_pending);"
        assert source.count(assignment) == 1, "conversion mutation anchor changed; review this test"
        source_path.write_text(source.replace(assignment, "out.recovery_pending = 0;"), encoding="utf-8")
        assert run(audit, scratch, environment).returncode == 0, (
            "conversion bodies must not masquerade as declaration changes"
        )
        mutant = run(command, scratch, environment)
        assert mutant.returncode != 0 and "running 1 test" in mutant.stdout, (
            "mapping regression did not execute and fail"
        )
        assert f"{test_name} ... FAILED" in mutant.stdout, "unrelated failure is not conversion evidence"
        assert "assertion" in mutant.stderr, "expected the retained-value assertion to fail"
        print(
            "Declaration audit stayed green; the recovery_pending retention assertion rejected the conversion mutation."
        )


if __name__ == "__main__":
    main()
