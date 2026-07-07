import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path


FIXTURE_PATH = Path(__file__).resolve().parents[1] / "broker_fixture.py"
REPO_ROOT = FIXTURE_PATH.parents[1]
SPEC = importlib.util.spec_from_file_location("broker_fixture", FIXTURE_PATH)
broker_fixture = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules["broker_fixture"] = broker_fixture
SPEC.loader.exec_module(broker_fixture)


class BrokerFixtureTests(unittest.TestCase):
    def scenario(self, name, *, transport="tcp", requires_broker=True):
        return broker_fixture.ScenarioRef(
            name=name,
            path=REPO_ROOT / "benchmarks" / "scenarios" / f"{name}.toml",
            data={
                "name": name,
                "requires_broker": requires_broker,
                "transport": transport,
            },
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
        self.assertIn("benchmarks/runner.py", command[1])
        self.assertIn("mqtts://localhost:18883", command)
        self.assertIn("--ca-cert", command)
        self.assertIn(str(ca_cert), command)
        self.assertIn("dev", command)
        self.assertIn("123", command)

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
            fake_mosquitto = Path(temp) / "fake_mosquitto.py"
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
