import importlib.util
import json
import tempfile
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
            "quality": {
                "min_success_rate": 1.0,
                "min_measured_runs": 2,
                "max_primary_cv_pct": 10.0,
                "max_primary_mad_pct": 5.0,
                "max_relative_ci_width_pct": 10.0,
            },
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

    def test_validate_scenario_requires_quality_table(self):
        scenario = self.scenario()
        del scenario["quality"]

        with self.assertRaisesRegex(RuntimeError, "quality table"):
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
        self.assertEqual(comparison["p99"]["paired_sample_count"], 1)

    def test_metric_summary_reports_noise(self):
        summary = runner.metric_summary([10.0, 12.0, 14.0])

        self.assertEqual(summary["median"], 12.0)
        self.assertEqual(summary["mad"], 2.0)
        self.assertGreater(summary["cv_pct"], 0.0)

    def test_paired_comparison_uses_run_order(self):
        comparison = runner.compare_summaries(
            [
                {"ok": True, "metrics": {"messages_sec": 100.0}},
                {"ok": True, "metrics": {"messages_sec": 200.0}},
            ],
            [
                {"ok": True, "metrics": {"messages_sec": 110.0}},
                {"ok": True, "metrics": {"messages_sec": 180.0}},
            ],
            scenario=self.scenario(),
            bootstrap_samples=100,
            confidence=0.95,
        )

        self.assertEqual(comparison["messages_sec"]["paired_sample_count"], 2)
        self.assertIn("relative_delta_ci_width_pct", comparison["messages_sec"])

    def test_ci_width_gate_marks_primary_metric_inconclusive(self):
        scenario = self.scenario(
            quality={
                "min_success_rate": 1.0,
                "min_measured_runs": 2,
                "max_primary_cv_pct": 100.0,
                "max_primary_mad_pct": 100.0,
                "max_relative_ci_width_pct": 1.0,
            }
        )

        comparison = runner.compare_summaries(
            [
                {"ok": True, "metrics": {"messages_sec": 100.0}},
                {"ok": True, "metrics": {"messages_sec": 100.0}},
            ],
            [
                {"ok": True, "metrics": {"messages_sec": 130.0}},
                {"ok": True, "metrics": {"messages_sec": 90.0}},
            ],
            scenario=scenario,
            bootstrap_samples=100,
            confidence=0.95,
        )

        self.assertEqual(comparison["messages_sec"]["classification"], "inconclusive")
        self.assertEqual(
            comparison["messages_sec"]["inconclusive_reason"],
            "ci_width_exceeds_quality_gate",
        )

    def test_quality_status_is_advisory(self):
        summary = runner.summarize_runs(
            [
                {"ok": True, "metrics": {"messages_sec": 10.0}},
            ]
        )

        quality = runner.evaluate_run_quality(self.scenario(), summary)

        self.assertEqual(quality["status"], "fail")
        self.assertTrue(any(gate["name"] == "min_measured_runs" for gate in quality["gates"]))

    def test_write_report_saves_raw_payloads_and_strips_summary_payload(self):
        with tempfile.TemporaryDirectory() as temp:
            output_dir = Path(temp)
            summary = {
                "mode": "run",
                "scenario": "codec-v5-publish-roundtrip",
                "scenario_metadata": runner.scenario_metadata(self.scenario()),
                "quality": {"status": "pass", "gates": []},
                "runs": [
                    {
                        "ok": True,
                        "run_id": "run-1",
                        "command": ["cargo"],
                        "returncode": 0,
                        "stderr": "",
                        "metrics": {"messages_sec": 1.0},
                        "payload": self.payload(),
                        "is_warmup": False,
                    }
                ],
                "summary": {},
            }

            runner.write_report(output_dir, summary)
            written = json.loads((output_dir / "summary.json").read_text())
            raw_path = output_dir / written["runs"][0]["raw_path"]
            raw = json.loads(raw_path.read_text())

        self.assertNotIn("payload", written["runs"][0])
        self.assertEqual(raw["payload"]["metrics"]["messages_sec"], 10.0)

    def test_scenario_file_hash_is_sha256(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "scenario.toml"
            path.write_text("name = 'x'\n", encoding="utf-8")

            digest = runner.scenario_file_hash(path)

        self.assertEqual(len(digest), 64)

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
        self.assertIn("mad_pct", summary["metrics"]["messages_sec"])


if __name__ == "__main__":
    unittest.main()
