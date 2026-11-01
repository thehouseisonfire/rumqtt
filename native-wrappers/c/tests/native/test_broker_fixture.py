"""Keep expected TLS aborts separate from unexpected broker failures."""

import ssl
import unittest
from unittest.mock import MagicMock, patch

from broker_fixture import Broker


class BrokerFailureTests(unittest.TestCase):
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
