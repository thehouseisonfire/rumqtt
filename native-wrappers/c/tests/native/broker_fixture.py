#!/usr/bin/env python3
"""Deterministic MQTT broker fixture and native executable runner."""

from __future__ import annotations

import argparse
import base64
import contextlib
import hashlib
import os
import shlex
import socket
import ssl
import struct
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass, field


class WebSocketStream:
    def __init__(self, stream: socket.socket) -> None:
        self.stream = stream
        self.buffer = bytearray()

    def settimeout(self, timeout: float) -> None:
        self.stream.settimeout(timeout)

    def close(self) -> None:
        self.stream.close()

    def sendall(self, data: bytes) -> None:
        length = len(data)
        header = bytearray((0x82,))
        if length < 126:
            header.append(length)
        elif length <= 0xFFFF:
            header.append(126)
            header.extend(struct.pack("!H", length))
        else:
            header.append(127)
            header.extend(struct.pack("!Q", length))
        self.stream.sendall(header + data)

    def recv(self, length: int) -> bytes:
        while len(self.buffer) < length:
            header = read_exact(self.stream, 2)
            if header is None:
                break
            opcode = header[0] & 0x0F
            masked = bool(header[1] & 0x80)
            payload_length = header[1] & 0x7F
            if payload_length == 126:
                encoded = read_exact(self.stream, 2)
                if encoded is None:
                    break
                payload_length = struct.unpack("!H", encoded)[0]
            elif payload_length == 127:
                encoded = read_exact(self.stream, 8)
                if encoded is None:
                    break
                payload_length = struct.unpack("!Q", encoded)[0]
            mask = read_exact(self.stream, 4) if masked else None
            payload = read_exact(self.stream, payload_length)
            if payload is None:
                break
            if mask is not None:
                payload = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
            if opcode in {0, 2}:
                self.buffer.extend(payload)
            elif opcode == 8:
                break
            elif opcode == 9:
                self.stream.sendall(bytes((0x8A, len(payload))) + payload)
        result = bytes(self.buffer[:length])
        del self.buffer[:length]
        return result


def accept_websocket(stream: socket.socket) -> WebSocketStream:
    request = bytearray()
    while b"\r\n\r\n" not in request:
        chunk = stream.recv(4096)
        if not chunk:
            raise ConnectionError("WebSocket handshake ended early")
        request.extend(chunk)
        if len(request) > 64 * 1024:
            raise ValueError("WebSocket handshake exceeded its bound")
    headers: dict[str, str] = {}
    for line in request.decode("ascii").split("\r\n")[1:]:
        if ":" in line:
            name, value = line.split(":", 1)
            headers[name.strip().lower()] = value.strip()
    key = headers.get("sec-websocket-key")
    if key is None or "mqtt" not in headers.get("sec-websocket-protocol", "").lower():
        raise ValueError("client did not request the MQTT WebSocket subprotocol")
    accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
    stream.sendall(
        b"HTTP/1.1 101 Switching Protocols\r\n"
        b"Upgrade: websocket\r\n"
        b"Connection: Upgrade\r\n"
        b"Sec-WebSocket-Protocol: mqtt\r\n"
        b"Sec-WebSocket-Accept: " + accept + b"\r\n\r\n"
    )
    return WebSocketStream(stream)


def encode_remaining(value: int) -> bytes:
    encoded = bytearray()
    while True:
        digit = value % 128
        value //= 128
        if value:
            digit |= 0x80
        encoded.append(digit)
        if not value:
            return bytes(encoded)


def frame(packet_type: int, flags: int, body: bytes) -> bytes:
    return bytes([(packet_type << 4) | flags]) + encode_remaining(len(body)) + body


def read_exact(stream: socket.socket, length: int) -> bytes | None:
    data = bytearray()
    while len(data) < length:
        try:
            chunk = stream.recv(length - len(data))
        except (ConnectionError, OSError, TimeoutError):
            return None
        if not chunk:
            return None
        data.extend(chunk)
    return bytes(data)


