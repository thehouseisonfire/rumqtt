# rumqttc for Python

`rumqttc` is the typed, asynchronous Python interface to the rumqtt MQTT 3.1.1
and MQTT 5 clients. It supports CPython 3.10 through 3.14. PyPy, free-threaded
CPython, subinterpreters, and the CPython limited API are not supported.

## Installation

`python -m pip install rumqttc` installs a version-specific wheel. Wheels are
published for manylinux 2.17 x86_64/aarch64, musllinux 1.2 x86_64, macOS 11+
x86_64/arm64, and Windows x86_64. Pip 25.3 is the minimum supported installer.

Source installation requires CPython development files, Rust 1.88 or newer
with Cargo, a native linker/C toolchain, and maturin 1.10 or newer. The build
never downloads a Rust toolchain or another executable:

```console
python -m pip install 'maturin>=1.10,<2'
python -m pip install --no-binary rumqttc rumqttc
```

```python
import asyncio

from rumqttc import MqttClient, MqttClientOptions, ProtocolVersion, QoS, PublishOptions


async def main() -> None:
    options = MqttClientOptions(
        protocol=ProtocolVersion.MQTT_5_0,
        broker_host="localhost",
        broker_port=1883,
        client_id="python-example",
    )
    async with MqttClient(options) as client:
        completion = await client.publish(
            "example/topic", b"payload", PublishOptions(qos=QoS.AT_LEAST_ONCE)
        )
        print(completion)


asyncio.run(main())
```

`connect()` waits for the first successful CONNACK. `enqueue_publish()` waits
only for bounded command admission, while `publish()` waits for the MQTT-aware
milestone: local transport flush for QoS 0, PUBACK for QoS 1, and PUBCOMP for
QoS 2. Tracked operations release their request-admission permit before waiting
for that milestone. Manual acknowledgements bypass the Python request permit
and use the native control lane, so saturated publish admission cannot block
protocol progress. Cancelling a Python waiter does not recall work already admitted.
Cancelling a `connect()` waiter likewise leaves a still-referenced client
reconnecting, while dropping the final client reference requests immediate
native cleanup.

There is no synchronous or callback facade. Do not use a client directly from
another thread or event loop. A foreign thread can submit work to the owning
loop with `asyncio.run_coroutine_threadsafe`:

```python
future = asyncio.run_coroutine_threadsafe(
    client.publish("commands", b"wake"), owning_loop
)
completion = future.result(timeout=10)
```

Stop foreign producers before closing the owning loop; scheduling or result
delivery can otherwise race loop closure.

Each client belongs to the `asyncio` loop where it is first used. Consume
`events()` continuously with one iterator. In manual acknowledgement mode,
eligible incoming QoS 1/2 publishes contain a one-shot `Acknowledgement`.
To observe initial connection failures, start `connect()` in an asyncio task;
`events()` becomes available as soon as that task starts native initialization,
before the first successful CONNACK. An iterator retained before shutdown can
be drained afterward through its terminal `Closed` event.

After the initial CONNACK, `close()` performs a graceful bounded shutdown. If
connection is still pending, it cancels startup immediately because there is
no established MQTT session to drain. `close_now()` always requests an
immediate bounded shutdown. Prefer `async with` or an explicit `finally` block.
The native extension is private; only names exported by `rumqttc` are public.

## Transports

For TCP and TLS transports, `broker_host` and `broker_port` select the
connection target; for TLS, `broker_host` is also the certificate-verification
and SNI name. For WebSocket transports, the URL selects the connection target;
for WSS, its host is the certificate-verification and SNI name. WebSocket URLs
must use the scheme matching their transport:

```python
from rumqttc import TlsOptions, TlsTransport, WebSocketTransport, WssTransport

tls = TlsTransport(TlsOptions(ca=custom_ca_pem))
ws = WebSocketTransport("ws://broker.example/mqtt")
wss = WssTransport(
    "wss://broker.example/mqtt",
    TlsOptions(
        ca=custom_ca_pem,
        client_certificate=client_certificate_pem,
        private_key=client_private_key_pem,
    ),
)
```

Client certificate and key must be supplied together. Malformed CA,
certificate, or key data causes `connect()` to fail immediately with a TLS
error during client initialization. Runtime trust, hostname-verification, and
peer-authentication failures produce `Disconnected` attempt events while
`connect()` continues waiting for a later successful CONNACK.

## Reconnect and timeouts

`connect()` is coalesced and waits through recoverable attempt failures. After
an established connection fails, `Disconnected(phase=ESTABLISHED)` precedes
the next `Connected`. Cancelling one connect waiter does not stop reconnection
or other waiters.

Timeouts are seconds. Booleans, negative/non-finite values, and overflow are
rejected. Omitted `keep_alive`, `connection_timeout`, and
`event_delivery_timeout` values are 60, 5, and 5 seconds respectively. A zero
keep-alive disables MQTT keep-alive; zero connection and event-delivery
timeouts are invalid, and event delivery has a 1 ms minimum. Omitting
`close(timeout=...)` uses a five-second graceful budget. Zero is an immediate
timeout observation and escalates the committed close to immediate shutdown.
Other operations have no caller-supplied timeout.

See `examples/application.py` for a dedicated event consumer, manual
acknowledgement, and cancellation-safe cleanup. See
`examples/mqtt5_properties.py` for PUBLISH, packet-level SUBSCRIBE, per-filter
subscription, and UNSUBSCRIBE properties.
