"""Exercise trust cleanup without modifying the host's actual certificate stores."""

import contextlib
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import platform_trust


class PlatformTrustTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name) / "root.pem"
        self.root.write_text("mock certificate", encoding="ascii")
        self.state = Path(self.directory.name) / "state"
        self.environment = {"SSL_CERT_FILE": "old", "SSL_CERT_DIR": "old"}
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.stack.enter_context(
            patch.dict(
                os.environ,
                {
                    "RUMQTTC_DISPOSABLE_TRUST_RUNNER": "1",
                    "RUMQTTC_PLATFORM_TRUST_STATE_DIR": str(self.state),
                },
            )
        )
        self.stack.enter_context(patch.object(platform_trust.ssl, "PEM_cert_to_DER_cert", return_value=b"root"))

    def assert_clean(self):
        self.assertEqual(list(self.state.glob("*")), [])

    def test_linux_uses_environment_without_changing_platform_trust(self):
        with patch.object(platform_trust.sys, "platform", "linux"), patch.object(platform_trust, "command") as command:
            with platform_trust.trusted_root(self.root, self.environment):
                self.assertEqual(self.environment["SSL_CERT_FILE"], str(self.root))
                empty = Path(self.environment["SSL_CERT_DIR"])
                self.assertTrue(empty.is_dir())
                self.assertEqual(list(empty.iterdir()), [])
            self.assertFalse(empty.exists())
            command.assert_not_called()
        self.assert_clean()

    def test_os_store_requires_explicit_disposable_runner(self):
        with (
            patch.object(platform_trust.sys, "platform", "win32"),
            patch.dict(
                os.environ,
                {
                    "RUMQTTC_DISPOSABLE_TRUST_RUNNER": "0",
                },
            ),
            self.assertRaisesRegex(RuntimeError, "disposable"),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            self.fail("fixture must not run")
        self.assert_clean()

    def test_windows_cleans_up_after_child_failure(self):
        scripts = []

        def powershell(script, state):
            scripts.append(script)
            if "Import-Certificate" in script:
                persisted = json.loads(next(self.state.glob("*.json")).read_text())
                self.assertTrue(persisted["owned"])
            return ""

        with (
            patch.object(platform_trust.sys, "platform", "win32"),
            patch.object(platform_trust, "powershell", side_effect=powershell),
            self.assertRaisesRegex(RuntimeError, "child failed"),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            self.assertNotIn("SSL_CERT_FILE", self.environment)
            self.assertNotIn("SSL_CERT_DIR", self.environment)
            self.assertEqual(self.environment["RUMQTTC_TEST_PLATFORM_TRUST"], "1")
            raise RuntimeError("child failed")
        self.assertIn("Remove-Item", scripts[-1])
        self.assert_clean()

    def test_windows_partial_import_is_removed(self):
        scripts = []

        def powershell(script, state):
            scripts.append(script)
            if "Import-Certificate" in script:
                raise subprocess.CalledProcessError(1, "powershell")
            return ""

        with (
            patch.object(platform_trust.sys, "platform", "win32"),
            patch.object(platform_trust, "powershell", side_effect=powershell),
            self.assertRaises(subprocess.CalledProcessError),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            self.fail("partial import must not reach child")
        self.assertIn("Remove-Item", scripts[-1])
        self.assert_clean()

    def test_windows_does_not_remove_preexisting_root(self):
        with (
            patch.object(platform_trust.sys, "platform", "win32"),
            patch.object(platform_trust, "powershell", side_effect=RuntimeError("root exists")) as powershell,
            self.assertRaisesRegex(RuntimeError, "root exists"),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            self.fail("fixture must not run")
        self.assertEqual(powershell.call_count, 1)
        self.assert_clean()

    def test_cleanup_failure_retains_manifest_for_ci_retry(self):
        calls = 0

        def powershell(script, state):
            nonlocal calls
            calls += 1
            if "Remove-Item" in script:
                raise RuntimeError("cleanup failed")
            return ""

        with (
            patch.object(platform_trust.sys, "platform", "win32"),
            patch.object(platform_trust, "powershell", side_effect=powershell),
            self.assertRaisesRegex(RuntimeError, "cleanup failed"),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            pass
        self.assertEqual(calls, 3)
        self.assertEqual(len(list(self.state.glob("*.json"))), 1)
        with patch.object(platform_trust, "powershell") as powershell:
            platform_trust.cleanup_all()
            self.assertIn("Remove-Item", powershell.call_args.args[0])
            platform_trust.cleanup_all()
            self.assertEqual(powershell.call_count, 1)
        self.assert_clean()

    def mac_commands(self, fail_install=False):
        original = ["/original/login.keychain-db", "/original/System.keychain"]
        current = original.copy()
        commands = []

        def command(arguments, environment=None):
            commands.append(arguments)
            if arguments[1] == "create-keychain":
                Path(arguments[-1]).touch()
            elif arguments[1] == "delete-keychain":
                Path(arguments[-1]).unlink()
            elif arguments[1] == "list-keychains":
                if "-s" in arguments:
                    current[:] = arguments[arguments.index("-s") + 1 :]
                else:
                    return "\n".join(f'"{entry}"' for entry in current)
            elif "add-trusted-cert" in arguments and fail_install:
                raise RuntimeError("partial trust installation")
            return ""

        return command, commands, original, current

    def test_macos_restores_search_list_and_removes_root_after_child_failure(self):
        command, commands, original, current = self.mac_commands()
        with (
            patch.object(platform_trust.sys, "platform", "darwin"),
            patch.object(platform_trust, "command", side_effect=command),
            patch.object(platform_trust, "mac_trusted", side_effect=[False, True, True, False]),
            self.assertRaisesRegex(RuntimeError, "child failed"),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            self.assertEqual(len(current), len(original) + 1)
            raise RuntimeError("child failed")
        self.assertEqual(current, original)
        self.assertTrue(any("remove-trusted-cert" in arguments for arguments in commands))
        self.assertTrue(any("delete-keychain" in arguments for arguments in commands))
        self.assert_clean()

    def test_macos_partial_installation_restores_search_list(self):
        command, commands, original, current = self.mac_commands(fail_install=True)
        with (
            patch.object(platform_trust.sys, "platform", "darwin"),
            patch.object(platform_trust, "command", side_effect=command),
            patch.object(platform_trust, "mac_trusted", side_effect=[False, True, False]),
            self.assertRaisesRegex(RuntimeError, "partial trust"),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            self.fail("partial installation must not reach child")
        self.assertEqual(current, original)
        self.assertTrue(any("remove-trusted-cert" in arguments for arguments in commands))
        self.assert_clean()

    def test_macos_retries_timed_out_trust_removal(self):
        command, commands, original, current = self.mac_commands()
        removal_attempts = 0

        def timeout_once(arguments, environment=None):
            nonlocal removal_attempts
            if "remove-trusted-cert" in arguments:
                removal_attempts += 1
                if removal_attempts == 1:
                    raise subprocess.TimeoutExpired(arguments, 30)
            return command(arguments, environment)

        with (
            patch.object(platform_trust.sys, "platform", "darwin"),
            patch.object(platform_trust, "command", side_effect=timeout_once),
            patch.object(platform_trust, "mac_trusted", side_effect=[False, True, True, True, False]),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            pass
        self.assertEqual(removal_attempts, 2)
        self.assertEqual(current, original)
        self.assertEqual(sum("remove-trusted-cert" in arguments for arguments in commands), 1)
        self.assert_clean()

    def test_macos_timeout_keeps_cleanup_manifest_for_later_retry(self):
        command, _, original, current = self.mac_commands()

        def always_timeout(arguments, environment=None):
            if "remove-trusted-cert" in arguments:
                raise subprocess.TimeoutExpired(arguments, 30)
            return command(arguments, environment)

        with (
            patch.object(platform_trust.sys, "platform", "darwin"),
            patch.object(platform_trust, "command", side_effect=always_timeout),
            patch.object(platform_trust, "mac_trusted", side_effect=[False, True, True, True, True, True]),
            self.assertRaises(subprocess.TimeoutExpired),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            pass
        self.assertEqual(len(list(self.state.glob("*.json"))), 1)
        with (
            patch.object(platform_trust, "command", side_effect=command),
            patch.object(platform_trust, "mac_trusted", side_effect=[True, False]),
        ):
            platform_trust.cleanup_all()
        self.assertEqual(current, original)
        self.assert_clean()

    def test_macos_does_not_remove_preexisting_trust(self):
        command, commands, original, current = self.mac_commands()
        with (
            patch.object(platform_trust.sys, "platform", "darwin"),
            patch.object(platform_trust, "command", side_effect=command),
            patch.object(platform_trust, "mac_trusted", return_value=True),
            self.assertRaisesRegex(RuntimeError, "already exists"),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            self.fail("fixture must not run")
        self.assertEqual(current, original)
        self.assertFalse(any("remove-trusted-cert" in arguments for arguments in commands))
        self.assert_clean()


if __name__ == "__main__":
    unittest.main()