def read_frame(stream: socket.socket) -> tuple[int, int, bytes] | None:
    first = read_exact(stream, 1)
    if first is None:
        return None
    remaining = 0
    multiplier = 1
    while True:
        digit = read_exact(stream, 1)
        if digit is None:
            return None
        remaining += (digit[0] & 0x7F) * multiplier
        if not digit[0] & 0x80:
            break
        multiplier *= 128
        if multiplier > 128**3:
            return None
    body = read_exact(stream, remaining)
    if body is None:
        return None
    return first[0] >> 4, first[0] & 0x0F, body


def string_at(body: bytes, offset: int) -> tuple[bytes, int]:
    length = struct.unpack_from("!H", body, offset)[0]
    offset += 2
    return body[offset : offset + length], offset + length


def variable_byte_integer_at(body: bytes, offset: int) -> tuple[int, int]:
    length = 0
    multiplier = 1
    while True:
        digit = body[offset]
        offset += 1
        length += (digit & 0x7F) * multiplier
        if not digit & 0x80:
            return length, offset
        multiplier *= 128


def properties_at(body: bytes, offset: int) -> tuple[bytes, int]:
    length, offset = variable_byte_integer_at(body, offset)
    return body[offset : offset + length], offset + length


def skip_properties(body: bytes, offset: int) -> int:
    return properties_at(body, offset)[1]


@dataclass
class Connection:
    stream: socket.socket
    protocol: int
    client_id: bytes
    subscriptions: set[bytes] = field(default_factory=set)
    wire_acceptance: set[str] = field(default_factory=set)
    next_packet_id: int = 100
    outstanding_incoming: set[int] = field(default_factory=set)
    pressure_packet_ids: list[bytes] = field(default_factory=list)
    pressure_release_started: bool = False
    pressure_released: bool = False
    pressure_lock: threading.Lock = field(default_factory=threading.Lock)

    def send(self, data: bytes) -> None:
        self.stream.sendall(data)

    def publish(self, topic: bytes, payload: bytes, qos: int = 0) -> None:
        body = struct.pack("!H", len(topic)) + topic
        if qos:
            packet_id = self.next_packet_id
            body += struct.pack("!H", packet_id)
            self.next_packet_id += 1
            self.outstanding_incoming.add(packet_id)
        if self.protocol == 5:
            # Payload format, one user property, and binary correlation data with embedded zeros.
            body += b"\x0f\x01\x00\x26\x00\x01k\x00\x01v\x09\x00\x03\x00\x05\x00"
        body += payload
        self.send(frame(3, qos << 1, body))


