import importlib.util
import unittest
from pathlib import Path


RUNNER_PATH = Path(__file__).resolve().parents[1] / "runner.py"
SPEC = importlib.util.spec_from_file_location("runner", RUNNER_PATH)
runner = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(runner)


class RunnerTests(unittest.TestCase):
    def test_scenario_command_builds_canonical_cli(self):
        scenario = {
            "name": "codec-v5-publish-roundtrip",
            "group": "codec",
            "command": "roundtrip",
            "args": {"protocol": "v5", "payload_size": 64, "messages": 1000},
        }

        command = runner.scenario_command(
            scenario,
            run_id="run-1",
            broker_url=None,
            ca_cert=None,
        )

        self.assertEqual(
            command[:8],
            [
                "cargo",
                "run",
                "-p",
                "benchmarks",
                "--bin",
                "rumqtt-bench",
                "--",
                "codec",
            ],
        )
        self.assertIn("--payload-size", command)
        self.assertIn("--run-id", command)

    def test_summarize_runs_uses_only_successful_metrics(self):
        summary = runner.summarize_runs(
            [
                {"ok": True, "metrics": {"messages_sec": 10.0}},
                {"ok": True, "metrics": {"messages_sec": 20.0}},
                {"ok": False, "metrics": {"messages_sec": 1000.0}},
            ]
        )

        self.assertEqual(summary["total_runs"], 3)
        self.assertEqual(summary["successful_runs"], 2)
        self.assertEqual(summary["metrics"]["messages_sec"]["median"], 15.0)


if __name__ == "__main__":
    unittest.main()
