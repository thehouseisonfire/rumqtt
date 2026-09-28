#!/usr/bin/env python3
"""Build, install, and consume each supported C TLS/proxy feature profile."""

from __future__ import annotations

import argparse
import os
import subprocess
from pathlib import Path

PROFILES = {
    "minimal": (),
    "rustls": ("use-rustls-ring", "websocket"),
    "native": ("use-native-tls", "websocket"),
    "mixed": ("use-rustls-ring", "use-native-tls", "websocket"),
    "rustls-proxy": ("use-rustls-ring", "websocket", "proxy"),
    "native-proxy": ("use-native-tls", "websocket", "proxy"),
    "mixed-proxy": ("use-rustls-ring", "use-native-tls", "websocket", "proxy"),
}


def run(command: list[str], workspace: Path, environment: dict[str, str]) -> None:
    subprocess.run(command, cwd=workspace, env=environment, check=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=PROFILES, action="append")
    parser.add_argument("--native", action="store_true", help="also run native transport fixtures per profile")
    args = parser.parse_args()
    workspace = Path(__file__).resolve().parents[2]
    for profile in args.profile or PROFILES:
        features = PROFILES[profile]
        environment = os.environ.copy()
        build = workspace / "target" / "c-feature-matrix" / profile
        install = build / "install"
        command = ["cargo", "build", "--locked", "--release", "-p", "rumqttc-c-next", "--no-default-features"]
        if features:
            command += ["--features", ",".join(features)]
        run(command, workspace, environment)
        native_tls = "use-native-tls" in features
        run(
            [
                "cmake",
                "-S",
                "c",
                "-B",
                str(build / "package"),
                f"-DCMAKE_INSTALL_PREFIX={install}",
                "-DCMAKE_INSTALL_LIBDIR=lib",
                f"-DRUMQTTC_NATIVE_TLS={'ON' if native_tls else 'OFF'}",
            ],
            workspace,
            environment,
        )
        run(["cmake", "--install", str(build / "package"), "--config", "Release"], workspace, environment)
        environment["PKG_CONFIG_PATH"] = (
            str(install / "lib" / "pkgconfig") + os.pathsep + environment.get("PKG_CONFIG_PATH", "")
        )
        expected = {
            "RUSTLS": "use-rustls-ring" in features,
            "NATIVE_TLS": native_tls,
            "WEBSOCKET": "websocket" in features,
            "HTTP_PROXY": "proxy" in features,
            "SOCKS5_PROXY": "proxy" in features,
        }
        run(
            [
                "cmake",
                "-S",
                "c/tests/cmake",
                "-B",
                str(build / "consumer"),
                f"-DCMAKE_PREFIX_PATH={install}",
                "-DRUMQTTC_CHECK_FEATURES=ON",
                "-DRUMQTTC_CHECK_PKGCONFIG=ON",
                *[f"-DRUMQTTC_EXPECT_{feature}={'ON' if enabled else 'OFF'}" for feature, enabled in expected.items()],
            ],
            workspace,
            environment,
        )
        run(["cmake", "--build", str(build / "consumer"), "--config", "Release"], workspace, environment)
        run(
            ["ctest", "--test-dir", str(build / "consumer"), "-C", "Release", "--output-on-failure"],
            workspace,
            environment,
        )
        if args.native:
            run(
                [
                    "cmake",
                    "-S",
                    "c/tests/native",
                    "-B",
                    str(build / "native"),
                    f"-DRUMQTTC_LIBRARY_DIR={workspace / 'target' / 'release'}",
                ],
                workspace,
                environment,
            )
            run(["cmake", "--build", str(build / "native"), "--config", "Release"], workspace, environment)
            run(
                [
                    "ctest",
                    "--test-dir",
                    str(build / "native"),
                    "-C",
                    "Release",
                    "--output-on-failure",
                    "-R",
                    "rumqttc-native-(proxy|redirect|srv|wire|runtime|tls|network|websocket|unix|socket)",
                ],
                workspace,
                environment,
            )


if __name__ == "__main__":
    main()