class Broker:
    def __init__(self, tls_context: ssl.SSLContext | None = None, *, websocket: bool = False) -> None:
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen()
        self.listener.settimeout(0.2)
        self.port = self.listener.getsockname()[1]
        self.tls_context = tls_context
        self.websocket = websocket
        self.stopping = threading.Event()
        self.threads: list[threading.Thread] = []
        self.failures: list[str] = []
        self.client_ids: set[bytes] = set()
        self.observations: dict[bytes, set[str]] = {}
        self.connection_attempts: dict[bytes, int] = {}
        self.tls_disconnects_before_connect = 0
        self.failure_lock = threading.Lock()
        self.accept_thread = threading.Thread(target=self.accept, name="mqtt-fixture", daemon=True)

    def start(self) -> None:
        self.accept_thread.start()

    def stop(self) -> None:
        self.stopping.set()
        self.listener.close()
        self.accept_thread.join(timeout=3)
        if self.accept_thread.is_alive():
            self.failures.append("broker accept thread did not stop before its deadline")
        for thread in self.threads:
            thread.join(timeout=3)
            if thread.is_alive():
                self.failures.append(f"broker connection thread {thread.name} survived shutdown")
        for client_id in {b"js-3.1.1", b"js-5.0"}.intersection(self.client_ids):
            required = {"abandoned-waiter", "shutdown-delivery"}
            observed = self.observations.get(client_id, set())
            if not required.issubset(observed):
                self.failures.append(
                    f"JavaScript wire observations were incomplete for {client_id!r}: {sorted(observed)}"
                )
        if (
            self.tls_context is not None
            and b"js-tls-valid" in self.client_ids
            and self.tls_disconnects_before_connect < 2
        ):
            self.failures.append("wrong-CA and wrong-host TLS connections were not both rejected")
        if self.failures:
            raise RuntimeError("; ".join(self.failures))

    def accept(self) -> None:
        while not self.stopping.is_set():
            try:
                stream, _ = self.listener.accept()
            except (TimeoutError, OSError):
                continue
            stream.settimeout(5)
            if self.tls_context is not None:
                try:
                    stream = self.tls_context.wrap_socket(stream, server_side=True)
                except ssl.SSLError:
                    self.tls_disconnects_before_connect += 1
                    stream.close()
                    continue
                except (ConnectionError, OSError):
                    stream.close()
                    continue
            if self.websocket:
                try:
                    stream = accept_websocket(stream)
                except (ConnectionError, OSError, TimeoutError, ValueError):
                    stream.close()
                    continue
            thread = threading.Thread(target=self.serve, args=(stream,), daemon=True)
            self.threads.append(thread)
            thread.start()

    def serve(self, stream: socket.socket) -> None:
        connection: Connection | None = None
        try:
            connected = read_frame(stream)
            if connected is None or connected[0] != 1:
                if self.tls_context is not None:
                    self.tls_disconnects_before_connect += 1
                return
            body = connected[2]
            protocol_name, offset = string_at(body, 0)
            if protocol_name != b"MQTT":
                return
            protocol = body[offset]
            offset += 1
            offset += 1  # CONNECT flags
            offset += 2  # Keep Alive
            if protocol == 4:
                pass
            elif protocol == 5:
                offset = skip_properties(body, offset)
            else:
                return
            client_id, _ = string_at(body, offset)
            with self.failure_lock:
                attempt = self.connection_attempts.get(client_id, 0) + 1
                self.connection_attempts[client_id] = attempt
            if client_id.startswith(b"python-attempt-recovery-") and attempt == 1:
                return
            if client_id.startswith(b"python-capability-") and attempt > 1:
                time.sleep(0.3)
            if protocol == 4:
                stream.sendall(b"\x20\x02\x00\x00")
            elif client_id.startswith(b"python-capability-"):
                stream.sendall(b"\x20\x06\x00\x00\x03\x22\x00\x0a")
            else:
                stream.sendall(b"\x20\x03\x00\x00\x00")
            connection = Connection(stream, protocol, client_id)
            self.client_ids.add(client_id)
            while not self.stopping.is_set():
                packet = read_frame(stream)
                if packet is None:
                    return
                packet_type, flags, body = packet
                if connection.client_id in {
                    b"native-invalid-protocol-options",
                    b"native-v4-protocol-options",
                } and packet_type in {3, 8, 10}:
                    raise AssertionError(f"rejected native command emitted packet type {packet_type}")
                if packet_type == 3:
                    if not self.handle_publish(connection, flags, body):
                        return
                elif packet_type == 6:
                    packet_id = body[:2]
                    suffix = b"" if protocol == 4 else b"\x00\x00"
                    connection.send(frame(7, 0, packet_id + suffix))
                elif packet_type == 4:
                    connection.outstanding_incoming.discard(struct.unpack("!H", body[:2])[0])
                elif packet_type == 5:
                    suffix = b"" if protocol == 4 else b"\x00\x00"
                    connection.send(frame(6, 2, body[:2] + suffix))
                elif packet_type == 7:
                    connection.outstanding_incoming.discard(struct.unpack("!H", body[:2])[0])
                elif packet_type == 8:
                    self.handle_subscribe(connection, body)
                elif packet_type == 10:
                    self.handle_unsubscribe(connection, body)
                elif packet_type == 12:
                    connection.send(frame(13, 0, b""))
                elif packet_type == 14:
                    return
        except Exception as error:
            # Fixture failures must reach the runner.
            with self.failure_lock:
                self.failures.append(repr(error))
        finally:
            with contextlib.suppress(OSError):
                stream.close()
            if (
                connection is not None
                and connection.outstanding_incoming
                and not connection.client_id.startswith((b"js-stale-", b"python-ack-rejection-"))
            ):
                with self.failure_lock:
                    self.failures.append(
                        f"connection closed without acknowledging incoming packet ids "
                        f"{sorted(connection.outstanding_incoming)}"
                    )
            if (
                connection is not None
                and connection.client_id == b"native-v5-protocol-options"
                and connection.wire_acceptance != {"default-subscribe", "extended-subscribe", "unsubscribe"}
            ):
                with self.failure_lock:
                    self.failures.append(
                        f"native MQTT 5 option coverage was incomplete: {sorted(connection.wire_acceptance)}"
                    )

    def handle_publish(self, connection: Connection, flags: int, body: bytes) -> bool:
        topic, offset = string_at(body, 0)
        qos = (flags >> 1) & 3
        packet_id = body[offset : offset + 2] if qos else b""
        if qos:
            offset += 2
        if connection.protocol == 5:
            offset = skip_properties(body, offset)
        payload = body[offset:]
        if topic == b"rumqttc/native/binary" and payload != b"\x00\x01\x00\x02\xff\x00":
            raise AssertionError(f"binary payload changed at the C boundary: {payload!r}")
        if topic == b"rumqttc/native/sliced" and payload != b"\x00\x07\x00\x08":
            raise AssertionError(f"sliced binary payload changed at the JS boundary: {payload!r}")
        if topic == b"rumqttc/native/abandoned":
            self.observations.setdefault(connection.client_id, set()).add("abandoned-waiter")
        if topic == b"rumqttc/native/shutdown-delivery":
            self.observations.setdefault(connection.client_id, set()).add("shutdown-delivery")
        if topic == b"rumqttc/native/stall":
            return True
        if topic.startswith(b"rumqttc/native/race/publish/slow/"):
            time.sleep(0.03)
        if topic == b"rumqttc/native/reject" and connection.protocol == 5 and qos == 1:
            connection.send(frame(4, 0, packet_id + b"\x87\x00"))
            return True
        if topic == b"rumqttc/native/pressure" and qos == 1:
            start_release = False
            with connection.pressure_lock:
                released = connection.pressure_released
                if not released:
                    connection.pressure_packet_ids.append(packet_id)
                    if not connection.pressure_release_started:
                        connection.pressure_release_started = True
                        start_release = True
            if released:
                suffix = b"" if connection.protocol == 4 else b"\x00\x00"
                connection.send(frame(4, 0, packet_id + suffix))
            elif start_release:
                threading.Thread(
                    target=self.release_pressure,
                    args=(connection,),
                    daemon=True,
                ).start()
            return True
        if qos == 1:
            suffix = b"" if connection.protocol == 4 else b"\x00\x00"
            connection.send(frame(4, 0, packet_id + suffix))
        elif qos == 2:
            suffix = b"" if connection.protocol == 4 else b"\x00\x00"
            connection.send(frame(5, 0, packet_id + suffix))
        if topic == b"rumqttc/native/interrupt":
            return False
        if topic in connection.subscriptions:
            connection.publish(topic, payload, qos=1)
        return True

    def handle_subscribe(self, connection: Connection, body: bytes) -> None:
        packet_id = body[:2]
        offset = 2
        properties = b""
        if connection.protocol == 5:
            properties, offset = properties_at(body, offset)
        subscriptions: list[tuple[bytes, int]] = []
        while offset < len(body):
            topic, offset = string_at(body, offset)
            options = body[offset]
            subscriptions.append((topic, options))
            connection.subscriptions.add(topic)
            offset += 1

        if any(topic.startswith(b"rumqttc/native/race/subscribe/slow/") for topic, _ in subscriptions):
            time.sleep(0.03)

        if connection.client_id == b"native-v5-protocol-options":
            if subscriptions == [(b"rumqttc/native/v5/default", 0)]:
                if properties:
                    raise AssertionError(f"default MQTT 5 SUBSCRIBE properties changed: {properties!r}")
                connection.wire_acceptance.add("default-subscribe")
            elif subscriptions == [
                (b"rumqttc/native/v5/options/0", 0x04),
                (b"rumqttc/native/v5/options/1", 0x19),
                (b"rumqttc/native/v5/options/2", 0x22),
            ]:
                expected = b"\x0b\x07\x26\x00\x01k\x00\x01v"
                if properties != expected:
                    raise AssertionError(f"extended MQTT 5 SUBSCRIBE properties changed: {properties!r}")
                connection.wire_acceptance.add("extended-subscribe")
            else:
                raise AssertionError(f"unexpected MQTT 5 option acceptance SUBSCRIBE: {subscriptions!r}")

        suffix = bytes([1] * len(subscriptions))
        if connection.protocol == 5:
            suffix = b"\x00" + suffix
        connection.send(frame(9, 0, packet_id + suffix))
        for topic, _ in subscriptions:
            if topic == b"rumqttc/native/incoming":
                connection.publish(topic, b"\x00native\x00", qos=1)
            elif topic == b"rumqttc/native/ack-burst":
                for index in range(128):
                    connection.publish(topic, index.to_bytes(2, "big"), qos=1)
            elif topic == b"rumqttc/native/overflow":
                for index in range(8):
                    connection.publish(topic, bytes([index, 0]), qos=0)
            elif topic == b"rumqttc/native/automatic/qos1":
                connection.publish(topic, b"automatic-qos1", qos=1)
            elif topic == b"rumqttc/native/automatic/qos2":
                connection.publish(topic, b"automatic-qos2", qos=2)
            elif topic.startswith(b"rumqttc/native/race/acknowledge/"):
                connection.publish(topic, b"ack-race", qos=1)

    def release_pressure(self, connection: Connection) -> None:
        time.sleep(0.2)
        with connection.pressure_lock:
            connection.pressure_released = True
            packet_ids = connection.pressure_packet_ids
            connection.pressure_packet_ids = []
        suffix = b"" if connection.protocol == 4 else b"\x00\x00"
        for packet_id in packet_ids:
            connection.send(frame(4, 0, packet_id + suffix))

    def handle_unsubscribe(self, connection: Connection, body: bytes) -> None:
        packet_id = body[:2]
        offset = 2
        properties = b""
        if connection.protocol == 5:
            properties, offset = properties_at(body, offset)
        filters: list[bytes] = []
        while offset < len(body):
            topic, offset = string_at(body, offset)
            filters.append(topic)

        if any(topic.startswith(b"rumqttc/native/race/unsubscribe/slow/") for topic in filters):
            time.sleep(0.03)

        if connection.client_id == b"native-v5-protocol-options":
            if filters != [b"rumqttc/native/v5/options/2"]:
                raise AssertionError(f"unexpected MQTT 5 option acceptance UNSUBSCRIBE: {filters!r}")
            expected = b"\x26\x00\x01u\x00\x01p"
            if properties != expected:
                raise AssertionError(f"MQTT 5 UNSUBSCRIBE properties changed: {properties!r}")
            connection.wire_acceptance.add("unsubscribe")

        suffix = b"" if connection.protocol == 4 else b"\x00\x11"
        connection.send(frame(11, 0, packet_id + suffix))


