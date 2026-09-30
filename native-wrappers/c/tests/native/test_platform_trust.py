"""Exercise trust cleanup without modifying the host's actual certificate stores."""

import contextlib
import copy
import hashlib
import json
import os
import plistlib
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

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

    def mac_store(self, *, unrelated=True):
        """Model the public security commands, without touching the host store."""
        thumbprint = hashlib.sha1(b"root").hexdigest().upper()
        trust = {"trustVersion": 1, "trustList": {}}
        if unrelated:
            trust["trustList"]["OTHER"] = {"issuerName": b"other root", "trustSettings": []}
        certificates = set()
        commands = []
        failures = {}

        def command(arguments, environment=None):
            commands.append(arguments)
            action = arguments[3] if arguments[0] == "sudo" else arguments[1]
            failure = failures.get(action)
            if failure:
                failure(arguments)
            if action == "find-certificate":
                return "\n".join(f"SHA-1 hash: {value}" for value in certificates)
            if action == "add-trusted-cert":
                self.assertEqual(arguments[arguments.index("-k") + 1], "/Library/Keychains/System.keychain")
                result = arguments[arguments.index("-r") + 1]
                if result == "trustRoot":
                    persisted = json.loads(next(self.state.glob("*.json")).read_text())
                    self.assertTrue(persisted["owned"])
                    self.assertTrue(persisted["certificate_owned"])
                certificates.add(thumbprint)
                trust["trustList"][thumbprint] = {
                    "issuerName": b"fixture root",
                    "trustSettings": [{"kSecTrustSettingsResult": 3 if result == "deny" else 1}],
                }
            elif action == "remove-trusted-cert":
                trust["trustList"].pop(thumbprint, None)
            elif action == "trust-settings-import":
                with Path(arguments[-1]).open("rb") as source:
                    trust.clear()
                    trust.update(plistlib.load(source))
            elif action == "delete-certificate":
                self.assertEqual(arguments[-1], "/Library/Keychains/System.keychain")
                self.assertEqual(arguments[-2], thumbprint)
                certificates.discard(thumbprint)
            else:
                self.fail(f"unexpected macOS command: {arguments}")
            return ""

        self.stack.enter_context(patch.object(platform_trust.sys, "platform", "darwin"))
        self.command = self.stack.enter_context(patch.object(platform_trust, "command", side_effect=command))
        self.stack.enter_context(
            patch.object(platform_trust, "mac_trust_settings", side_effect=lambda: copy.deepcopy(trust))
        )
        return thumbprint, trust, certificates, commands, failures

    def test_macos_system_keychain_install_and_cleanup_preserve_unrelated_trust(self):
        thumbprint, trust, certificates, commands, _ = self.mac_store()
        original = copy.deepcopy(trust)
        with platform_trust.trusted_root(self.root, self.environment):
            self.assertIn(thumbprint, certificates)
            self.assertIn(thumbprint, trust["trustList"])
            self.assertNotIn("SSL_CERT_FILE", self.environment)
            self.assertNotIn("SSL_CERT_DIR", self.environment)
            self.assertEqual(self.environment["RUMQTTC_TEST_PLATFORM_TRUST"], "1")
        self.assertEqual(trust, original)
        self.assertFalse(certificates)
        self.assertFalse(any("delete-keychain" in args or "list-keychains" in args for args in commands))
        self.assert_clean()

    def test_macos_final_entry_is_denied_and_certificate_deleted_after_child_failure(self):
        thumbprint, trust, certificates, commands, _ = self.mac_store(unrelated=False)
        with (
            self.assertRaisesRegex(RuntimeError, "child failed"),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            raise RuntimeError("child failed")
        self.assertEqual(trust["trustList"][thumbprint]["trustSettings"], [{"kSecTrustSettingsResult": 3}])
        self.assertFalse(certificates)
        self.assertFalse(any("remove-trusted-cert" in args or "trust-settings-import" in args for args in commands))
        self.assertFalse(list(self.state.glob("*.json")))
        self.assertFalse(list(self.state.glob("*.cer")))
        audit = json.loads(next(self.state.glob("*.cleanup")).read_text())
        self.assertTrue(audit["certificate_removed"])
        self.assertIn("unconditional deny", audit["retained_metadata"])
        before = len(commands)
        platform_trust.cleanup_all()
        self.assertEqual(len(commands), before)

    def test_macos_partial_installation_removes_owned_certificate(self):
        thumbprint, _, certificates, _, failures = self.mac_store()

        def partial_install(arguments):
            persisted = json.loads(next(self.state.glob("*.json")).read_text())
            self.assertTrue(persisted["certificate_owned"])
            certificates.add(thumbprint)
            raise RuntimeError("partial installation")

        failures["add-trusted-cert"] = partial_install
        with (
            self.assertRaisesRegex(RuntimeError, "partial installation"),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            self.fail("partial installation must not reach child")
        self.assertFalse(certificates)
        self.assert_clean()

    def test_macos_timeout_import_preserves_unrelated_current_trust(self):
        _, trust, certificates, commands, failures = self.mac_store()
        original = copy.deepcopy(trust)

        def timeout(arguments):
            raise subprocess.TimeoutExpired(arguments, 30)

        failures["remove-trusted-cert"] = timeout
        with platform_trust.trusted_root(self.root, self.environment):
            pass
        self.assertTrue(any("trust-settings-import" in args for args in commands))
        self.assertEqual(trust, original)
        self.assertFalse(certificates)
        self.assert_clean()

    def test_macos_empty_import_is_avoided(self):
        thumbprint, trust, _, commands, _ = self.mac_store(unrelated=False)
        trust["trustList"][thumbprint.lower()] = {"trustSettings": []}
        self.assertFalse(platform_trust.mac_remove_trust_with_import(thumbprint))
        self.assertFalse(commands)
        self.assertIn(thumbprint.lower(), trust["trustList"])

    def test_macos_import_requires_disposable_runner(self):
        with (
            patch.dict(os.environ, {"RUMQTTC_DISPOSABLE_TRUST_RUNNER": "0"}),
            patch.object(platform_trust, "command") as command,
            self.assertRaisesRegex(RuntimeError, "disposable"),
        ):
            platform_trust.mac_remove_trust_with_import("FIXTURE")
        command.assert_not_called()

    def test_macos_deny_requires_disposable_runner(self):
        with (
            patch.dict(os.environ, {"RUMQTTC_DISPOSABLE_TRUST_RUNNER": "0"}),
            patch.object(platform_trust, "command") as command,
            self.assertRaisesRegex(RuntimeError, "disposable"),
        ):
            platform_trust.mac_distrust_fixture({})
        command.assert_not_called()

    def test_macos_import_failure_keeps_manifest_for_retry(self):
        _, trust, certificates, _, failures = self.mac_store()
        original = copy.deepcopy(trust)

        def timeout(arguments):
            raise subprocess.TimeoutExpired(arguments, 30)

        def failed_import(arguments):
            raise RuntimeError("import failed")

        failures.update({"remove-trusted-cert": timeout, "trust-settings-import": failed_import})
        with (
            self.assertRaisesRegex(RuntimeError, "import failed"),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            pass
        self.assertEqual(len(list(self.state.glob("*.json"))), 1)
        self.assertEqual(len(list(self.state.glob("*.cer"))), 1)
        self.assertTrue(certificates)
        failures.clear()
        platform_trust.cleanup_all()
        self.assertFalse(certificates)
        self.assertEqual(trust, original)
        self.assert_clean()

    def test_macos_rechecks_empty_store_after_removal_timeout(self):
        thumbprint, trust, certificates, commands, failures = self.mac_store()

        def timeout(arguments):
            trust["trustList"].pop("OTHER")
            raise subprocess.TimeoutExpired(arguments, 30)

        failures["remove-trusted-cert"] = timeout
        with platform_trust.trusted_root(self.root, self.environment):
            pass
        self.assertEqual(trust["trustList"][thumbprint]["trustSettings"], [{"kSecTrustSettingsResult": 3}])
        self.assertFalse(certificates)
        self.assertFalse(any("trust-settings-import" in args for args in commands))

    def test_macos_failed_deny_retains_certificate_and_cleanup_manifest(self):
        _, _, certificates, commands, failures = self.mac_store(unrelated=False)
        with (
            self.assertRaisesRegex(RuntimeError, "deny failed"),
            platform_trust.trusted_root(self.root, self.environment),
        ):

            def fail_deny(arguments):
                raise RuntimeError("deny failed")

            failures["add-trusted-cert"] = fail_deny
        self.assertTrue(certificates)
        self.assertEqual(len(list(self.state.glob("*.json"))), 1)
        self.assertFalse(list(self.state.glob("*.cleanup")))
        self.assertFalse(any("delete-certificate" in args for args in commands))

    def test_macos_deny_is_verified_before_deleting_certificate(self):
        thumbprint, trust, certificates, commands, _ = self.mac_store(unrelated=False)
        original_command = self.command.side_effect

        def no_effect(arguments, environment=None):
            if "deny" in arguments:
                return ""
            return original_command(arguments, environment)

        with (
            self.assertRaisesRegex(RuntimeError, "not explicitly distrusted"),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            self.command.side_effect = no_effect
        self.assertTrue(certificates)
        self.assertEqual(trust["trustList"][thumbprint]["trustSettings"], [{"kSecTrustSettingsResult": 1}])
        self.assertFalse(any("delete-certificate" in args for args in commands))
        self.assertEqual(len(list(self.state.glob("*.json"))), 1)

    def test_macos_certificate_deletion_is_verified_and_retryable(self):
        _, _, certificates, _, _ = self.mac_store()
        original_command = self.command.side_effect

        def no_effect(arguments, environment=None):
            if "delete-certificate" in arguments:
                return ""
            return original_command(arguments, environment)

        with (
            self.assertRaisesRegex(RuntimeError, "certificate survived"),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            self.command.side_effect = no_effect
        self.assertTrue(certificates)
        self.assertEqual(len(list(self.state.glob("*.json"))), 1)
        self.command.side_effect = original_command
        platform_trust.cleanup_all()
        self.assertFalse(certificates)
        self.assert_clean()

    def test_macos_does_not_remove_preexisting_trust(self):
        thumbprint, trust, _, commands, _ = self.mac_store()
        trust["trustList"][thumbprint] = {"trustSettings": []}
        original = copy.deepcopy(trust)
        with (
            self.assertRaisesRegex(RuntimeError, "already exists"),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            self.fail("fixture must not run")
        self.assertEqual(trust, original)
        self.assertFalse(commands)
        self.assert_clean()

    def test_macos_does_not_remove_preexisting_certificate(self):
        thumbprint, _, certificates, commands, _ = self.mac_store()
        certificates.add(thumbprint)
        with (
            self.assertRaisesRegex(RuntimeError, "already exists"),
            platform_trust.trusted_root(self.root, self.environment),
        ):
            self.fail("fixture must not run")
        self.assertEqual(certificates, {thumbprint})
        self.assertEqual(len(commands), 1)
        self.assert_clean()

    def test_legacy_search_list_is_restored_even_if_trust_cleanup_fails(self):
        self.state.mkdir()
        root = self.state / "fixture.cer"
        root.touch()
        keychain = self.state / "fixture.keychain-db"
        keychain.touch()
        manifest = self.state / "fixture.json"
        state = {
            "platform": "darwin",
            "thumbprint": "FIXTURE",
            "owned": True,
            "root": str(root),
            "keychain": str(keychain),
            "keychain_owned": True,
            "search_changed": True,
            "search_list": ["/original/login.keychain-db"],
        }
        platform_trust.save_state(manifest, state)
        commands = []

        def command(arguments, environment=None):
            commands.append(arguments)
            if "add-trusted-cert" in arguments:
                raise RuntimeError("deny failed")
            if "list-keychains" in arguments and "-s" not in arguments:
                return '"/original/login.keychain-db"'
            return ""

        with (
            patch.object(platform_trust, "mac_trust_settings", return_value={"trustList": {"FIXTURE": {}}}),
            patch.object(platform_trust, "command", side_effect=command),
            self.assertRaisesRegex(RuntimeError, "deny failed"),
        ):
            platform_trust.cleanup(manifest)
        self.assertIn(["security", "list-keychains", "-d", "user", "-s", "/original/login.keychain-db"], commands)
        self.assertTrue(manifest.exists())
        self.assertTrue(root.exists())
        self.assertTrue(keychain.exists())


class CommandTests(unittest.TestCase):
    @unittest.skipIf(os.name == "nt", "POSIX process groups")
    def test_sudo_timeout_kills_privileged_group_before_returning(self):
        arguments = ["sudo", "-n", "security", "remove-trusted-cert"]
        process = MagicMock(pid=123)
        process.communicate.side_effect = [subprocess.TimeoutExpired(arguments, 30), ("out", "err")]
        with (
            patch.object(platform_trust.subprocess, "Popen", return_value=process) as popen,
            patch.object(platform_trust.subprocess, "run") as run,
            self.assertRaises(subprocess.TimeoutExpired) as failure,
        ):
            platform_trust.command(arguments)
        self.assertTrue(popen.call_args.kwargs["start_new_session"])
        self.assertEqual(run.call_args.args[0], ["sudo", "-n", "/bin/kill", "-KILL", "--", "-123"])
        self.assertEqual(process.communicate.call_count, 2)
        self.assertEqual(failure.exception.stdout, "out")

    @unittest.skipIf(os.name == "nt", "POSIX process groups")
    def test_timeout_terminates_child_process_group(self):
        # A sleeping descendant inherits the pipe. Killing only its parent would
        # leave communicate() blocked and prevent the cleanup retry from running.
        script = (
            "import subprocess,sys,time; "
            "subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)']); time.sleep(60)"
        )
        with self.assertRaises(subprocess.TimeoutExpired):
            platform_trust.command([sys.executable, "-c", script], timeout=0.3)

    @unittest.skipIf(os.name == "nt", "POSIX command implementation")
    def test_command_preserves_failure_status_and_output(self):
        with self.assertRaises(subprocess.CalledProcessError) as failure:
            platform_trust.command(
                [sys.executable, "-c", "import sys; print('out'); print('err',file=sys.stderr); sys.exit(7)"]
            )
        self.assertEqual(failure.exception.returncode, 7)
        self.assertEqual(failure.exception.stdout.strip(), "out")
        self.assertEqual(failure.exception.stderr.strip(), "err")


if __name__ == "__main__":
    unittest.main()
