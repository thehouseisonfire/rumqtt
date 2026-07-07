import importlib.util
import json
import unittest
from pathlib import Path


RUNNER_PATH = Path(__file__).resolve().parents[1] / "runner.py"
SPEC = importlib.util.spec_from_file_location("runner", RUNNER_PATH)
runner = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(runner)


class RunnerTests(unittest.TestCase):
    def scenario(self, **overrides):
        scenario = {
            "name": "codec-v5-publish-roundtrip",
            "group": "codec",
            "command": "roundtrip",
            "description": "MQTT 5 codec roundtrip throughput.",
            "primary_metric": "messages_sec",
            "higher_is_better": True,
            "requires_broker": False,
            "args": {"protocol": "v5", "payload_size": 64, "messages": 1000},
        }
        scenario.update(overrides)
        return scenario

    def payload(self, metrics=None):
        return {
            "schema_version": 1,
            "run_id": "run-1",
            "scenario": "codec-v5-roundtrip",
            "started_at_unix": 1,
            "finished_at_unix": 2,
            "config": {"protocol": "v5"},
            "metrics": metrics or {"messages_sec": 10.0},
            "samples": {"messages": [10.0]},
            "environment": {
                "git_commit": None,
                "rustc": None,
                "target": "unknown",
                "os": "linux",
                "arch": "x86_64",
                "cpu_count": 1,
            },
        }

    def test_scenario_command_builds_canonical_cli(self):
        scenario = self.scenario()

        command = runner.scenario_command(
            scenario,
            run_id="run-1",
            broker_url=None,
            ca_cert=None,
        )

        self.assertEqual(
            command[:9],
            [
                "cargo",
                "run",
                "--release",
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

    def test_scenario_command_allows_explicit_dev_profile(self):
        command = runner.scenario_command(
            self.scenario(),
            run_id="run-1",
            broker_url=None,
            ca_cert=None,
            cargo_profile="dev",
        )

        self.assertNotIn("--release", command)

    def test_validate_scenario_requires_metadata(self):
        scenario = self.scenario(primary_metric="")

        with self.assertRaisesRegex(RuntimeError, "primary_metric"):
            runner.validate_scenario(Path("scenario.toml"), scenario)

    def test_validate_scenario_requires_broker_for_client_scenarios(self):
        scenario = self.scenario(
            group="client",
            command="throughput",
            requires_broker=False,
        )

        with self.assertRaisesRegex(RuntimeError, "requires_broker"):
            runner.validate_scenario(Path("scenario.toml"), scenario)

    def test_broker_required_scenario_fails_without_broker_url(self):
        scenario = self.scenario(
            group="client",
            command="throughput",
            requires_broker=True,
            primary_metric="throughput_msg_sec",
        )

        with self.assertRaisesRegex(RuntimeError, "requires an external broker"):
            runner.validate_broker_requirement(scenario, None)

        runner.validate_broker_requirement(scenario, "mqtt://127.0.0.1:1883")

    def test_read_benchmark_json_rejects_unsupported_schema_version(self):
        payload = self.payload()
        payload["schema_version"] = 2

        with self.assertRaisesRegex(RuntimeError, "schema_version"):
            runner.read_benchmark_json(json.dumps(payload), self.scenario())

    def test_read_benchmark_json_rejects_missing_primary_metric(self):
        payload = self.payload(metrics={"bytes_sec": 20.0})

        with self.assertRaisesRegex(RuntimeError, "messages_sec"):
            runner.read_benchmark_json(json.dumps(payload), self.scenario())

    def test_read_benchmark_json_rejects_non_numeric_metrics(self):
        payload = self.payload(metrics={"messages_sec": "fast"})

        with self.assertRaisesRegex(RuntimeError, "messages_sec"):
            runner.read_benchmark_json(json.dumps(payload), self.scenario())

    def test_lower_is_better_comparison_classifies_improvement(self):
        scenario = self.scenario(
            name="client-v5-latency-qos1",
            group="client",
            command="latency",
            primary_metric="p99",
            higher_is_better=False,
            requires_broker=True,
        )

        comparison = runner.compare_summaries(
            [{"ok": True, "metrics": {"p99": 100.0}}],
            [{"ok": True, "metrics": {"p99": 80.0}}],
            scenario=scenario,
            bootstrap_samples=100,
            confidence=0.95,
        )

        self.assertFalse(comparison["p99"]["higher_is_better"])
        self.assertEqual(comparison["p99"]["classification"], "improvement")

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
