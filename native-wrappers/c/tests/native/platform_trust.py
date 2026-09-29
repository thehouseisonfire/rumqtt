#!/usr/bin/env python3
"""Temporary platform trust for native fixtures on explicitly disposable runners."""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import plistlib
import shlex
import ssl
import subprocess
import sys
import tempfile
from collections.abc import Iterator
from pathlib import Path


def state_directory() -> Path:
    return Path(
        os.environ.get(
            "RUMQTTC_PLATFORM_TRUST_STATE_DIR", Path(__file__).resolve().parents[3] / "target" / "platform-trust-state"
        )
    )


def command(arguments: list[str], environment: dict[str, str] | None = None) -> str:
    return subprocess.run(arguments, env=environment, check=True, capture_output=True, text=True, timeout=30).stdout


def powershell(script: str, state: dict[str, object]) -> str:
    environment = os.environ.copy()
    environment["RUMQTTC_PLATFORM_CA"] = str(state["root"])
    environment["RUMQTTC_PLATFORM_THUMBPRINT"] = str(state["thumbprint"])
    return command(
        ["powershell", "-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference = 'Stop'; " + script],
        environment,
    )


def mac_trust_settings() -> dict:
    with tempfile.TemporaryDirectory(prefix="rumqttc-trust-inspect-") as directory:
        export = Path(directory) / "trust.plist"
        result = subprocess.run(
            ["security", "trust-settings-export", "-d", str(export)], capture_output=True, text=True, timeout=30
        )
        if result.returncode and "No Trust Settings were found" in result.stdout + result.stderr:
            return {"trustList": {}}
        result.check_returncode()
        with export.open("rb") as source:
            return plistlib.load(source)


def mac_trusted(thumbprint: str) -> bool:
    return thumbprint in {str(key).upper() for key in mac_trust_settings().get("trustList", {})}


def mac_remove_trust_with_import(thumbprint: str) -> None:
    """Bypass stalled per-certificate removal on disposable macOS CI runners."""
    if os.environ.get("RUMQTTC_DISPOSABLE_TRUST_RUNNER") != "1":
        raise RuntimeError("platform trust requires RUMQTTC_DISPOSABLE_TRUST_RUNNER=1 on a disposable runner")
    # Export current settings, preserving unrelated entries and metadata. Do not
    # restore an old snapshot that could discard changes made during the fixture.
    trust = mac_trust_settings()
    entries = trust.get("trustList", {})
    matching = [key for key in entries if str(key).upper() == thumbprint]
    if not matching:
        return
    for key in matching:
        del entries[key]
    with tempfile.TemporaryDirectory(prefix="rumqttc-trust-remove-") as directory:
        settings = Path(directory) / "trust.plist"
        with settings.open("wb") as destination:
            plistlib.dump(trust, destination)
        command(["sudo", "-n", "security", "trust-settings-import", "-d", str(settings)])


def save_state(path: Path, state: dict[str, object]) -> None:
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(state), encoding="utf-8")
    temporary.replace(path)


def cleanup(path: Path) -> None:
    """Idempotent cleanup, also used by an unconditional CI step after interrupted tests."""
    state = json.loads(path.read_text(encoding="utf-8"))
    thumbprint = str(state["thumbprint"])
    if state["platform"] == "win32":
        if state["owned"]:
            powershell(
                "$path = 'Cert:\\CurrentUser\\Root\\' + $env:RUMQTTC_PLATFORM_THUMBPRINT; "
                "if (Test-Path $path) { Remove-Item -LiteralPath $path -Force }; "
                "if (Test-Path $path) { throw 'test root survived cleanup' }",
                state,
            )
    elif state["platform"] == "darwin":
        if state["owned"] and mac_trusted(thumbprint):
            try:
                command(["sudo", "-n", "security", "remove-trusted-cert", "-d", str(state["root"])])
            except subprocess.TimeoutExpired:
                if mac_trusted(thumbprint):
                    print(
                        "macOS trust removal timed out; removing fixture entry with trust-settings-import",
                        file=sys.stderr,
                    )
                    mac_remove_trust_with_import(thumbprint)
        if state["owned"] and mac_trusted(thumbprint):
            raise RuntimeError("test root survived macOS trust cleanup")
        if state.get("search_changed"):
            command(["security", "list-keychains", "-d", "user", "-s", *state["search_list"]])
            actual = shlex.split(command(["security", "list-keychains", "-d", "user"]))
            if actual != state["search_list"]:
                raise RuntimeError("keychain search list was not restored")
        keychain = Path(str(state["keychain"]))
        if state.get("keychain_owned") and keychain.exists():
            command(["security", "delete-keychain", str(keychain)])
        if state.get("keychain_owned") and keychain.exists():
            raise RuntimeError("test keychain survived cleanup")
    Path(str(state["root"])).unlink(missing_ok=True)
    path.unlink()


