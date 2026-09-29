#!/usr/bin/env python3
"""Check C Will behavior across orderly shutdown and abrupt process exit."""

from __future__ import annotations

import argparse
import os
import shlex
import shutil
import socket
import subprocess
import tempfile
import time
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    args = parser.parse_args()
    broker_binary = os.environ.get("MOSQUITTO_BIN") or shutil.which("mosquitto")
    if broker_binary is None:
        print("Will process fixture requires mosquitto or MOSQUITTO_BIN")
        return 1 if os.environ.get("RUMQTTC_REQUIRE_MOSQUITTO") == "1" else 77
    with tempfile.TemporaryDirectory(prefix="rumqttc-c-will-") as directory:
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        config = Path(directory) / "mosquitto.conf"
        config.write_text(f"listener {port} 127.0.0.1\nallow_anonymous true\npersistence false\n")
        broker = subprocess.Popen(
            [broker_binary, "-c", str(config)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
        )
        try:
            deadline = time.monotonic() + 5
            while True:
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                        break
                except OSError:
                    if broker.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError("Will broker did not start") from None
                    time.sleep(0.01)
            launcher = shlex.split(os.environ.get("RUMQTTC_NATIVE_LAUNCHER", ""))
            for protocol in ("v4", "v5"):
                for shutdown in ("graceful", "abrupt"):
                    environment = os.environ.copy()
                    environment["RUMQTTC_TEST_PORT"] = str(port)
                    ready = Path(directory) / f"{protocol}-{shutdown}.ready"
                    release = Path(directory) / f"{protocol}-{shutdown}.release"
                    environment["RUMQTTC_TEST_READY_PATH"] = str(ready)
                    environment["RUMQTTC_TEST_RELEASE_PATH"] = str(release)
                    observer = subprocess.Popen(
                        [*launcher, args.binary, "observer", protocol, shutdown], env=environment
                    )
                    try:
                        deadline = time.monotonic() + 5
                        while not ready.exists():
                            if observer.poll() is not None or time.monotonic() >= deadline:
                                raise RuntimeError("Will observer did not subscribe")
                            time.sleep(0.01)
                        subprocess.run(
                            [*launcher, args.binary, "source", protocol, shutdown],
                            env=environment,
                            check=True,
                            timeout=10,
                        )
                        release.touch()
                        if observer.wait(timeout=10):
                            raise RuntimeError("Will observer failed")
                        print(f"Will verified: {protocol} {shutdown}", flush=True)
                    finally:
                        if observer.poll() is None:
                            observer.kill()
                            observer.wait(timeout=5)
        finally:
            broker.terminate()
            try:
                broker.wait(timeout=5)
            except subprocess.TimeoutExpired:
                broker.kill()
                broker.wait(timeout=5)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
