"""Keep expected TLS aborts separate from unexpected broker failures."""

import ssl
import unittest
from unittest.mock import MagicMock, patch

from broker_fixture import Broker, Connection


class BrokerFailureTests(unittest.TestCase):
    def test_event_burst_waits_for_trigger_completion_in_each_queue_case(self):
        broker = Broker()
        self.addCleanup(broker.listener.close)
        topic = b"native/events/burst"
        for protocol in (4, 5):
            for mode in ("retained", "backpressure"):
                with self.subTest(protocol=protocol, mode=mode):
                    name = f"native-events-{mode}-v{protocol}"
                    connection = Connection(MagicMock(), protocol, name.encode("ascii"))
                    body = len(topic).to_bytes(2, "big") + topic + (b"\x00" if protocol == 5 else b"")
                    with patch.object(connection, "publish") as publish:

                        def ready(_barrier_name):
                            publish.assert_not_called()

                        with patch.object(broker, "barrier", side_effect=ready) as barrier:
                            self.assertTrue(broker.handle_publish(connection, 0, body))
                        barrier.assert_called_once_with(name)
                        self.assertEqual(publish.call_count, 512)

    def test_integration_overflow_waits_for_subscription_observation_and_socket_close(self):
        broker = Broker()
        self.addCleanup(broker.listener.close)
        stream = MagicMock()
        stream.recv.return_value = b""
        connection = Connection(stream, 4, b"native-overflow")
        topic = b"rumqttc/native/overflow"
        body = b"\x00\x01" + len(topic).to_bytes(2, "big") + topic + b"\x00"

        def ready(name):
            self.assertEqual(name, "integration-overflow-ready")
            # Only SUBACK is sent before the C consumer observes completion.
            self.assertEqual(stream.sendall.call_count, 1)
            stream.recv.assert_not_called()

        def closed(name):
            self.assertEqual(name, "integration-overflow-closed")
            self.assertEqual(stream.sendall.call_count, 9)
            stream.recv.assert_called_once_with(1)

        with (
            patch.object(broker, "barrier", side_effect=ready) as barrier,
            patch.object(broker, "signal", side_effect=closed) as signal,
        ):
            broker.handle_subscribe(connection, body)
        barrier.assert_called_once_with("integration-overflow-ready")
        signal.assert_called_once_with("integration-overflow-closed")

    def serve_error(self, error, tls):
        broker = Broker(ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER) if tls else None)
        self.addCleanup(broker.listener.close)
        stream = MagicMock()
        stream.recv.return_value = b"\x10"
        with patch("broker_fixture.read_frame", side_effect=error):
            broker.serve(stream)
        stream.close.assert_called_once()
        return broker

    def test_tls_eof_before_connect_is_a_rejected_handshake(self):
        broker = self.serve_error(ssl.SSLEOFError(8, "peer rejected handshake"), True)
        self.assertEqual(broker.failures, [])
        self.assertEqual(broker.tls_disconnects_before_connect, 1)

    def test_plain_stream_tls_eof_is_still_a_failure(self):
        broker = self.serve_error(ssl.SSLEOFError(8, "unexpected EOF"), False)
        self.assertEqual(len(broker.failures), 1)

    def test_other_tls_errors_are_still_failures(self):
        broker = self.serve_error(ssl.SSLError(1, "unexpected TLS error"), True)
        self.assertEqual(len(broker.failures), 1)

    def test_tls_eof_after_connect_is_still_a_failure(self):
        broker = Broker(ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER))
        self.addCleanup(broker.listener.close)
        stream = MagicMock()
        connect = (1, 0, b"\x00\x04MQTT\x04\x02\x00\x3c\x00\x04test")
        with patch("broker_fixture.read_frame", side_effect=[connect, ssl.SSLEOFError(8, "unexpected EOF")]):
            broker.serve(stream)
        self.assertIn(b"test", broker.client_ids)
        self.assertEqual(len(broker.failures), 1)
        self.assertIn("SSLEOFError", broker.failures[0])


if __name__ == "__main__":
    unittest.main()
