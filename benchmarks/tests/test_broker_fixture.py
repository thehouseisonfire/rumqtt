import hashlib
import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

FIXTURE_PATH = Path(__file__).resolve().parents[1] / "broker_fixture.py"
REPO_ROOT = FIXTURE_PATH.parents[1]
SPEC = importlib.util.spec_from_file_location("broker_fixture", FIXTURE_PATH)
if SPEC is None or SPEC.loader is None:
    raise ImportError(f"cannot load spec from {FIXTURE_PATH}")
broker_fixture = importlib.util.module_from_spec(SPEC)
sys.modules["broker_fixture"] = broker_fixture
SPEC.loader.exec_module(broker_fixture)


class BrokerFixtureTests(unittest.TestCase):
    def scenario(self, name, *, transport="tcp", requires_broker=True, qos=1):
        return broker_fixture.ScenarioRef(
            name=name,
            path=REPO_ROOT / "benchmarks" / "scenarios" / f"{name}.toml",
            data={
                "name": name,
                "requires_broker": requires_broker,
                "transport": transport,
                "args": {"qos": qos},
            },
        )

    def test_pinned_production_broker_images_use_exact_versions(self):
        self.assertEqual(broker_fixture.PINNED_MOSQUITTO_IMAGE, "eclipse-mosquitto:2.0.22")
        self.assertEqual(broker_fixture.PINNED_EMQX_IMAGE, "emqx/emqx:5.9.3")

    def test_mosquitto_image_override_defaults_to_pin_and_honors_environment(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            self.assertEqual(
                broker_fixture.docker_image_from_env(),
                broker_fixture.PINNED_MOSQUITTO_IMAGE,
            )
        with mock.patch.dict(
            os.environ,
            {"RUMQTT_BENCH_MOSQUITTO_IMAGE": "example.test/mosquitto:custom"},
        ):
            self.assertEqual(
                broker_fixture.docker_image_from_env(),
                "example.test/mosquitto:custom",
            )

    def test_mosquitto_config_includes_tcp_tls_and_websocket_listeners(self):
        config = broker_fixture.mosquitto_config(
            tcp_port=1883,
            tls_port=8883,
            websocket_port=9001,
            certfile="/certs/server.crt",
            keyfile="/certs/server.key",
        )

        self.assertIn("listener 1883", config)
        self.assertIn("listener 8883", config)
        self.assertIn("certfile /certs/server.crt", config)
        self.assertIn("keyfile /certs/server.key", config)
        self.assertIn("listener 9001", config)
        self.assertIn("protocol websockets", config)
        self.assertEqual(
            broker_fixture.mosquitto_effective_transports(config),
            ["tcp", "tls", "websocket"],
        )

    def test_system_config_can_omit_websocket_listener(self):
        with tempfile.TemporaryDirectory() as temp:
            config = broker_fixture.build_system_config(
                broker_fixture.BrokerPorts(tcp=1883, tls=8883, websocket=9001),
                Path(temp),
                include_websocket=False,
            )

        self.assertIn("listener 1883 127.0.0.1", config)
        self.assertIn("listener 8883 127.0.0.1", config)
        self.assertNotIn("protocol websockets", config)
        self.assertEqual(
            broker_fixture.mosquitto_effective_transports(config),
            ["tcp", "tls"],
        )

    def test_system_config_binds_websocket_listener_to_loopback(self):
        with tempfile.TemporaryDirectory() as temp:
            config = broker_fixture.build_system_config(
                broker_fixture.BrokerPorts(tcp=1883, tls=8883, websocket=9001),
                Path(temp),
                include_websocket=True,
            )

        self.assertIn("listener 1883 127.0.0.1", config)
        self.assertIn("listener 8883 127.0.0.1", config)
        self.assertIn("listener 9001 127.0.0.1", config)

    def test_docker_command_maps_ports_and_mounts_config(self):
        with tempfile.TemporaryDirectory() as temp:
            command = broker_fixture.build_docker_run_command(
                container_name="rumqtt-bench-test",
                image="eclipse-mosquitto:2.0",
                ports=broker_fixture.BrokerPorts(tcp=11883, tls=18883, websocket=19001),
                config_dir=Path(temp) / "config",
            )

        self.assertEqual(command[:5], ["docker", "run", "-d", "--name", "rumqtt-bench-test"])
        self.assertIn("127.0.0.1:11883:1883", command)
        self.assertIn("127.0.0.1:18883:8883", command)
        self.assertIn("127.0.0.1:19001:9001", command)
        self.assertIn("eclipse-mosquitto:2.0", command)
        self.assertTrue(any(value.endswith(":/mosquitto/config:ro") for value in command))

    def test_persisted_mosquitto_config_is_exact_and_hashed(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            paths = broker_fixture.BrokerPaths(
                root=root,
                config_dir=root,
                config=root / "mosquitto.conf",
                ca_cert=root / "ca.crt",
                cert=root / "server.crt",
                key=root / "server.key",
            )
            contents = b"persistence false\nallow_anonymous true\n"
            paths.config.write_bytes(contents)
            metadata = broker_fixture.persist_mosquitto_config(paths, root / "out")
            persisted = root / "out" / metadata["path"]
            self.assertEqual(persisted.read_bytes(), contents)
            self.assertEqual(metadata["sha256"], hashlib.sha256(contents).hexdigest())

    def test_normalized_broker_metadata_is_stable_and_secret_free(self):
        metadata = broker_fixture.normalized_broker_metadata(
            broker_kind="emqx",
            metadata={
                "backend": "docker-emqx",
                "image": broker_fixture.PINNED_EMQX_IMAGE,
                "image_digest": "emqx@sha256:test",
                "environment_overrides": {"Z": "last", "A": "first"},
            },
            ports=broker_fixture.BrokerPorts(tcp=11883, tls=18883, websocket=19001),
            selected_transports=["tcp"],
            effective_transports=["tcp"],
            config=None,
        )
        self.assertEqual(list(metadata["environment_overrides"]), ["A", "Z"])
        self.assertEqual(metadata["listeners"][0]["port"], 11883)
        self.assertEqual(metadata["authentication"]["mode"], "anonymous")
        self.assertNotIn("key", str(metadata).lower())

    def test_broker_metadata_reports_effective_mosquitto_listeners(self):
        metadata = broker_fixture.normalized_broker_metadata(
            broker_kind="mosquitto",
            metadata={
                "backend": "docker",
                "image": broker_fixture.PINNED_MOSQUITTO_IMAGE,
                "image_digest": "mosquitto@sha256:test",
            },
            ports=broker_fixture.BrokerPorts(tcp=11883, tls=18883, websocket=19001),
            selected_transports=["tcp"],
            effective_transports=["tcp", "tls", "websocket"],
            config={"path": "broker-config/mosquitto.conf", "sha256": "abc"},
        )
        self.assertEqual(metadata["selected_transports"], ["tcp"])
        self.assertEqual(
            metadata["active_transports"],
            ["tcp", "tls", "websocket"],
        )
        self.assertEqual(
            [listener["transport"] for listener in metadata["listeners"]],
            ["tcp", "tls", "websocket"],
        )
        self.assertEqual(
            metadata["tls_certificate_mode"],
            "generated-self-signed-private-ca",
        )

    def test_emqx_command_records_every_sorted_environment_override(self):
        command = broker_fixture.build_emqx_docker_run_command(
            container_name="emqx-test",
            image=broker_fixture.PINNED_EMQX_IMAGE,
            ports=broker_fixture.BrokerPorts(tcp=11883, tls=18883, websocket=19001),
        )
        assignments = [command[index + 1] for index, value in enumerate(command) if value == "-e"]
        self.assertEqual(
            assignments,
            [f"{key}={value}" for key, value in sorted(broker_fixture.EMQX_ENV_OVERRIDES.items())],
        )

    def test_select_scenarios_filters_transport_explicit_scenario_and_soak(self):
        scenarios = [
            self.scenario("client-v4-throughput-qos1-1kib-1p1s", transport="tcp"),
            self.scenario("client-v4-throughput-websocket-qos1-1kib-1p1s", transport="websocket"),
            self.scenario("client-v4-soak-qos1-1kib-1p1s", transport="tcp"),
            self.scenario("codec-v4-publish-roundtrip", requires_broker=False),
        ]

        selected, skipped = broker_fixture.select_scenarios(
            scenarios,
            transport="websocket",
            requested=[
                "client-v4-throughput-websocket-qos1-1kib-1p1s",
                "client-v4-throughput-qos1-1kib-1p1s",
                "missing-scenario",
            ],
            include_soak=False,
        )

        self.assertEqual(
            [scenario.name for scenario in selected],
            ["client-v4-throughput-websocket-qos1-1kib-1p1s"],
        )
        skipped_by_name = {entry["name"]: entry["reason"] for entry in skipped}
        self.assertEqual(skipped_by_name["client-v4-throughput-qos1-1kib-1p1s"], "transport_mismatch")
        self.assertEqual(skipped_by_name["missing-scenario"], "not_found")

    def test_select_scenarios_skips_soaks_by_default(self):
        selected, skipped = broker_fixture.select_scenarios(
            [self.scenario("client-v5-soak-qos1-1kib-1p1s")],
            transport="all",
            requested=[],
            include_soak=False,
        )

        self.assertEqual(selected, [])
        self.assertEqual(skipped, [{"name": "client-v5-soak-qos1-1kib-1p1s", "reason": "soak_skipped"}])

    def test_synthetic_backend_rejects_non_tcp_and_qos2_scenarios(self):
        with self.assertRaisesRegex(broker_fixture.FixtureError, "TCP QoS 0/1"):
            broker_fixture.validate_synthetic_scenarios(
                [
                    self.scenario("tls", transport="tls"),
                    self.scenario("qos2", qos=2),
                ]
            )

        broker_fixture.validate_synthetic_scenarios(
            [
                self.scenario("v4-qos0", qos=0),
                self.scenario("v5-qos1", qos=1),
            ]
        )

    def test_runner_command_uses_broker_url_ca_cert_and_dev_profile(self):
        with tempfile.TemporaryDirectory() as temp:
            scenario = self.scenario("client-v5-throughput-tls-qos1-1kib-1p1s", transport="tls")
            ca_cert = Path(temp) / "ca.crt"
            command = broker_fixture.build_runner_command(
                scenario=scenario,
                broker_url="mqtts://localhost:18883",
                output_dir=Path(temp) / "out",
                runs=1,
                warmup_runs=0,
                cargo_profile="dev",
                timeout_sec=123,
                ca_cert=ca_cert,
            )

        self.assertEqual(command[0], sys.executable)
        self.assertEqual(Path(command[1]), broker_fixture.BENCHMARKS_DIR / "runner.py")
        self.assertIn("mqtts://localhost:18883", command)
        self.assertIn("--ca-cert", command)
        self.assertIn(str(ca_cert), command)
        self.assertIn("dev", command)
        self.assertIn("123", command)

    def test_runner_command_forwards_ca_cert_to_matched_comparison(self):
        with tempfile.TemporaryDirectory() as temp:
            scenario = self.scenario("matched-v5-throughput-tls", transport="tls")
            scenario.data["group"] = "matched"
            ca_cert = Path(temp) / "ca.crt"
            command = broker_fixture.build_runner_command(
                scenario=scenario,
                broker_url="mqtts://localhost:18883",
                output_dir=Path(temp) / "out",
                runs=1,
                warmup_runs=0,
                cargo_profile="dev",
                timeout_sec=123,
                ca_cert=ca_cert,
            )

        self.assertIn("compare-libraries", command)
        self.assertEqual(command[command.index("--ca-cert") + 1], str(ca_cert))

    def test_runner_command_omits_ca_cert_for_websocket(self):
        with tempfile.TemporaryDirectory() as temp:
            scenario = self.scenario(
                "client-v4-throughput-websocket-qos1-1kib-1p1s",
                transport="websocket",
            )
            command = broker_fixture.build_runner_command(
                scenario=scenario,
                broker_url="ws://127.0.0.1:19001/mqtt",
                output_dir=Path(temp) / "out",
                runs=1,
                warmup_runs=0,
                cargo_profile="dev",
                timeout_sec=123,
                ca_cert=None,
            )

        self.assertIn("ws://127.0.0.1:19001/mqtt", command)
        self.assertNotIn("--ca-cert", command)

    def test_websocket_scenario_declares_feature_gate(self):
        _, scenario = broker_fixture.runner.load_scenario(
            REPO_ROOT,
            "client-v4-throughput-websocket-qos1-1kib-1p1s",
        )
        command = broker_fixture.runner.scenario_command(
            scenario,
            run_id="run-1",
            broker_url="ws://127.0.0.1:19001/mqtt",
            ca_cert=None,
            cargo_profile="dev",
        )

        self.assertIn("--features", command)
        self.assertIn("websocket", command)
        self.assertNotIn("--release", command)

    def test_system_mosquitto_websocket_probe_reports_unsupported(self):
        with tempfile.TemporaryDirectory() as temp:
            if os.name == "nt":
                fake_mosquitto = Path(temp) / "fake_mosquitto.bat"
                fake_mosquitto.write_text(
                    "@echo off\r\necho Error: Websockets support not available. 1>&2\r\nexit /b 1\r\n",
                    encoding="utf-8",
                )
            else:
                fake_mosquitto = Path(temp) / "fake_mosquitto"
                fake_mosquitto.write_text(
                    "#!/usr/bin/env python3\n"
                    "import sys\n"
                    "print('Error: Websockets support not available.', file=sys.stderr)\n"
                    "raise SystemExit(1)\n",
                    encoding="utf-8",
                )
                fake_mosquitto.chmod(0o755)

            supported, message = broker_fixture.probe_system_mosquitto_websockets(str(fake_mosquitto))

        self.assertFalse(supported)
        self.assertIn("Websockets support not available", message)


if __name__ == "__main__":
    unittest.main()
