#!/usr/bin/env python3
"""Deterministic MQTT broker fixture and native executable runner."""

from __future__ import annotations

import argparse
import base64
import contextlib
import hashlib
import hmac
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
from pathlib import Path

from platform_trust import trusted_root

PRIVATE_VALUES: list[bytes] = [b"private-device", b"rumqttc-missing-device"]


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
    ordered_headers: list[tuple[str, str]] = []
    for line in request.decode("ascii").split("\r\n")[1:]:
        if ":" in line:
            name, value = line.split(":", 1)
            headers[name.strip().lower()] = value.strip()
            ordered_headers.append((name.strip().lower(), value.strip()))
    path = request.decode("ascii").split("\r\n", 1)[0].split(" ")[1]
    if path == "/native-headers":
        if [value for name, value in ordered_headers if name == "x-native"] != ["three", ""]:
            raise AssertionError("WebSocket ordered header edits changed")
        if "x-remove" in headers:
            raise AssertionError("removed WebSocket header survived")
    elif path == "/native-headers-cleared" and ("x-native" in headers or "x-remove" in headers):
        raise AssertionError("cleared WebSocket header edits survived")
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


def scram_auth_data(properties: bytes) -> bytes:
    offset = 0
    method = None
    data = None
    while offset < len(properties):
        property_id = properties[offset]
        offset += 1
        if property_id in (0x15, 0x16):
            value, offset = string_at(properties, offset)
            if property_id == 0x15:
                method = value
            else:
                data = value
        elif property_id in (0x11, 0x27):
            offset += 4
        elif property_id in (0x21, 0x22):
            offset += 2
        elif property_id in (0x17, 0x19):
            offset += 1
        elif property_id == 0x26:
            _, offset = string_at(properties, offset)
            _, offset = string_at(properties, offset)
        else:
            raise AssertionError(f"unexpected SCRAM property 0x{property_id:02x}")
    if method != b"SCRAM-SHA-256" or data is None:
        raise AssertionError("SCRAM method or data missing")
    return data