@contextlib.contextmanager
def trusted_root(root: Path, environment: dict[str, str]) -> Iterator[None]:
    if sys.platform.startswith("linux"):
        with tempfile.TemporaryDirectory(prefix="rumqttc-empty-roots-") as empty:
            environment["SSL_CERT_FILE"] = str(root)
            environment["SSL_CERT_DIR"] = empty
            environment["RUMQTTC_TEST_PLATFORM_TRUST"] = "1"
            yield
        return
    if sys.platform not in {"darwin", "win32"}:
        raise RuntimeError(f"platform trust fixture is unsupported on {sys.platform}")
    if os.environ.get("RUMQTTC_DISPOSABLE_TRUST_RUNNER") != "1":
        raise RuntimeError("platform trust requires RUMQTTC_DISPOSABLE_TRUST_RUNNER=1 on a disposable runner")
    der = ssl.PEM_cert_to_DER_cert(root.read_text(encoding="ascii"))
    thumbprint = hashlib.sha1(der).hexdigest().upper()
    directory = state_directory()
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / f"{thumbprint}.json"
    if path.exists():
        raise RuntimeError("a previous platform trust fixture still needs cleanup")
    saved_root = directory / f"{thumbprint}.cer"
    if saved_root.exists():
        raise RuntimeError("a previous platform trust certificate still needs cleanup")
    state: dict[str, object] = {
        "platform": sys.platform,
        "thumbprint": thumbprint,
        "root": str(saved_root),
        "owned": False,
    }
    search_list: list[str] = []
    if sys.platform == "darwin":
        state["keychain"] = str(directory / f"{thumbprint}.keychain-db")
        search_list = shlex.split(command(["security", "list-keychains", "-d", "user"]))
        state["search_list"] = search_list
    saved_root.write_bytes(der)
    save_state(path, state)
    try:
        # Persist cleanup intent before every side effect, including partial installation.
        if sys.platform == "win32":
            powershell(
                "if (Test-Path ('Cert:\\CurrentUser\\Root\\' + $env:RUMQTTC_PLATFORM_THUMBPRINT)) "
                "{ throw 'test root already exists' }",
                state,
            )
            state["owned"] = True
            save_state(path, state)
            powershell(
                "Import-Certificate -FilePath $env:RUMQTTC_PLATFORM_CA "
                "-CertStoreLocation Cert:\\CurrentUser\\Root | Out-Null; "
                "if (-not (Test-Path ('Cert:\\CurrentUser\\Root\\' + $env:RUMQTTC_PLATFORM_THUMBPRINT))) "
                "{ throw 'test root was not installed' }",
                state,
            )
        else:
            if mac_trusted(thumbprint):
                raise RuntimeError("test root already exists in macOS trust settings")
            keychain = str(state["keychain"])
            if Path(keychain).exists():
                raise RuntimeError("test keychain already exists")
            state["owned"] = True
            state["keychain_owned"] = True
            save_state(path, state)
            command(["security", "create-keychain", "-p", "rumqttc-fixture", keychain])
            command(["security", "unlock-keychain", "-p", "rumqttc-fixture", keychain])
            state["search_changed"] = True
            save_state(path, state)
            command(["security", "list-keychains", "-d", "user", "-s", *search_list, keychain])
            command(
                [
                    "sudo",
                    "-n",
                    "security",
                    "add-trusted-cert",
                    "-d",
                    "-r",
                    "trustRoot",
                    "-p",
                    "ssl",
                    "-k",
                    keychain,
                    str(saved_root),
                ]
            )
            if not mac_trusted(thumbprint):
                raise RuntimeError("test root was not installed in macOS trust settings")
        # The two OS backends must consult their platform stores, not SSL_CERT_* overrides.
        environment.pop("SSL_CERT_FILE", None)
        environment.pop("SSL_CERT_DIR", None)
        environment["RUMQTTC_TEST_PLATFORM_TRUST"] = "1"
        yield
    finally:
        cleanup(path)


def cleanup_all() -> None:
    directory = state_directory()
    for path in sorted(directory.glob("*.json")):
        cleanup(path)


if __name__ == "__main__":
    if sys.argv[1:] != ["--cleanup"]:
        raise SystemExit("usage: platform_trust.py --cleanup")
    cleanup_all()
