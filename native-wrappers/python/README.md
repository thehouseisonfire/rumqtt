# rumqttc for Python

`rumqttc` is the typed, asynchronous Python interface to the rumqtt MQTT 3.1.1
and MQTT 5 clients. It supports CPython 3.10 through 3.14. PyPy, free-threaded
CPython, subinterpreters, and the CPython limited API are not supported.

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