def scram_exchange(stream: socket.socket, first: bytes, initial: bool, invalid_proof: bool = False) -> None:
    if not first.startswith(b"n,,n=scram-private-username,r="):
        raise AssertionError("unexpected SCRAM client first message")
    first_bare = first[3:]
    nonce = first_bare.split(b"r=", 1)[1]
    if not nonce:
        raise AssertionError("empty SCRAM nonce")
    server_nonce = nonce + b"fixedServerNonce"
    salt = b"fixed-salt"
    server_first = b"r=" + server_nonce + b",s=" + base64.b64encode(salt) + b",i=4096"
    method = b"\x15\x00\x0dSCRAM-SHA-256"
    challenge = method + b"\x16" + struct.pack("!H", len(server_first)) + server_first
    stream.sendall(frame(15, 0, b"\x18" + encode_remaining(len(challenge)) + challenge))
    answer = read_frame(stream)
    if answer is None or answer[0] != 15 or answer[2][0] != 0x18:
        raise AssertionError("SCRAM client final message missing")
    answer_properties, _ = properties_at(answer[2], 1)
    final = scram_auth_data(answer_properties)
    without_proof, marker, encoded_proof = final.rpartition(b",p=")
    if not marker or b"r=" + server_nonce not in without_proof:
        raise AssertionError("SCRAM client final nonce changed")
    auth_message = b",".join((first_bare, server_first, without_proof))
    salted = hashlib.pbkdf2_hmac("sha256", b"scram-private-password", salt, 4096)
    client_key = hmac.new(salted, b"Client Key", hashlib.sha256).digest()
    stored_key = hashlib.sha256(client_key).digest()
    signature = hmac.new(stored_key, auth_message, hashlib.sha256).digest()
    expected_proof = bytes(left ^ right for left, right in zip(client_key, signature, strict=True))
    if not hmac.compare_digest(base64.b64decode(encoded_proof), expected_proof):
        raise AssertionError("SCRAM client proof failed")
    server_key = hmac.new(salted, b"Server Key", hashlib.sha256).digest()
    server_proof = b"v=" + base64.b64encode(hmac.new(server_key, auth_message, hashlib.sha256).digest())
    if invalid_proof:
        server_proof = b"v=AAAA"
    PRIVATE_VALUES.extend((nonce, encoded_proof, server_proof, server_proof[2:]))
    success_properties = method + b"\x16" + struct.pack("!H", len(server_proof)) + server_proof
    if initial:
        stream.sendall(frame(2, 0, b"\x00\x00" + encode_remaining(len(success_properties)) + success_properties))
    else:
        stream.sendall(frame(15, 0, b"\x00" + encode_remaining(len(success_properties)) + success_properties))


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
    limit_packet_ids: list[bytes] = field(default_factory=list)
    limit_admitted: threading.Event = field(default_factory=threading.Event)

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
    def __init__(
        self,
        tls_context: ssl.SSLContext | None = None,
        *,
        websocket: bool = False,
        tls_proxy: bool = False,
        unix_path: str | None = None,
    ) -> None:
        self.listener = socket.socket(socket.AF_UNIX if unix_path else socket.AF_INET, socket.SOCK_STREAM)
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(unix_path if unix_path else ("127.0.0.1", 0))
        self.listener.listen()
        self.listener.settimeout(0.2)
        self.port = 0 if unix_path else self.listener.getsockname()[1]
        self.tls_context = tls_context
        self.websocket = websocket
        self.tls_proxy = tls_proxy
        self.stopping = threading.Event()
        self.threads: list[threading.Thread] = []
        self.failures: list[str] = []
        self.client_ids: set[bytes] = set()
        self.observations: dict[bytes, set[str]] = {}
        self.connection_attempts: dict[bytes, int] = {}
        self.restart_packet_ids: dict[tuple[bytes, bytes], bytes] = {}
        self.restart_subscribe_packets: dict[bytes, bytes] = {}
        self.redirect_targets: dict[str, Broker] = {}
        self.expected_isolated: str | None = None
        self.expected_bind_ports: set[int] = set()
        self.tunnel_peers: dict[int, str] = {}
        self.close_pending: dict[bytes, tuple[Connection, bytes]] = {}
        self.limit_connections: dict[bytes, Connection] = {}
        self.alias_packet_ids: dict[bytes, bytes] = {}
        self.alias_packets_seen: dict[bytes, int] = {}
        self.control: Path | None = None
        self.tls_disconnects_before_connect = 0
        self.failure_lock = threading.Lock()
        self.accept_thread = threading.Thread(target=self.accept, name="mqtt-fixture", daemon=True)

    def signal(self, name: str, value: int = 1) -> None:
        assert self.control is not None
        temporary = self.control / (name + ".tmp")
        temporary.write_text(f"{value}\n", encoding="ascii")
        temporary.replace(self.control / name)

    def barrier(self, name: str) -> int:
        assert self.control is not None
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and not self.stopping.is_set():
            path = self.control / name
            if path.exists():
                return int(path.read_text(encoding="ascii"))
            time.sleep(0.001)
        raise AssertionError(f"fixture barrier timed out: {name}")

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
        for client_id in {b"native-store-restart-v4", b"native-store-restart-v5"}.intersection(self.client_ids):
            required = {
                "replayed-qos1",
                "replayed-qos2",
                "replayed-subscribe",
                "incoming-qos2-recorded",
                "incoming-qos2-completed",
            }
            if not required.issubset(self.observations.get(client_id, set())):
                self.failures.append(f"stored restart state was not recovered: {client_id!r}")
        lost_clients = {b"native-store-restart-lost-v4", b"native-store-restart-lost-v5"}
        for client_id in lost_clients.intersection(self.client_ids):
            required = {"incoming-qos2-recorded", "fresh-barrier"}
            if not required.issubset(self.observations.get(client_id, set())):
                self.failures.append(f"lost-session restart did not finish: {client_id!r}")
        if b"native-v5-auth-overlap" in self.client_ids and "reauth-start" not in self.observations.get(
            b"native-v5-auth-overlap", set()
        ):
            self.failures.append("overlapping reauthentication did not reach the broker")
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

    def batching(self, stream: socket.socket, protocol: int, client_id: bytes) -> None:
        name = client_id.decode("ascii")
        reads = b"-read-" in client_id
        batch = int(client_id.rsplit(b"-", 1)[1])
        expected = batch or (8 if reads else 1)
        self.barrier(name + "-admitted")
        connack = frame(2, 0, b"\x00\x00" + (b"\x03\x21\x00\x10" if protocol == 5 else b""))
        packets = b""
        if reads:
            # Preload the entire burst together with CONNACK, before the first connected poll.
            for index in range(12):
                body = b"\x00\x05batch" + struct.pack("!H", 100 + index)
                packets += frame(3, 2, body + (b"\x00" if protocol == 5 else b"") + bytes((index,)))
        stream.sendall(connack + packets)

        def observe(index: int) -> None:
            packet = read_frame(stream)
            if packet is None:
                raise AssertionError(f"batch packet {index} missing for {name}")
            kind, flags, body = packet
            if reads:
                expected_body = struct.pack("!H", 100 + index)
                # MQTT 5 PUBACK may omit default success and empty properties.
                if kind != 4 or flags or body not in (expected_body, expected_body + b"\x00\x00"):
                    raise AssertionError(f"batch acknowledgement changed for {name}: {packet!r}")
            else:
                expected_body = b"\x00\x05batch" + (b"\x00" if protocol == 5 else b"") + bytes((index,))
                if kind != 3 or flags or body != expected_body:
                    raise AssertionError(f"batch publish changed for {name}: {packet!r}")

        for index in range(expected):
            observe(index)
        stream.settimeout(0.15)
        extra = read_frame(stream)
        stream.settimeout(5)
        if extra is not None:
            raise AssertionError(f"configured batch boundary exceeded for {name}: {extra!r}")
        self.signal(name + "-measured", expected)
        self.barrier(name + "-released")
        for index in range(expected, 12):
            observe(index)
        self.signal(name, 12)
        closed = read_frame(stream)
        if closed is None or closed[0] != 14:
            raise AssertionError(f"batch client did not close gracefully: {name}")

    def serve(self, stream: socket.socket) -> None:
        connection: Connection | None = None
        try:
            proxy_kind: str | None = None
            if self.tls_proxy:
                request = bytearray()
                while b"\r\n\r\n" not in request:
                    byte = stream.recv(1)
                    if not byte or len(request) > 4096:
                        raise AssertionError("incomplete HTTPS CONNECT request")
                    request.extend(byte)
                if not request.startswith(b"CONNECT broker.invalid:1883 HTTP/1.1\r\n"):
                    raise AssertionError("HTTPS proxy target was changed")
                if b"Proxy-Authorization: Basic dXNlcjpwYXNz\r\n" not in request:
                    raise AssertionError("HTTPS proxy credentials were not preserved")
                stream.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                proxy_kind = "http"
            if not self.websocket and not self.tls_context:
                prefix = stream.recv(1, socket.MSG_PEEK)
                if prefix == b"C":
                    while len(prefix) < 7:
                        prefix = stream.recv(7, socket.MSG_PEEK)
                        if not prefix:
                            raise AssertionError("connection closed during proxy preface")
                if prefix.startswith(b"CONNECT"):
                    request = bytearray()
                    while b"\r\n\r\n" not in request:
                        byte = stream.recv(1)
                        if not byte or len(request) > 4096:
                            raise AssertionError("incomplete HTTP CONNECT request")
                        request.extend(byte)
                    if not request.startswith(b"CONNECT broker.invalid:1883 HTTP/1.1\r\n"):
                        raise AssertionError("proxy target was changed")
                    if b"Proxy-Authorization: Basic dXNlcjpwYXNz\r\n" not in request:
                        raise AssertionError("HTTP proxy credentials were not preserved")
                    stream.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    proxy_kind = "http"
                elif prefix[:1] == b"\x05":
                    greeting = read_exact(stream, 2)
                    if greeting is None or greeting[0] != 5:
                        raise AssertionError("incomplete SOCKS greeting")
                    methods = read_exact(stream, greeting[1])
                    if methods is None or 2 not in methods:
                        raise AssertionError("SOCKS username/password method missing")
                    stream.sendall(b"\x05\x02")
                    auth_header = read_exact(stream, 2)
                    if auth_header is None or auth_header[0] != 1:
                        raise AssertionError("incomplete SOCKS authentication")
                    username = read_exact(stream, auth_header[1])
                    password_length = read_exact(stream, 1)
                    password = read_exact(stream, password_length[0]) if password_length else None
                    if username != b"user" or password != b"pass":
                        raise AssertionError("SOCKS credentials were changed")
                    stream.sendall(b"\x01\x00")
                    address_header = read_exact(stream, 4)
                    if address_header is None or address_header[:3] != b"\x05\x01\x00" or address_header[3] != 3:
                        raise AssertionError("SOCKS remote DNS policy was not used")
                    host_length = read_exact(stream, 1)
                    host = read_exact(stream, host_length[0]) if host_length else None
                    port = read_exact(stream, 2)
                    if host != b"broker.invalid" or port != b"\x07\x5b":
                        raise AssertionError("SOCKS proxy target was changed")
                    stream.sendall(b"\x05\x00\x00\x01\x7f\x00\x00\x01\x00\x00")
                    proxy_kind = "socks5"
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
            connect_flags = body[offset]
            offset += 1
            offset += 2  # Keep Alive
            if protocol == 4:
                connect_properties = b""
            elif protocol == 5:
                connect_properties, offset = properties_at(body, offset)
            else:
                return
            client_id, offset = string_at(body, offset)
            if client_id == b"native-socket-options" and stream.getpeername()[1] not in self.expected_bind_ports:
                raise AssertionError("configured local bind port did not reach the broker")
            if client_id == b"native-socket-cleared" and stream.getpeername()[1] in self.expected_bind_ports:
                raise AssertionError("cleared bind address still reached the broker")
            if client_id.startswith(b"native-tls-matrix-positive"):
                tls_stream = stream.stream if isinstance(stream, WebSocketStream) else stream
                if not isinstance(tls_stream, ssl.SSLSocket) or tls_stream.selected_alpn_protocol() != "mqtt":
                    raise AssertionError("TLS ALPN was not negotiated")
            if client_id.startswith(b"native-tunnel-"):
                peer = stream.stream if isinstance(stream, WebSocketStream) else stream
                with self.failure_lock:
                    actual_proxy = self.tunnel_peers.pop(peer.getpeername()[1], None)
                kind = int(client_id.split(b"-")[3])
                if actual_proxy != ("socks5" if kind == 3 else "http"):
                    raise AssertionError("TLS/WSS MQTT connection bypassed its proxy tunnel")
            if client_id.startswith(b"native-proxy-"):
                expected_proxy = "http" if b"http" in client_id else "socks5"
                if proxy_kind != expected_proxy:
                    raise AssertionError("MQTT connection bypassed the configured proxy")
            with self.failure_lock:
                attempt = self.connection_attempts.get(client_id, 0) + 1
                self.connection_attempts[client_id] = attempt
            if client_id in {b"native-wire-options-v4", b"native-wire-options-v5"}:
                user_properties = b"\x26\x00\x01b\x00\x013\x26\x00\x01b\x00\x00"
                if attempt <= 2:
                    if not (connect_flags & 0x04) or (connect_flags >> 3) & 3 != 2 or connect_flags & 0x20:
                        raise AssertionError("replacement Will flags, QoS or retain changed")
                    if protocol == 5:
                        if connect_properties != b"\x21\x00\x09\x19\x01" + user_properties:
                            raise AssertionError(f"replacement CONNECT properties changed: {connect_properties!r}")
                        will_properties, offset = properties_at(body, offset)
                        expected_will = b"\x18\x00\x00\x00\x02\x03\x00\x06binary\x09\x00\x02\xff\x00" + user_properties
                        if will_properties != expected_will:
                            raise AssertionError(f"replacement Will properties changed: {will_properties!r}")
                    will_topic, offset = string_at(body, offset)
                    will_payload, offset = string_at(body, offset)
                    if will_topic != b"native/replacement" or will_payload != b"\xff\x00\x80\x01":
                        raise AssertionError("replacement Will topic or payload changed")
                elif attempt == 3:
                    default_connect = b"\x27\x00\x00\x28\x00" if protocol == 5 else b""
                    if connect_flags & 0x04 or connect_properties != default_connect:
                        raise AssertionError("cleared Will or CONNECT properties survived")
                else:
                    raise AssertionError("wire-options fixture connected more than three times")
                if offset != len(body):
                    raise AssertionError("unexpected CONNECT payload fields")
            if client_id.startswith(b"native-batch-"):
                self.batching(stream, protocol, client_id)
                return
            if client_id.startswith(b"native-proxy-") and b"-reconnect-" in client_id and attempt == 1:
                return
            if client_id.startswith((b"native-unix-reconnect-", b"native-websocket-reconnect-")) and attempt == 1:
                return
            if client_id.startswith(b"python-attempt-recovery-") and attempt == 1:
                return
            if client_id.startswith(b"python-capability-") and attempt > 1:
                time.sleep(0.3)
            if (
                client_id.startswith(b"native-runtime-incoming-")
                and protocol == 5
                and connect_properties != b"\x27\x00\x00\x04\x00"
            ):
                raise AssertionError("local input decoder limit changed the advertised maximum")
            if client_id.startswith(b"native-cleared-limits-"):
                if protocol == 5 and connect_properties:
                    raise AssertionError("cleared advertised maximum survived CONNECT")
                properties = b"\x21\x00\x04" if protocol == 5 else b""
                stream.sendall(
                    frame(
                        2, 0, b"\x00\x00" + (encode_remaining(len(properties)) + properties if protocol == 5 else b"")
                    )
                )
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id.startswith(b"native-runtime-inflight-"):
                remote = 2 if client_id.endswith(b"remote") else 5
                properties = b"\x21\x00" + bytes((remote,)) if protocol == 5 else b""
                stream.sendall(
                    frame(
                        2, 0, b"\x00\x00" + (encode_remaining(len(properties)) + properties if protocol == 5 else b"")
                    )
                )
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
                self.limit_connections[client_id] = connection
            elif client_id.startswith(b"native-outgoing-"):
                properties = b"\x27\x00\x00\x00\x20" if protocol == 5 else b""
                stream.sendall(
                    frame(
                        2, 0, b"\x00\x00" + (encode_remaining(len(properties)) + properties if protocol == 5 else b"")
                    )
                )
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id == b"" and b"\x19\x01" in connect_properties:
                properties = (
                    b"\x11\x00\x00\x00\x00\x21\x00\x07\x24\x01\x25\x00"
                    b"\x27\x00\x00\x04\x00\x22\x00\x02\x28\x00\x29\x01\x2a\x00"
                    b"\x13\x00\x1e\x12\x00\x0fassigned-native\x1f\x00\x00\x1a\x00\x00"
                    b"\x1c\x00\x0elocalhost:1883\x26\x00\x01k\x00\x01v\x26\x00\x01k\x00\x00"
                )
                stream.sendall(frame(2, 0, b"\x00\x00" + encode_remaining(len(properties)) + properties))
                connection = Connection(stream, protocol, b"native-rich-properties")
                self.client_ids.add(client_id)
            elif client_id.startswith(b"native-alias-"):
                properties = b"\x22\x00" + bytes((2 if attempt == 1 else 0,))
                stream.sendall(
                    frame(2, 0, bytes((int(attempt > 1), 0)) + encode_remaining(len(properties)) + properties)
                )
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id.startswith(b"native-unix-timeout-"):
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id.startswith(b"native-redirect-matrix-"):
                source, form = client_id.decode().removeprefix("native-redirect-matrix-").split("-", 1)
                if form == "malformed":
                    reference = b"mqtt://user:private@localhost:1883"
                elif form == "disallowed":
                    reference = b"https://localhost:1883/mqtt"
                else:
                    target = self.redirect_targets[form if form != "exhausted" else "mqtt"]
                    target.expected_isolated = form
                    reference = (
                        f"localhost:{target.port}"
                        if form == "authority"
                        else f"{form if form != 'exhausted' else 'mqtt'}://localhost:{target.port}"
                        + ("/mqtt?redirect=one" if form in {"ws", "wss"} else "")
                    ).encode()
                properties = b"\x1c" + struct.pack("!H", len(reference)) + reference
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
                if source == "disconnect":
                    stream.sendall(frame(2, 0, b"\x00\x00\x00"))
                    stream.sendall(frame(14, 0, b"\x9d" + encode_remaining(len(properties)) + properties))
                else:
                    stream.sendall(frame(2, 0, b"\x00\x9c" + encode_remaining(len(properties)) + properties))
            elif client_id == b"" and self.expected_isolated is not None:
                form = self.expected_isolated
                self.expected_isolated = None
                if protocol != 5 or connect_flags != 2:
                    raise AssertionError("redirect carried origin credentials, Will or persistent session")
                if connect_properties != b"\x11\x00\x00\x00\x00\x27\x00\x00\x28\x00" or offset != len(body):
                    raise AssertionError("redirect carried origin CONNECT properties")
                if form == "exhausted":
                    reference = b"127.0.0.2:1"
                    properties = b"\x1c" + struct.pack("!H", len(reference)) + reference
                    stream.sendall(frame(2, 0, b"\x00\x9c" + encode_remaining(len(properties)) + properties))
                else:
                    assigned = b"native-redirect-target"
                    properties = b"\x12" + struct.pack("!H", len(assigned)) + assigned
                    stream.sendall(frame(2, 0, b"\x00\x00" + encode_remaining(len(properties)) + properties))
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id in {b"native-v5-redirect-reject", b"native-v5-redirect-loop"} and attempt == 1:
                reference = f"127.0.0.1:{self.port}".encode("ascii")
                properties = b"\x1c" + struct.pack("!H", len(reference)) + reference
                stream.sendall(frame(2, 0, b"\x00\x9c" + encode_remaining(len(properties)) + properties))
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id in {
                b"native-v5-srv-redirect",
                b"native-v5-srv-fallback",
                b"native-v5-srv-exhaust",
                b"native-v5-srv-unusable",
                b"native-v5-srv-weighted",
                b"native-v5-srv-empty",
                b"native-v5-srv-missing",
                b"native-v5-srv-failed",
                b"native-v5-srv-cancel",
            } and (attempt == 1 or client_id in {b"native-v5-srv-cancel", b"native-v5-srv-weighted"}):
                reference = b"_mqtt._tcp.service.invalid"
                properties = b"\x1c" + struct.pack("!H", len(reference)) + reference
                stream.sendall(frame(2, 0, b"\x00\x9c" + encode_remaining(len(properties)) + properties))
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id == b"" and (
                self.connection_attempts.get(b"native-v5-srv-redirect", 0)
                or self.connection_attempts.get(b"native-v5-srv-fallback", 0)
            ):
                assigned = b"native-v5-srv-target"
                properties = b"\x12" + struct.pack("!H", len(assigned)) + assigned
                stream.sendall(frame(2, 0, b"\x00\x00" + encode_remaining(len(properties)) + properties))
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id in {b"native-v5-auth-timeout", b"native-v5-auth-cancel", b"native-v5-auth-abandon"}:
                method = b"\x15\x00\x06custom"
                if protocol != 5 or method not in body:
                    raise AssertionError("timeout fixture did not receive authenticated CONNECT")
                challenge = method + b"\x16\x00\x06server"
                stream.sendall(frame(15, 0, b"\x18" + encode_remaining(len(challenge)) + challenge))
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id == b"native-v5-auth-reject":
                method = b"\x15\x00\x06custom"
                if protocol != 5 or method not in body:
                    raise AssertionError("rejection fixture did not receive authenticated CONNECT")
                stream.sendall(frame(15, 0, b"\x18" + encode_remaining(len(method)) + method))
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id == b"native-v5-auth-reconnect":
                method = b"\x15\x00\x06custom"
                if protocol != 5 or method not in body:
                    raise AssertionError("reconnect fixture did not receive authenticated CONNECT")
                stream.sendall(frame(15, 0, b"\x18" + encode_remaining(len(method)) + method))
                answer = read_frame(stream)
                if answer is None or answer[0] != 15 or answer[2][0] != 0x18:
                    raise AssertionError("reconnect AUTH response missing")
                answer_properties, _ = properties_at(answer[2], 1)
                if method not in answer_properties:
                    raise AssertionError("reconnect AUTH method changed")
                stream.sendall(frame(2, 0, b"\x00\x00" + encode_remaining(len(method)) + method))
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
                if attempt == 1:
                    return
            elif client_id == b"native-v5-auth-overlap" or client_id.startswith(b"native-v5-auth-invalid-"):
                method = b"\x15\x00\x06custom"
                if protocol != 5 or method not in body:
                    raise AssertionError("overlap fixture did not receive authenticated CONNECT")
                if client_id == b"native-v5-auth-overlap" and attempt == 2:
                    self.signal("auth-reconnected")
                    self.barrier("auth-reconnect-release")
                elif client_id == b"native-v5-auth-overlap" and attempt != 1:
                    raise AssertionError("unexpected overlap reconnect")
                stream.sendall(frame(2, 0, b"\x00\x00" + encode_remaining(len(method)) + method))
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id == b"native-v5-auth-async":
                if protocol != 5 or b"\x15\x00\x06custom" not in body or b"\x16\x00\x07initial" not in body:
                    raise AssertionError("deferred AUTH start did not reach CONNECT intact")
                method = b"\x15\x00\x06custom"
                challenge = (
                    method
                    + b"\x16\x00\x08server\x00\xff"
                    + b"\x1f\x00\x09auth-step"
                    + b"\x26\x00\x01k\x00\x011\x26\x00\x01k\x00\x00\x26\x00\x01k\x00\x012"
                )
                stream.sendall(frame(15, 0, b"\x18" + encode_remaining(len(challenge)) + challenge))
                answer = read_frame(stream)
                if answer is None or answer[0] != 15 or answer[2][0] != 0x18:
                    raise AssertionError("deferred AUTH response was not sent")
                answer_properties, _ = properties_at(answer[2], 1)
                if answer_properties != (method + b"\x16\x00\x05reply\x26\x00\x01p\x00\x011\x26\x00\x01p\x00\x012"):
                    raise AssertionError("deferred AUTH response fields changed on the wire")
                success = method + b"\x16\x00\x0cserver-proof"
                stream.sendall(frame(2, 0, b"\x00\x00" + encode_remaining(len(success)) + success))
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id == b"native-v5-scram":
                if protocol != 5:
                    raise AssertionError("SCRAM fixture did not use MQTT 5")
                scram_exchange(stream, scram_auth_data(connect_properties), True)
                reauth = read_frame(stream)
                if reauth is None or reauth[0] != 15 or reauth[2][0] != 0x19:
                    raise AssertionError("SCRAM reauthentication start missing")
                reauth_properties, _ = properties_at(reauth[2], 1)
                scram_exchange(stream, scram_auth_data(reauth_properties), False)
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id == b"native-v5-scram-invalid":
                if protocol != 5:
                    raise AssertionError("invalid SCRAM proof fixture did not use MQTT 5")
                scram_exchange(stream, scram_auth_data(connect_properties), True, invalid_proof=True)
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id == b"c-auth-example":
                method = b"\x15\x00\x04demo"
                if protocol != 5 or method not in body or b"\x16\x00\x05hello" not in body:
                    raise AssertionError("C authenticator example did not send CONNECT authentication")
                stream.sendall(frame(2, 0, b"\x00\x00" + encode_remaining(len(method)) + method))
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id.startswith(b"native-store-restart-"):
                if (client_id.endswith(b"v4") and protocol != 4) or (client_id.endswith(b"v5") and protocol != 5):
                    raise AssertionError("restart fixture protocol changed")
                if attempt > 2:
                    raise AssertionError("restart fixture connected more than twice")
                present = int(attempt == 2 and b"-lost-" not in client_id)
                if protocol == 4:
                    stream.sendall(frame(2, 0, bytes((present, 0))))
                else:
                    stream.sendall(frame(2, 0, bytes((present, 0, 0))))
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
                if present:
                    connection.outstanding_incoming.add(100)
                    suffix = b"" if protocol == 4 else b"\x00\x00"
                    stream.sendall(frame(6, 2, b"\x00\x64" + suffix))
            elif client_id in {b"native-store-policy-strict", b"native-store-policy-allow"}:
                if protocol != 5 or connect_flags & 2:
                    raise AssertionError("resume policy fixture must request a persistent MQTT 5 session")
                stream.sendall(frame(2, 0, b"\x01\x00\x00"))
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            elif client_id == b"native-store-atomic":
                if protocol != 4:
                    raise AssertionError("atomic checkpoint fixture changed protocol")
                stream.sendall(frame(2, 0, bytes((int(attempt > 1), 0))))
                connection = Connection(stream, protocol, client_id)
                self.client_ids.add(client_id)
            else:
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
                    if client_id.startswith(b"native-store-restart-") and attempt == 1:
                        if body[:2] != b"\x00\x64":
                            raise AssertionError("incoming QoS2 PUBREC packet id changed")
                        self.observations.setdefault(client_id, set()).add("incoming-qos2-recorded")
                        connection.outstanding_incoming.discard(100)
                    else:
                        suffix = b"" if protocol == 4 else b"\x00\x00"
                        connection.send(frame(6, 2, body[:2] + suffix))
                elif packet_type == 7:
                    connection.outstanding_incoming.discard(struct.unpack("!H", body[:2])[0])
                    if client_id.startswith(b"native-store-restart-") and attempt == 2:
                        if b"-lost-" in client_id:
                            raise AssertionError("incoming QoS2 survived broker session loss")
                        if body[:2] != b"\x00\x64":
                            raise AssertionError("recovered incoming QoS2 PUBCOMP packet id changed")
                        self.observations.setdefault(client_id, set()).add("incoming-qos2-completed")
                elif packet_type == 8:
                    self.handle_subscribe(connection, body)
                elif packet_type == 10:
                    self.handle_unsubscribe(connection, body)
                elif packet_type == 12:
                    connection.send(frame(13, 0, b""))
                elif packet_type == 14:
                    if client_id.startswith(b"native-close-options-"):
                        expected = (
                            b"\x11\x00\x00\x00\x00\x1f\x00\x08selected\x26\x00\x01k\x00\x011\x26\x00\x01k\x00\x012"
                        )
                        if body != b"\x00" + encode_remaining(len(expected)) + expected:
                            raise AssertionError("admitted DISCONNECT options changed")
                        self.observations.setdefault(client_id, set()).add("close-wire")
                    return
                elif packet_type == 15 and connection.client_id == b"native-v5-auth-async":
                    if body[0] != 0x19 or b"\x15\x00\x06custom" not in body or b"\x16\x00\x07initial" not in body:
                        raise AssertionError("client reauthentication packet differed")
                    method = b"\x15\x00\x06custom"
                    challenge = (
                        method
                        + b"\x16\x00\x08server\x00\xff"
                        + b"\x1f\x00\x09auth-step"
                        + b"\x26\x00\x01k\x00\x011\x26\x00\x01k\x00\x00\x26\x00\x01k\x00\x012"
                    )
                    stream.sendall(frame(15, 0, b"\x18" + encode_remaining(len(challenge)) + challenge))
                    answer = read_frame(stream)
                    if answer is None or answer[0] != 15 or answer[2][0] != 0x18:
                        raise AssertionError("deferred reauthentication response was not sent")
                    answer_properties, _ = properties_at(answer[2], 1)
                    expected = method + b"\x16\x00\x05reply" + b"\x26\x00\x01p\x00\x011\x26\x00\x01p\x00\x012"
                    if answer_properties != expected:
                        raise AssertionError("deferred reauthentication properties changed")
                    success = method + b"\x16\x00\x0cserver-proof"
                    stream.sendall(frame(15, 0, b"\x00" + encode_remaining(len(success)) + success))
                elif packet_type == 15 and connection.client_id == b"native-v5-auth-overlap":
                    if body[0] != 0x19 or b"\x15\x00\x06custom" not in body:
                        raise AssertionError("overlap fixture did not send reauthentication start")
                    self.observations.setdefault(client_id, set()).add("reauth-start")
                    method = b"\x15\x00\x06custom"
                    challenge = method + b"\x16\x00\x04hold"
                    stream.sendall(frame(15, 0, b"\x18" + encode_remaining(len(challenge)) + challenge))
                    answer = read_frame(stream)
                    if answer != (15, 0, b"\x18" + encode_remaining(len(method)) + method):
                        raise AssertionError("overlap AUTH continuation changed")
                    self.signal("auth-held")
                    self.barrier("auth-abort")
                    stream.settimeout(0.05)
                    if read_frame(stream) is not None:
                        raise AssertionError("overlapping AUTH reached the broker")
                    return  # Broker closes only after the host proves second-terminal/first-pending.
                elif packet_type == 15 and client_id.startswith(b"native-v5-auth-invalid-"):
                    if body[0] != 0x19 or b"\x15\x00\x06custom" not in body:
                        raise AssertionError("invalid-auth fixture did not send reauthentication start")
                    method = b"\x15\x00\x06custom"
                    code = 0x18
                    if client_id.endswith(b"success"):
                        code = 0
                    elif client_id.endswith(b"broker-method"):
                        method = b"\x15\x00\x07changed"
                    elif client_id.endswith(b"duplicate"):
                        method += method
                    elif client_id.endswith(b"property"):
                        method += b"\x21\x00\x01"  # Receive Maximum is forbidden in AUTH.
                    secret = b"auth-private-challenge"
                    PRIVATE_VALUES.append(secret)
                    properties = method + b"\x16" + struct.pack("!H", len(secret)) + secret
                    stream.sendall(frame(15, 0, bytes((code,)) + encode_remaining(len(properties)) + properties))
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
            publish_properties, offset = properties_at(body, offset)
        else:
            publish_properties = b""
        payload = body[offset:]
        if connection.client_id == b"native-rich-properties":
            connection.send(frame(4, 0, packet_id + b"\x00\x00"))
            self.signal("event-publish-id", int.from_bytes(packet_id, "big"))
            properties = (
                b"\x11\x00\x00\x00\x00\x1f\x00\x0cbroker-error"
                b"\x1c\x00\x13broker.invalid:1883"
                b"\x26\x00\x01k\x00\x01v\x26\x00\x01k\x00\x00"
            )
            connection.send(frame(14, 0, b"\x80" + encode_remaining(len(properties)) + properties))
            return True
        if topic == b"native/events/burst":
            for index in range(512):
                connection.publish(b"native/events/item", struct.pack("!I", index) + b"\x00\xff", qos=0)
            return True
        if topic == b"native/runtime/admitted":
            target = self.limit_connections.get(payload)
            if target is None:
                raise AssertionError("runtime admission barrier had no target")
            target.limit_admitted.set()
            return True
        if connection.client_id.startswith(b"native-cleared-limits-"):
            if topic != b"native/cleared" or payload != b"\xff" * 256:
                raise AssertionError("publish beyond removed limit changed on wire")
            connection.limit_packet_ids.append(packet_id)
            suffix = b"" if connection.protocol == 4 else b"\x00\x00"
            count = len(connection.limit_packet_ids)
            if connection.protocol == 4 or count == 5:
                connection.send(frame(4, 0, packet_id + suffix))
            elif count == 4:
                connection.stream.settimeout(0.15)
                extra = read_frame(connection.stream)
                connection.stream.settimeout(5)
                if extra is not None:
                    raise AssertionError("cleared local limit ignored broker receive maximum")
                for identifier in connection.limit_packet_ids:
                    connection.send(frame(4, 0, identifier + suffix))
                connection.publish(b"native/cleared", b"\x80" * 512, qos=0)
            return True
        if connection.client_id.startswith(b"native-runtime-inflight-"):
            connection.limit_packet_ids.append(packet_id)
            count = len(connection.limit_packet_ids)
            suffix = b"" if connection.protocol == 4 else b"\x00\x00"
            if count == 2:
                if not connection.limit_admitted.wait(5):
                    raise AssertionError("runtime admission barrier timed out")
                connection.stream.settimeout(0.15)
                extra = read_frame(connection.stream)
                connection.stream.settimeout(5)
                if extra is not None:
                    raise AssertionError("publish exceeded the negotiated inflight boundary")
                for identifier in connection.limit_packet_ids:
                    connection.send(frame(4, 0, identifier + suffix))
            elif count > 2:
                connection.send(frame(4, 0, packet_id + suffix))
            return True
        if connection.client_id.startswith(b"native-outgoing-") and len(body) + 2 != 32:
            raise AssertionError("outgoing packet crossed its exact size boundary")
        if connection.client_id.startswith(b"native-runtime-incoming-"):
            connection.publish(b"a", b"small", qos=0)
            connection.publish(b"a", b"x" * 64, qos=0)
            if connection.client_id.endswith(b"bytes"):
                return True
        if connection.client_id.startswith(b"native-alias-"):
            attempt = self.connection_attempts[connection.client_id]
            count = self.alias_packets_seen.get(connection.client_id, 0) + 1
            self.alias_packets_seen[connection.client_id] = count
            if qos != 1 or payload != b"alias":
                raise AssertionError("alias publish changed")
            if attempt == 1:
                if publish_properties != b"\x23\x00\x01" or topic != (b"native/alias" if count == 1 else b""):
                    raise AssertionError("alias registration or reuse changed")
                if count == 2:
                    self.alias_packet_ids[connection.client_id] = packet_id
                    return False
            else:
                if topic != b"native/alias" or publish_properties:
                    raise AssertionError("alias state survived the reconnect limit change")
                if count == 3 and (packet_id != self.alias_packet_ids.get(connection.client_id) or not flags & 8):
                    raise AssertionError("alias replay lost its concrete topic, packet id or DUP bit")
        if connection.client_id.startswith(b"native-close-options-") and topic == b"native/close/stall":
            if qos != 1:
                raise AssertionError("close fixture did not send pending QoS1")
            with self.failure_lock:
                self.close_pending[connection.client_id] = (connection, packet_id)
            connection.publish(b"native/close/ready", b"", qos=0)
            return True
        if topic == b"native/close/release":
            with self.failure_lock:
                pending = self.close_pending.pop(payload, None)
            if pending is None:
                raise AssertionError("close fixture release had no pending publish")
            target, identifier = pending
            target.send(frame(4, 0, identifier + b"\x00\x00"))
            return True
        if connection.client_id.startswith(b"native-store-restart-"):
            attempt = self.connection_attempts[connection.client_id]
            if attempt == 2 and b"-lost-" in connection.client_id:
                if topic != b"rumqttc/native/restart/fresh" or payload != b"fresh" or qos != 1:
                    raise AssertionError("outgoing stored work survived broker session loss")
                self.observations.setdefault(connection.client_id, set()).add("fresh-barrier")
                suffix = b"" if connection.protocol == 4 else b"\x00\x00"
                connection.send(frame(4, 0, packet_id + suffix))
                return True
            expected_qos = {
                b"rumqttc/native/restart/qos1": 1,
                b"rumqttc/native/restart/qos2": 2,
            }.get(topic)
            if expected_qos is None or payload != b"persist" or qos != expected_qos:
                raise AssertionError("restart publish changed on the wire")
            key = (connection.client_id, topic)
            if attempt == 1:
                self.restart_packet_ids[key] = packet_id
                return True
            if packet_id != self.restart_packet_ids.get(key) or not (flags & 8):
                raise AssertionError("restored publish lost its packet id or DUP bit")
            self.observations.setdefault(connection.client_id, set()).add(f"replayed-qos{qos}")
            suffix = b"" if connection.protocol == 4 else b"\x00\x00"
            connection.send(frame(4 if qos == 1 else 5, 0, packet_id + suffix))
            return True
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
        if connection.client_id == b"native-rich-properties":
            self.signal("event-subscribe-id", int.from_bytes(packet_id, "big"))
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

        if connection.client_id.startswith(b"native-store-restart-"):
            if b"-lost-" in connection.client_id and self.connection_attempts[connection.client_id] == 2:
                raise AssertionError("stored subscription survived broker session loss")
            if subscriptions != [(b"rumqttc/native/restart/incoming", 2)]:
                raise AssertionError("restored subscription fields changed")
            if self.connection_attempts[connection.client_id] == 1:
                self.restart_subscribe_packets[connection.client_id] = body
                connection.publish(b"rumqttc/native/restart/incoming", b"inbound", qos=2)
                return
            if body != self.restart_subscribe_packets.get(connection.client_id):
                raise AssertionError("restored SUBSCRIBE packet id/properties changed")
            self.observations.setdefault(connection.client_id, set()).add("replayed-subscribe")

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
        if connection.client_id == b"native-rich-properties":
            self.signal("event-unsubscribe-id", int.from_bytes(packet_id, "big"))
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
    context.set_alpn_protocols(["mqtt"])
    mtls_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    mtls_context.load_cert_chain(server_cert, server_key)
    mtls_context.load_verify_locations(ca_cert)
    mtls_context.verify_mode = ssl.CERT_REQUIRED
    mtls_context.set_alpn_protocols(["mqtt"])
    return context, mtls_context, ca_cert, wrong_cert, client_cert, client_key


