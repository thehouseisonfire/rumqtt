import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


RUNNER_PATH = Path(__file__).resolve().parents[1] / "runner.py"
REPO_ROOT = RUNNER_PATH.parents[1]
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

    def test_persistence_scenario_routes_to_nested_workspace(self):
        command = runner.scenario_command(
            self.scenario(
                name="persistence-envelope-1mib",
                group="persistence",
                command="envelope",
                primary_metric="operations_sec",
                args={"protocol": "v5", "payload_size": 1048576},
            ),
            run_id="run-1",
            broker_url=None,
            ca_cert=None,
        )

        self.assertIn("session-store-file/Cargo.toml", command)
        self.assertIn("session-store-file-benchmarks", command)
        self.assertIn("rumqtt-session-store-file-bench", command)

    def test_named_persistence_scenario_uses_nested_catalog_and_results(self):
        path, scenario = runner.load_scenario(REPO_ROOT, "persistence-envelope-1mib")

        self.assertEqual(
            path,
            REPO_ROOT
            / "session-store-file"
            / "benchmarks"
            / "scenarios"
            / "persistence-envelope-1mib.toml",
        )
        output = runner.default_output_dir(REPO_ROOT, "runs", scenario)
        self.assertEqual(
            output.parents[1], REPO_ROOT / "session-store-file" / "benchmarks" / "results"
        )

    def test_scenario_command_includes_declared_cargo_features(self):
        command = runner.scenario_command(
            self.scenario(
                name="options-v5-parse-url",
                group="options",
                command="parse-url",
                primary_metric="parses_sec",
                cargo_features=["url"],
                args={"protocol": "v5", "parses": 1000},
            ),
            run_id="run-1",
            broker_url=None,
            ca_cert=None,
        )

        self.assertIn("--features", command)
        self.assertIn("url", command)
        self.assertIn("options", command)
        self.assertIn("parse-url", command)

    def test_external_command_translates_v5_throughput_scenario(self):
        scenario = self.scenario(
            name="client-v5-throughput-qos1",
            group="client",
            command="throughput",
            primary_metric="throughput_msg_sec",
            requires_broker=True,
            transport="tcp",
            args={
                "protocol": "v5",
                "duration_sec": 6,
                "warmup_sec": 2,
                "payload_size": 64,
                "qos": 1,
                "topic": "bench/external",
                "publishers": 2,
                "subscribers": 3,
            },
        )
        comparable = runner.external_comparison_scenario(scenario)

        command = runner.external_command(
            comparable,
            external_bin="/usr/bin/mqttv5",
            broker_url="mqtt://127.0.0.1:1883",
            ca_cert=None,
            run_id="external-run",
        )

        self.assertEqual(command[:3], ["/usr/bin/mqttv5", "bench", "--mode"])
        self.assertIn("throughput", command)
        self.assertIn("bench/external/#", command)
        self.assertEqual(comparable["args"]["filter"], "bench/external/#")
        self.assertNotIn("filter", scenario["args"])

    def test_external_comparison_rejects_unrepresentable_latency_rate(self):
        scenario = self.scenario(
            name="client-v5-latency-rate-100",
            group="client",
            command="latency",
            primary_metric="p99_us",
            requires_broker=True,
            transport="tcp",
            args={"protocol": "v5", "rate": 100},
        )

        with self.assertRaisesRegex(RuntimeError, "fixed 1000"):
            runner.validate_external_scenario(scenario)

    def test_external_throughput_output_normalizes_to_v1_metrics(self):
        scenario = self.scenario(
            name="client-v5-throughput-qos1",
            group="client",
            command="throughput",
            primary_metric="throughput_msg_sec",
            requires_broker=True,
            transport="tcp",
            args={"protocol": "v5"},
        )

        payload = runner.normalize_external_payload(
            {
                "mode": "throughput",
                "config": {"transport": "mqtt"},
                "results": {
                    "published": 12,
                    "received": 10,
                    "elapsed_secs": 2.0,
                    "throughput_avg": 5.0,
                    "samples": [4, 6],
                },
            },
            scenario=scenario,
            run_id="external-run",
            started_at_unix=1,
            finished_at_unix=3,
            external_bin="/usr/bin/mqttv5",
            version="mqttv5 1.0",
        )

        self.assertEqual(payload["schema_version"], 1)
        self.assertEqual(payload["metrics"]["throughput_msg_sec"], 5.0)
        self.assertEqual(payload["samples"]["received_per_sec"], [4.0, 6.0])
        self.assertEqual(payload["environment"]["external_version"], "mqttv5 1.0")

    def test_external_json_is_extracted_after_progress_output(self):
        output = """\
connecting...
warming up...
running benchmark...
{
  "mode": "throughput",
  "config": {"transport": "mqtt"},
  "results": {"throughput_avg": 5.0}
}
"""

        data = runner.read_external_json(output)

        self.assertEqual(data["mode"], "throughput")
        self.assertEqual(data["results"]["throughput_avg"], 5.0)

    def test_external_json_rejects_trailing_non_json_output(self):
        with self.assertRaisesRegex(RuntimeError, "did not end with a JSON object"):
            runner.read_external_json('progress\n{"mode": "throughput"}\ndone\n')

    def test_all_real_scenarios_validate(self):
        scenario_files = sorted((REPO_ROOT / "benchmarks" / "scenarios").glob("*.toml"))
        scenario_files.extend(
            sorted(
                (REPO_ROOT / "session-store-file" / "benchmarks" / "scenarios").glob("*.toml")
            )
        )

        self.assertGreaterEqual(len(scenario_files), 60)
        for path in scenario_files:
            with self.subTest(path=path.name):
                loaded_path, scenario = runner.load_scenario(REPO_ROOT, str(path))
                self.assertEqual(loaded_path, path)
                self.assertEqual(scenario["name"], path.stem)

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

    def test_broker_transport_must_match_scenario(self):
        scenario = self.scenario(
            group="client",
            command="throughput",
            requires_broker=True,
            primary_metric="throughput_msg_sec",
            transport="tls",
        )

        with self.assertRaisesRegex(RuntimeError, "expects tls"):
            runner.validate_broker_requirement(scenario, "mqtt://127.0.0.1:1883")

        runner.validate_broker_requirement(scenario, "mqtts://127.0.0.1:8883")

    def test_tcp_transport_rejects_tcp_url_scheme_not_supported_by_binary(self):
        scenario = self.scenario(
            group="client",
            command="throughput",
            requires_broker=True,
            primary_metric="throughput_msg_sec",
            transport="tcp",
        )

        with self.assertRaisesRegex(RuntimeError, "use one of: mqtt://"):
            runner.validate_broker_requirement(scenario, "tcp://127.0.0.1:1883")

        runner.validate_broker_requirement(scenario, "mqtt://127.0.0.1:1883")

    def test_matched_scenario_rejects_websocket_transport(self):
        scenario = self.scenario(
            group="matched",
            command="throughput",
            requires_broker=True,
            primary_metric="throughput_msg_sec",
            transport="websocket",
        )

        with self.assertRaisesRegex(RuntimeError, "matched transport must be one of: tcp, tls"):
            runner.validate_scenario(Path("scenario.toml"), scenario)

    def test_matched_tls_rejects_ssl_alias_not_supported_by_adapter(self):
        scenario = self.scenario(
            group="matched",
            command="throughput",
            requires_broker=True,
            primary_metric="throughput_msg_sec",
            transport="tls",
        )

        with self.assertRaisesRegex(RuntimeError, "use one of: mqtts://"):
            runner.validate_broker_requirement(scenario, "ssl://127.0.0.1:8883")

        runner.validate_broker_requirement(scenario, "mqtts://127.0.0.1:8883")

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
            primary_metric="p99_us",
            higher_is_better=False,
            requires_broker=True,
        )

        comparison = runner.compare_summaries(
            [{"ok": True, "metrics": {"p99_us": 100.0}}],
            [{"ok": True, "metrics": {"p99_us": 80.0}}],
            scenario=scenario,
            bootstrap_samples=100,
            confidence=0.95,
        )

        self.assertFalse(comparison["p99_us"]["higher_is_better"])
        self.assertEqual(comparison["p99_us"]["classification"], "improvement")
        self.assertEqual(comparison["p99_us"]["paired_sample_count"], 1)

    def test_latency_us_metrics_are_lower_is_better_even_when_secondary(self):
        comparison = runner.compare_summaries(
            [{"ok": True, "metrics": {"throughput_msg_sec": 100.0, "p99_us": 100.0}}],
            [{"ok": True, "metrics": {"throughput_msg_sec": 105.0, "p99_us": 80.0}}],
            scenario=self.scenario(
                group="client",
                command="throughput",
                primary_metric="throughput_msg_sec",
                requires_broker=True,
            ),
            bootstrap_samples=100,
            confidence=0.95,
        )

        self.assertFalse(comparison["p99_us"]["higher_is_better"])
        self.assertEqual(comparison["p99_us"]["classification"], "improvement")

    def test_rss_byte_metrics_are_lower_is_better(self):
        comparison = runner.compare_summaries(
            [{"ok": True, "metrics": {"throughput_msg_sec": 100.0, "rss_max_bytes": 100.0}}],
            [{"ok": True, "metrics": {"throughput_msg_sec": 100.0, "rss_max_bytes": 120.0}}],
            scenario=self.scenario(
                group="client",
                command="throughput",
                primary_metric="throughput_msg_sec",
                requires_broker=True,
            ),
            bootstrap_samples=100,
            confidence=0.95,
        )

        self.assertFalse(comparison["rss_max_bytes"]["higher_is_better"])
        self.assertEqual(comparison["rss_max_bytes"]["classification"], "regression")

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

    def test_summary_environment_preserves_v2_provenance_and_non_null_fallbacks(self):
        environment = runner.summary_environment(
            REPO_ROOT,
            [
                {
                    "payload": {
                        "environment": {
                            "rustc": "rustc matched",
                            "target": "x86_64-unknown-linux-gnu",
                            "os": "linux",
                            "arch": "x86_64",
                            "cpu_count": None,
                            "logical_cpu_count": 16,
                            "cpu_model": "Benchmark CPU",
                            "total_memory_bytes": 32_000_000_000,
                            "allocator": "system-counting",
                            "cargo_features": ["alloc-metrics"],
                            "optimization_profile": "release",
                            "library": "mqtt5",
                            "library_version": "0.37.2",
                        }
                    }
                }
            ],
        )

        self.assertIsNotNone(environment["cpu_count"])
        self.assertEqual(environment["logical_cpu_count"], 16)
        self.assertEqual(environment["target"], "x86_64-unknown-linux-gnu")
        self.assertEqual(environment["cpu_model"], "Benchmark CPU")
        self.assertEqual(environment["total_memory_bytes"], 32_000_000_000)
        self.assertEqual(environment["allocator"], "system-counting")
        self.assertEqual(environment["cargo_features"], ["alloc-metrics"])
        self.assertEqual(environment["optimization_profile"], "release")
        self.assertEqual(environment["library"], "mqtt5")
        self.assertEqual(environment["library_version"], "0.37.2")

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

    def test_matched_command_selects_one_backend_in_one_process(self):
        scenario = self.scenario(
            name="matched",
            group="matched",
            command="throughput",
            requires_broker=True,
            args={"qos": 1, "payload_size": 64},
        )

        command = runner.matched_command(
            scenario,
            client="mqtt5",
            run_id="pair-1",
            broker_url="mqtt://127.0.0.1:1883",
        )

        self.assertIn("rumqtt-library-bench", command)
        self.assertEqual(command[command.index("--client") + 1], "mqtt5")
        self.assertNotIn("rumqttc", command)

    def test_matched_command_forwards_ca_certificate(self):
        scenario = self.scenario(
            name="matched-tls",
            group="matched",
            command="throughput",
            requires_broker=True,
            transport="tls",
            args={"qos": 1, "payload_size": 64},
        )

        command = runner.matched_command(
            scenario,
            client="rumqttc",
            run_id="pair-tls-1",
            broker_url="mqtts://localhost:8883",
            ca_cert="/tmp/fixture-ca.crt",
        )

        self.assertEqual(command[command.index("--ca-cert") + 1], "/tmp/fixture-ca.crt")

    def test_certificate_metadata_resolves_path_and_hashes_contents(self):
        with tempfile.TemporaryDirectory() as temp:
            certificate = Path(temp) / "ca.crt"
            certificate.write_bytes(b"fixture-ca")

            metadata = runner.certificate_metadata(str(certificate))

        self.assertEqual(metadata["path"], str(certificate.resolve()))
        self.assertEqual(
            metadata["sha256"],
            "fca046ca96fabdc57856c287f889f3a2a20dc3192abefa0443ae0e6505595fdf",
        )

    def test_matched_comparison_persists_raw_runs_and_strips_large_fields(self):
        run = {
            "run_id": "pair-1",
            "ok": True,
            "stdout": '{"schema_version": 2}',
            "stderr": "",
            "payload": {"metrics": {"throughput_msg_sec": 10.0}},
            "metrics": {"throughput_msg_sec": 10.0},
        }
        summary = {
            "mode": "compare-libraries",
            "runs": {"rumqttc": [dict(run)], "mqtt5": [dict(run)]},
        }

        with tempfile.TemporaryDirectory() as temp:
            output_dir = Path(temp)
            runner.write_report(output_dir, summary)
            written = json.loads((output_dir / "summary.json").read_text())
            rumqttc_run = written["runs"]["rumqttc"][0]
            raw = json.loads((output_dir / rumqttc_run["raw_path"]).read_text())

        self.assertNotIn("payload", rumqttc_run)
        self.assertNotIn("stdout", rumqttc_run)
        self.assertNotIn("stderr", rumqttc_run)
        self.assertEqual(raw["payload"]["metrics"]["throughput_msg_sec"], 10.0)
        self.assertEqual(raw["stdout"], '{"schema_version": 2}')

    def test_v2_payload_requires_effective_config_and_validity(self):
        scenario = self.scenario(
            name="matched",
            group="matched",
            command="throughput",
            requires_broker=True,
            primary_metric="throughput_msg_sec",
        )
        payload = self.payload({"throughput_msg_sec": 10.0})
        payload.update(
            schema_version=2,
            client="rumqttc",
            effective_config={"publish_completion": "puback"},
            quality={"valid": True},
        )

        runner.validate_benchmark_payload(payload, scenario)

    def test_equivalence_band_requires_the_whole_interval_inside_band(self):
        equivalent = runner.bootstrap_delta(
            [100.0] * 12,
            [103.0] * 12,
            samples=100,
            confidence=0.95,
            rng=runner.random.Random(1),
            higher_is_better=True,
            equivalence_band_pct=5.0,
        )
        outside = runner.bootstrap_delta(
            [100.0] * 12,
            [107.0] * 12,
            samples=100,
            confidence=0.95,
            rng=runner.random.Random(1),
            higher_is_better=True,
            equivalence_band_pct=5.0,
        )

        self.assertEqual(equivalent["classification"], "equivalent")
        self.assertEqual(outside["classification"], "improvement")


if __name__ == "__main__":
    unittest.main()
