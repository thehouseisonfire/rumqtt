"""Require supported Will cases and release child processes when a case fails."""

import os
import subprocess
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

import will_fixture


class WillFixtureTests(unittest.TestCase):
    def test_missing_broker_fails_required_execution_and_only_skips_optional_runs(self):
        with (
            patch("sys.argv", ["will_fixture.py", "--binary", "native-will"]),
            patch.dict(os.environ, {"MOSQUITTO_BIN": ""}),
            patch.object(will_fixture.shutil, "which", return_value=None),
        ):
            for required, expected in [("1", 1), ("0", 77)]:
                with patch.dict(os.environ, {"RUMQTTC_REQUIRE_MOSQUITTO": required}):
                    self.assertEqual(will_fixture.main(), expected)

    def test_source_failure_kills_observer_and_terminates_broker(self):
        broker = Mock()
        broker.poll.return_value = None
        observer = Mock()
        observer.poll.return_value = None
        connection = Mock()
        connection.__enter__ = Mock(return_value=connection)
        connection.__exit__ = Mock(return_value=False)

        def start(arguments, **kwargs):
            if kwargs.get("env"):
                Path(kwargs["env"]["RUMQTTC_TEST_READY_PATH"]).touch()
                return observer
            return broker

        with (
            patch("sys.argv", ["will_fixture.py", "--binary", "native-will"]),
            patch.dict(os.environ, {"MOSQUITTO_BIN": "mock-mosquitto"}),
            patch.object(will_fixture.subprocess, "Popen", side_effect=start),
            patch.object(will_fixture.socket, "create_connection", return_value=connection),
            patch.object(will_fixture.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "source")),
            self.assertRaises(subprocess.CalledProcessError),
        ):
            will_fixture.main()
        observer.kill.assert_called_once()
        observer.wait.assert_called_once_with(timeout=5)
        broker.terminate.assert_called_once()
        broker.wait.assert_called_once_with(timeout=5)


if __name__ == "__main__":
    unittest.main()
