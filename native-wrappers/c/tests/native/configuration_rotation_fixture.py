"""Check the C example's credential rotation on two real MQTT connections."""

import argparse
import os
import socket
import subprocess
import threading


def exact(peer, length):
    result = bytearray()
    while len(result) < length:
        data = peer.recv(length - len(result))
        if not data:
            raise AssertionError("unexpected connection closure")
        result.extend(data)
    return bytes(result)


def frame(peer):
    header = exact(peer, 1)[0]
    size, shift = 0, 0
    while True:
        value = exact(peer, 1)[0]
        size |= (value & 127) << shift
        if value < 128:
            return header, exact(peer, size)
        shift += 7
        assert shift < 28


def connect(peer, version, password):
    header, body = frame(peer)
    assert header == 0x10 and body[6] == version
    flags = body[7]
    assert flags & 0xC0 == 0xC0 and not flags & 4
    cursor = 10
    if version == 5:
        properties, shift = 0, 0
        while True:
            value = body[cursor]
            cursor += 1
            properties |= (value & 127) << shift
            if value < 128:
                break
            shift += 7
        cursor += properties
    values = []
    for _ in range(3):
        size = int.from_bytes(body[cursor:cursor + 2], "big")
        cursor += 2
        values.append(body[cursor:cursor + size])
        cursor += size
    assert values == [b"c-rotation", b"rotation-user", password], "unexpected CONNECT profile"
    peer.sendall(b"\x20\x03\x00\x00\x00" if version == 5 else b"\x20\x02\x00\x00")


def run(binary, version):
    errors = []
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        listener.settimeout(10)

        def broker():
            try:
                peer, _ = listener.accept()
                with peer:
                    peer.settimeout(10)
                    connect(peer, version, b"initial-token")
                    assert frame(peer)[0] >> 4 == 3
                peer, _ = listener.accept()
                with peer:
                    peer.settimeout(10)
                    connect(peer, version, b"replacement-token")
                    assert frame(peer)[0] >> 4 == 14
            except BaseException as error:
                errors.append(error)

        worker = threading.Thread(target=broker, daemon=True)
        worker.start()
        env = dict(os.environ, RUMQTTC_AWAIT_RECONNECT="1", RUMQTTC_USERNAME="rotation-user",
                   RUMQTTC_PASSWORD="initial-token", RUMQTTC_NEXT_PASSWORD="replacement-token")
        result = subprocess.run([binary, "127.0.0.1", str(listener.getsockname()[1]), str(version)],
                                env=env, timeout=20, check=False)
        worker.join(12)
        assert not worker.is_alive(), "broker did not finish"
        if errors:
            raise errors[0]
        assert result.returncode == 0, "rotation consumer failed"


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    args = parser.parse_args()
    for mqtt_version in (4, 5):
        run(args.binary, mqtt_version)