def run_native(command: list[str], environment: dict[str, str], redact: bool) -> int:
    result = subprocess.run(command, env=environment, capture_output=redact, check=False)
    if redact:
        captured = result.stdout + result.stderr
        private_values = (
            b"scram-private-password",
            b"scram-private-username",
            b"fixedServerNonce",
            b"v=AAAA",
            b"Proxy-Authorization:",
            b"dXNlcjpwYXNz",
            b"private-pkcs12-password",
            b"private-invalid-identity",
            b"private-wrong-password",
            b"proxy-private-user",
            b"proxy-private-password",
            base64.b64encode(b"proxy-private-user:proxy-private-password"),
            *PRIVATE_VALUES,
        )
        if any(value in captured for value in private_values):
            raise AssertionError("native process output disclosed a private fixture value")
        sys.stdout.buffer.write(result.stdout)
        sys.stderr.buffer.write(result.stderr)
    return result.returncode


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--omit-address-arguments",
        action="store_true",
        help="do not append the fixture host and port to the child command",
    )
    parser.add_argument("--binary", required=True)
    parser.add_argument("--atomic-restart", action="store_true")
    parser.add_argument("--redact", action="store_true")
    parser.add_argument("--proxy-matrix", action="store_true")
    parser.add_argument("--tls-matrix", action="store_true")
    parser.add_argument("--network-matrix", action="store_true")
    parser.add_argument("--custom-transport", action="store_true")
    parser.add_argument("argument", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="rumqttc-native-tls-") as directory:
        tls_context, mtls_context, ca_cert, wrong_cert, client_cert, client_key = make_tls_fixture(directory)
        broker = Broker()
        untrusted_broker = None
        if args.tls_matrix:
            untrusted_directory = os.path.join(directory, "untrusted")
            os.mkdir(untrusted_directory)
            untrusted_context, _, _, _, _, _ = make_tls_fixture(untrusted_directory)
            untrusted_broker = Broker(untrusted_context)
        tls_broker = Broker(tls_context)
        tls_proxy_broker = Broker(tls_context, tls_proxy=True)
        websocket_broker = Broker(websocket=True)
        wss_broker = Broker(tls_context, websocket=True)
        mtls_broker = Broker(mtls_context)
        mtls_wss_broker = Broker(mtls_context, websocket=True)
        broker.redirect_targets = {
            "authority": broker,
            "mqtt": broker,
            "mqtts": tls_broker,
            "ws": websocket_broker,
            "wss": wss_broker,
        }
        broker.control = Path(directory)
        broker.start()
        if untrusted_broker is not None:
            untrusted_broker.start()
        tls_broker.start()
        tls_proxy_broker.start()
        websocket_broker.start()
        wss_broker.start()
        mtls_broker.start()
        mtls_wss_broker.start()
        environment = os.environ.copy()
        environment["RUMQTTC_TEST_CONTROL_DIR"] = directory
        environment["RUMQTTC_TEST_HOST"] = "127.0.0.1"
        environment["RUMQTTC_TEST_PORT"] = str(broker.port)
        # Bound sockets without listen keep these local refusal endpoints deterministic.
        refused = [socket.socket(), socket.socket()]
        for index, reservation in enumerate(refused, 1):
            reservation.bind(("127.0.0.1", 0))
            environment[f"RUMQTTC_TEST_REFUSED_PORT_{index}"] = str(reservation.getsockname()[1])
        if untrusted_broker is not None:
            environment["RUMQTTC_TEST_UNTRUSTED_TLS_PORT"] = str(untrusted_broker.port)
        environment["RUMQTTC_TEST_TLS_PORT"] = str(tls_broker.port)
        environment["RUMQTTC_TEST_HTTPS_PROXY_PORT"] = str(tls_proxy_broker.port)
        environment["RUMQTTC_TEST_WS_PORT"] = str(websocket_broker.port)
        environment["RUMQTTC_TEST_WSS_PORT"] = str(wss_broker.port)
        environment["RUMQTTC_TEST_MTLS_PORT"] = str(mtls_broker.port)
        environment["RUMQTTC_TEST_MTLS_WSS_PORT"] = str(mtls_wss_broker.port)
        unix_broker = None
        if args.network_matrix:
            reservations = [socket.socket(), socket.socket()]
            for protocol, reservation in zip(("V4", "V5"), reservations, strict=True):
                reservation.bind(("127.0.0.1", 0))
                port = reservation.getsockname()[1]
                broker.expected_bind_ports.add(port)
                environment[f"RUMQTTC_TEST_BIND_PORT_{protocol}"] = str(port)
            for reservation in reservations:
                reservation.close()
        if args.network_matrix and os.name != "nt":
            unix_path = os.path.join(directory, "mqtt.socket")
            unix_broker = Broker(unix_path=unix_path)
            unix_broker.start()
            environment["RUMQTTC_TEST_UNIX_PATH"] = unix_path
        with open(ca_cert, encoding="utf-8") as source:
            environment["RUMQTTC_TEST_CA_PEM"] = source.read()
        with open(wrong_cert, encoding="utf-8") as source:
            environment["RUMQTTC_TEST_WRONG_CA_PEM"] = source.read()
        with open(client_cert, encoding="utf-8") as source:
            environment["RUMQTTC_TEST_CLIENT_CERT_PEM"] = source.read()
        with open(client_key, encoding="utf-8") as source:
            environment["RUMQTTC_TEST_CLIENT_KEY_PEM"] = source.read()
        if args.tls_matrix:
            archive = os.path.join(directory, "client.p12")
            subprocess.run(
                [
                    "openssl",
                    "pkcs12",
                    "-export",
                    "-out",
                    archive,
                    "-inkey",
                    client_key,
                    "-in",
                    client_cert,
                    "-certfile",
                    ca_cert,
                    "-passout",
                    "pass:private-pkcs12-password",
                ],
                check=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            environment["RUMQTTC_TEST_PKCS12_FILE"] = archive
            PRIVATE_VALUES.append(environment["RUMQTTC_TEST_CLIENT_KEY_PEM"].encode())
        proxies = []
        if args.proxy_matrix:
            from proxy_fixture import Proxy

            proxy_directory = os.path.join(directory, "proxy")
            os.mkdir(proxy_directory)
            proxy_context, _, proxy_ca, _, _, _ = make_tls_fixture(proxy_directory)
            ports = {broker.port, tls_broker.port, wss_broker.port}
            tunnel_brokers = {item.port: item for item in (broker, tls_broker, wss_broker)}
            if args.custom_transport:
                ports.add(websocket_broker.port)
                tunnel_brokers[websocket_broker.port] = websocket_broker

            def observe_tunnel(peer_port: int, target_port: int, kind: str) -> None:
                target = tunnel_brokers[target_port]
                with target.failure_lock:
                    target.tunnel_peers[peer_port] = kind

            for name, proxy_tls, failure in (
                ("TUNNEL", None, None),
                ("TLS_TUNNEL", proxy_context, None),
                ("RECOVER_TUNNEL", None, "recover"),
                ("TIMEOUT_TUNNEL", None, "timeout"),
            ):
                proxy = Proxy(ports, tls=proxy_tls, failure=failure, observe_tunnel=observe_tunnel)
                proxy.start()
                proxies.append(proxy)
                environment[f"RUMQTTC_TEST_{name}_PORT"] = str(proxy.port)
            with open(proxy_ca, encoding="utf-8") as source:
                environment["RUMQTTC_TEST_PROXY_CA_PEM"] = source.read()
        if args.custom_transport:
            from byte_tunnel_fixture import ByteTunnel
            tunnel = ByteTunnel({broker.port, tls_broker.port, websocket_broker.port, wss_broker.port,
                                 *(proxy.port for proxy in proxies)})
            tunnel.start()
            proxies.append(tunnel)
            environment["RUMQTTC_TEST_BYTE_TUNNEL_PORT"] = str(tunnel.port)
        try:
            launcher = shlex.split(environment.get("RUMQTTC_NATIVE_LAUNCHER", ""))
            address_arguments = [] if args.omit_address_arguments else ["127.0.0.1", str(broker.port)]
            child_arguments = args.argument[1:] if args.argument[:1] == ["--"] else args.argument
            if args.atomic_restart:
                environment["RUMQTTC_TEST_ATOMIC_FILE"] = os.path.join(directory, "checkpoint")
                for stage in ("seed", "interrupt", "verify"):
                    status = run_native(
                        [*launcher, args.binary, stage, *child_arguments, *address_arguments],
                        environment,
                        args.redact,
                    )
                    if status:
                        return status
                return 0
            platform_roots = args.tls_matrix and (
                sys.platform.startswith("linux") or environment.get("RUMQTTC_DISPOSABLE_TRUST_RUNNER") == "1"
            )
            if args.tls_matrix and not platform_roots:
                if environment.get("CI"):
                    raise RuntimeError("CI must execute the disposable platform trust fixture")
                print("platform trust: pending; requires an explicitly disposable runner")
            trust = trusted_root(Path(ca_cert), environment) if platform_roots else contextlib.nullcontext()
            with trust:
                return run_native(
                    [*launcher, args.binary, *child_arguments, *address_arguments],
                    environment,
                    args.redact,
                )
        finally:
            for reservation in refused:
                reservation.close()
            if unix_broker is not None:
                unix_broker.stop()
            for proxy in proxies:
                proxy.stop()
            broker.stop()
            if untrusted_broker is not None:
                untrusted_broker.stop()
            tls_broker.stop()
            tls_proxy_broker.stop()
            websocket_broker.stop()
            wss_broker.stop()
            mtls_broker.stop()
            mtls_wss_broker.stop()


if __name__ == "__main__":
    sys.exit(main())