def make_tls_fixture(directory: str) -> tuple[ssl.SSLContext, ssl.SSLContext, str, str, str, str]:
    ca_key = os.path.join(directory, "ca.key")
    ca_cert = os.path.join(directory, "ca.pem")
    wrong_key = os.path.join(directory, "wrong-ca.key")
    wrong_cert = os.path.join(directory, "wrong-ca.pem")
    server_key = os.path.join(directory, "server.key")
    server_csr = os.path.join(directory, "server.csr")
    server_cert = os.path.join(directory, "server.pem")
    extensions = os.path.join(directory, "server.ext")
    client_key = os.path.join(directory, "client.key")
    client_csr = os.path.join(directory, "client.csr")
    client_cert = os.path.join(directory, "client.pem")
    client_extensions = os.path.join(directory, "client.ext")
    with open(extensions, "w", encoding="utf-8") as output:
        output.write("subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\n")
    with open(client_extensions, "w", encoding="utf-8") as output:
        output.write("extendedKeyUsage=clientAuth\n")
    commands = [
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=rumqttc test CA",
            "-keyout",
            ca_key,
            "-out",
            ca_cert,
        ],
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=rumqttc wrong CA",
            "-keyout",
            wrong_key,
            "-out",
            wrong_cert,
        ],
        [
            "openssl",
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-subj",
            "/CN=localhost",
            "-keyout",
            server_key,
            "-out",
            server_csr,
        ],
        [
            "openssl",
            "x509",
            "-req",
            "-days",
            "1",
            "-in",
            server_csr,
            "-CA",
            ca_cert,
            "-CAkey",
            ca_key,
            "-CAcreateserial",
            "-extfile",
            extensions,
            "-out",
            server_cert,
        ],
        [
            "openssl",
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-subj",
            "/CN=rumqttc test client",
            "-keyout",
            client_key,
            "-out",
            client_csr,
        ],
        [
            "openssl",
            "x509",
            "-req",
            "-days",
            "1",
            "-in",
            client_csr,
            "-CA",
            ca_cert,
            "-CAkey",
            ca_key,
            "-CAcreateserial",
            "-extfile",
            client_extensions,
            "-out",
            client_cert,
        ],
    ]
    for command in commands:
        subprocess.run(command, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(server_cert, server_key)
    mtls_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    mtls_context.load_cert_chain(server_cert, server_key)
    mtls_context.load_verify_locations(ca_cert)
    mtls_context.verify_mode = ssl.CERT_REQUIRED
    return context, mtls_context, ca_cert, wrong_cert, client_cert, client_key


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--omit-address-arguments",
        action="store_true",
        help="do not append the fixture host and port to the child command",
    )
    parser.add_argument("--binary", required=True)
    parser.add_argument("argument", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="rumqttc-native-tls-") as directory:
        tls_context, mtls_context, ca_cert, wrong_cert, client_cert, client_key = make_tls_fixture(directory)
        broker = Broker()
        tls_broker = Broker(tls_context)
        websocket_broker = Broker(websocket=True)
        wss_broker = Broker(tls_context, websocket=True)
        mtls_broker = Broker(mtls_context)
        broker.start()
        tls_broker.start()
        websocket_broker.start()
        wss_broker.start()
        mtls_broker.start()
        environment = os.environ.copy()
        environment["RUMQTTC_TEST_HOST"] = "127.0.0.1"
        environment["RUMQTTC_TEST_PORT"] = str(broker.port)
        environment["RUMQTTC_TEST_TLS_PORT"] = str(tls_broker.port)
        environment["RUMQTTC_TEST_WS_PORT"] = str(websocket_broker.port)
        environment["RUMQTTC_TEST_WSS_PORT"] = str(wss_broker.port)
        environment["RUMQTTC_TEST_MTLS_PORT"] = str(mtls_broker.port)
        with open(ca_cert, encoding="utf-8") as source:
            environment["RUMQTTC_TEST_CA_PEM"] = source.read()
        with open(wrong_cert, encoding="utf-8") as source:
            environment["RUMQTTC_TEST_WRONG_CA_PEM"] = source.read()
        with open(client_cert, encoding="utf-8") as source:
            environment["RUMQTTC_TEST_CLIENT_CERT_PEM"] = source.read()
        with open(client_key, encoding="utf-8") as source:
            environment["RUMQTTC_TEST_CLIENT_KEY_PEM"] = source.read()
        try:
            launcher = shlex.split(environment.get("RUMQTTC_NATIVE_LAUNCHER", ""))
            address_arguments = [] if args.omit_address_arguments else ["127.0.0.1", str(broker.port)]
            child_arguments = args.argument[1:] if args.argument[:1] == ["--"] else args.argument
            result = subprocess.run(
                [*launcher, args.binary, *child_arguments, *address_arguments],
                env=environment,
                check=False,
            )
            return result.returncode
        finally:
            broker.stop()
            tls_broker.stop()
            websocket_broker.stop()
            wss_broker.stop()
            mtls_broker.stop()


if __name__ == "__main__":
    sys.exit(main())
