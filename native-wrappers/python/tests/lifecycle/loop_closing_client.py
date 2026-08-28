from __future__ import annotations

import asyncio
import os

from rumqttc import (
    AckMode,
    IncomingPublish,
    MqttClient,
    MqttClientOptions,
    ProtocolVersion,
    PublishOptions,
    QoS,
    Subscription,
)


async def main() -> None:
    client = MqttClient(
        MqttClientOptions(
            protocol=ProtocolVersion.MQTT_5_0,
            broker_host=os.environ["RUMQTTC_TEST_HOST"],
            broker_port=int(os.environ["RUMQTTC_TEST_PORT"]),
            client_id="python-loop-closing-work",
            request_capacity=1,
            ack_mode=AckMode.MANUAL,
        )
    )
    await client.connect()
    events = client.events()
    await anext(events)
    await client.subscribe([Subscription("rumqttc/native/incoming", QoS.AT_LEAST_ONCE)])
    incoming = await anext(events)
    assert isinstance(incoming, IncomingPublish) and incoming.acknowledgement is not None

    completion_wait = asyncio.create_task(
        client.publish("rumqttc/native/stall", b"pending", PublishOptions(qos=QoS.AT_LEAST_ONCE))
    )
    acknowledgement_wait = asyncio.create_task(incoming.acknowledgement.ack())
    admission_waits = [
        asyncio.create_task(
            client.enqueue_publish(
                "rumqttc/native/pressure",
                bytes((index,)),
                PublishOptions(qos=QoS.AT_LEAST_ONCE),
            )
        )
        for index in range(64)
    ]
    await asyncio.sleep(0)
    assert not completion_wait.done()
    assert any(not task.done() for task in admission_waits)
    # asyncio.run now cancels admission/completion/ack observations and closes the loop. Interpreter
    # cleanup must terminate native ownership without scheduling another callback onto this loop.
    _ = acknowledgement_wait


asyncio.run(main())
