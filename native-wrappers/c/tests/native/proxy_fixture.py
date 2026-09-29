"""Local authenticated proxies that tunnel to the selected fixture endpoint."""

from __future__ import annotations

import base64
import contextlib
import socket
import ssl
import struct
import threading
from collections.abc import Callable


class Proxy:
    def __init__(
        self,
        ports: set[int],
        *,
        tls: ssl.SSLContext | None = None,
        failure: str | None = None,
        observe_tunnel: Callable[[int, int, str], None] | None = None,
    ) -> None:
        self.ports = ports
        self.tls = tls
        self.failure = failure
        self.observe_tunnel = observe_tunnel
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen()
        self.listener.settimeout(0.2)
        self.port = self.listener.getsockname()[1]
        self.stopping = threading.Event()
        self.failures: list[str] = []
        self.threads: list[threading.Thread] = []
        self.attempts = 0
        self.lock = threading.Lock()
        self.accept_thread = threading.Thread(target=self.accept, daemon=True)

    def start(self) -> None:
        self.accept_thread.start()

    def stop(self) -> None:
        self.stopping.set()
        self.listener.close()
        self.accept_thread.join(3)
        for thread in self.threads:
            thread.join(3)
            if thread.is_alive():
                self.failures.append("proxy worker survived shutdown")
        if self.accept_thread.is_alive():
            self.failures.append("proxy listener survived shutdown")
        if self.failures:
            raise RuntimeError("; ".join(self.failures))

    def accept(self) -> None:
        while not self.stopping.is_set():
            try:
                stream, _ = self.listener.accept()
            except OSError:
                continue
            stream.settimeout(5)
            with self.lock:
                self.attempts += 1
                attempt = self.attempts
            thread = threading.Thread(target=self.serve, args=(stream, attempt), daemon=True)
            self.threads.append(thread)
            thread.start()

    @staticmethod
    def read(stream: socket.socket, length: int) -> bytes:
        result = bytearray()
        while len(result) < length:
            chunk = stream.recv(length - len(result))
            if not chunk:
                raise ConnectionError("proxy peer closed")
            result.extend(chunk)
        return bytes(result)

    def serve(self, stream: socket.socket, attempt: int) -> None:
        upstream = None
        pump = None
        try:
            if self.tls is not None:
                stream = self.tls.wrap_socket(stream, server_side=True)
            first = self.read(stream, 1)
            if first == b"C":
                request = bytearray(first)
                while not request.endswith(b"\r\n\r\n"):
                    request.extend(self.read(stream, 1))
                    if len(request) > 4096:
                        raise AssertionError("proxy request exceeded its bound")
                lines = bytes(request).split(b"\r\n")
                method, authority, version = lines[0].split(b" ")
                if method != b"CONNECT" or version != b"HTTP/1.1":
                    raise AssertionError("invalid CONNECT request")
                authorization = b"Proxy-Authorization: Basic " + base64.b64encode(
                    b"proxy-private-user:proxy-private-password"
                )
                if authorization not in lines:
                    raise AssertionError("proxy credentials changed")
                host, port_text = authority.rsplit(b":", 1)
                port = int(port_text)
                http = True
            elif first == b"\x05":
                methods = self.read(stream, self.read(stream, 1)[0])
                if 2 not in methods:
                    raise AssertionError("SOCKS credentials were not requested")
                stream.sendall(b"\x05\x02")
                if self.read(stream, 1) != b"\x01":
                    raise AssertionError("invalid SOCKS credential version")
                username = self.read(stream, self.read(stream, 1)[0])
                password = self.read(stream, self.read(stream, 1)[0])
                if username != b"proxy-private-user" or password != b"proxy-private-password":
                    raise AssertionError("SOCKS credentials changed")
                stream.sendall(b"\x01\x00")
                if self.read(stream, 4) != b"\x05\x01\x00\x03":
                    raise AssertionError("SOCKS remote DNS target changed")
                host = self.read(stream, self.read(stream, 1)[0])
                port = struct.unpack("!H", self.read(stream, 2))[0]
                http = False
            else:
                raise AssertionError("configured proxy was bypassed")
            if host != b"localhost" or port not in self.ports:
                raise AssertionError("proxy target escaped the fixture endpoints")
            if self.failure == "timeout":
                # The client's total connection deadline must cancel negotiation.
                while stream.recv(1):
                    pass
                return
            if self.failure == "recover" and attempt % 2 == 1:
                stream.sendall(
                    b"HTTP/1.1 407 Authentication Required\r\n\r\n"
                    if http
                    else b"\x05\x01\x00\x01\x7f\x00\x00\x01\x00\x00"
                )
                return
            upstream = socket.create_connection(("127.0.0.1", port), timeout=5)
            if self.observe_tunnel is not None:
                self.observe_tunnel(upstream.getsockname()[1], port, "http" if http else "socks5")
            stream.sendall(
                b"HTTP/1.1 200 Connection Established\r\n\r\n" if http else b"\x05\x00\x00\x01\x7f\x00\x00\x01\x00\x00"
            )
            pump = threading.Thread(target=self.relay, args=(upstream, stream), daemon=True)
            pump.start()
            self.relay(stream, upstream)
        except (OSError, TimeoutError):
            # Trust rejection and cancellation are exercised by the client.
            pass
        except Exception:
            with self.lock:
                self.failures.append("proxy negotiation violated the fixture contract")
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
                        self.failures.append("proxy relay survived shutdown")
            stream.close()
            if upstream is not None:
                upstream.close()

    @staticmethod
    def relay(source: socket.socket, destination: socket.socket) -> None:
        try:
            while data := source.recv(16384):
                destination.sendall(data)
        except OSError:
            pass
        finally:
            with contextlib.suppress(OSError):
                destination.shutdown(socket.SHUT_WR)
