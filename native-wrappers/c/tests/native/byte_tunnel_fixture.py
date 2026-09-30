"""A custom byte tunnel, independent of HTTP/SOCKS/TLS/MQTT framing."""
import contextlib
import socket
import threading
from proxy_fixture import Proxy


class ByteTunnel(Proxy):
    def serve(self, stream: socket.socket, attempt: int) -> None:
        upstream = None
        pump = None
        try:
            request = bytearray()
            while not request.endswith(b"\n"):
                request.extend(self.read(stream, 1))
                if len(request) > 4096:
                    raise AssertionError("custom tunnel request exceeded its bound")
            if not request.startswith(b"RUMQTTC-TUNNEL "):
                raise AssertionError("custom tunnel prefix missing")
            host, port = request[len(b"RUMQTTC-TUNNEL "):-1].rsplit(b":", 1)
            if host not in (b"localhost", b"127.0.0.1") or int(port) not in self.ports:
                raise AssertionError("custom tunnel target is outside the fixture")
            upstream = socket.create_connection(("127.0.0.1", int(port)), timeout=5)
            stream.settimeout(None)
            upstream.settimeout(None)
            pump = threading.Thread(target=self.relay, args=(upstream, stream), daemon=True)
            pump.start()
            self.relay(stream, upstream)
        except (OSError, TimeoutError):
            pass  # Client cancellation and TLS trust rejection are expected.
        except Exception as error:
            with self.lock:
                self.failures.append(f"custom tunnel negotiation failed: {error}")
        finally:
            with contextlib.suppress(OSError):
                stream.shutdown(socket.SHUT_RDWR)
            if upstream is not None:
                with contextlib.suppress(OSError):
                    upstream.shutdown(socket.SHUT_RDWR)
            if pump is not None:
                pump.join(3)
                if pump.is_alive():
                    with self.lock:
                        self.failures.append("custom tunnel relay survived shutdown")
            stream.close()
            if upstream is not None:
                upstream.close()
