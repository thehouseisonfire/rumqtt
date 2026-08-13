import os
import pathlib
import platform
import re
import subprocess
import tempfile
import unittest

WORKSPACE = pathlib.Path(__file__).resolve().parents[3]
CRATE = WORKSPACE / "c"


def match(path: pathlib.Path, pattern: str) -> str:
    result = re.search(pattern, path.read_text(encoding="utf-8"), re.MULTILINE)
    if result is None:
        raise AssertionError(f"{pattern!r} was not found in {path}")
    return result.group(1)


class VersionIdentityTests(unittest.TestCase):
    def test_independent_package_and_abi_line_are_coherent(self):
        cargo = match(CRATE / "Cargo.toml", r'^version = "([^"]+)"')
        cmake_package = match(CRATE / "CMakeLists.txt", r'set\(RUMQTTC_PACKAGE_VERSION "([^"]+)"\)')
        cmake_abi = match(CRATE / "CMakeLists.txt", r'set\(RUMQTTC_ABI_LINE "([^"]+)"\)')
        abi_major = match(CRATE / "include/rumqttc.h", r"RUMQTTC_ABI_VERSION_MAJOR (\d+)u")
        abi_minor = match(CRATE / "include/rumqttc.h", r"RUMQTTC_ABI_VERSION_MINOR (\d+)u")
        rust_abi = int(match(CRATE / "src/ffi.rs", r"const ABI_VERSION: u32 = (\d+);"))

        self.assertEqual(cargo, cmake_package)
        self.assertEqual(cmake_abi, f"{abi_major}.{abi_minor}")
        self.assertEqual(rust_abi, (int(abi_major) << 16) | int(abi_minor))
        config = (CRATE / "cmake/rumqttcConfig.cmake.in").read_text(encoding="utf-8")
        self.assertIn('set(rumqttc_VERSION "@RUMQTTC_PACKAGE_VERSION@")', config)

    def test_prestable_line_has_versioned_packaging_names(self):
        cmake = (CRATE / "CMakeLists.txt").read_text(encoding="utf-8")
        self.assertIn('"rumqttc-${RUMQTTC_ABI_FILE_LINE}.dll"', cmake)
        self.assertIn('"librumqttc.${RUMQTTC_ABI_LINE}.dylib"', cmake)
        self.assertIn('"librumqttc.so.${RUMQTTC_ABI_LINE}"', cmake)

    def test_generic_unix_install_does_not_create_an_unversioned_self_symlink(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            build = pathlib.Path(temporary_directory) / "freebsd-package"
            subprocess.run(
                ["cmake", "-S", str(CRATE), "-B", str(build), "-DCMAKE_SYSTEM_NAME=FreeBSD"],
                check=True,
                capture_output=True,
                text=True,
            )
            install_script = (build / "cmake_install.cmake").read_text(encoding="utf-8")
            self.assertNotIn("create_symlink", install_script)

    def test_cmake_prerelease_does_not_satisfy_stable_version_request(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            temporary = pathlib.Path(temporary_directory)
            package_build = temporary / "package"
            subprocess.run(
                ["cmake", "-S", str(CRATE), "-B", str(package_build)],
                check=True,
                capture_output=True,
                text=True,
            )

            generated = (package_build / "rumqttcConfigVersion.cmake").read_text(encoding="utf-8")
            self.assertIn('set(PACKAGE_VERSION "0.1.0-alpha")', generated)

            consumer_source = temporary / "consumer"
            consumer_source.mkdir()
            (consumer_source / "CMakeLists.txt").write_text(
                "cmake_minimum_required(VERSION 3.20)\n"
                "project(version_check NONE)\n"
                "find_package(rumqttc 0.1.0 CONFIG REQUIRED NO_DEFAULT_PATH "
                f'PATHS "{package_build.as_posix()}")\n',
                encoding="utf-8",
            )
            result = subprocess.run(
                ["cmake", "-S", str(consumer_source), "-B", str(temporary / "consumer-build")],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("0.1.0-alpha", result.stdout + result.stderr)

            (consumer_source / "CMakeLists.txt").write_text(
                "cmake_minimum_required(VERSION 3.20)\n"
                "project(version_check NONE)\n"
                "find_package(rumqttc CONFIG REQUIRED NO_DEFAULT_PATH "
                f'PATHS "{package_build.as_posix()}")\n'
                'if(NOT rumqttc_VERSION STREQUAL "0.1.0-alpha")\n'
                '  message(FATAL_ERROR "unexpected package version: ${rumqttc_VERSION}")\n'
                "endif()\n",
                encoding="utf-8",
            )
            subprocess.run(
                ["cmake", "-S", str(consumer_source), "-B", str(temporary / "unversioned-consumer-build")],
                check=True,
                capture_output=True,
                text=True,
            )

    def test_unix_comparison_wrapper_rejects_mismatched_platform(self):
        hosts = {
            ("Linux", "x86_64"): "macos-arm64",
            ("Darwin", "arm64"): "linux-x86_64",
        }
        host = (platform.system(), platform.machine())
        if host not in hosts:
            self.skipTest(f"unsupported ABI comparison test host: {host[0]} {host[1]}")

        environment = os.environ.copy()
        environment["RUMQTTC_ABI_PLATFORM"] = hosts[host]
        result = subprocess.run(
            [str(CRATE / "tests/abi/compare-release.sh")],
            check=False,
            capture_output=True,
            text=True,
            env=environment,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("does not match host", result.stderr)

    def test_windows_comparison_wrapper_validates_host_architecture(self):
        powershell = (CRATE / "tests/abi/compare-release.ps1").read_text(encoding="utf-8")
        self.assertIn("RuntimeInformation]::OSArchitecture", powershell)
        self.assertIn("RuntimeInformation]::ProcessArchitecture", powershell)


if __name__ == "__main__":
    unittest.main()
