from __future__ import annotations

import asyncio
import os

from rumqttc import (
    AckMode,
    Connected,
    IncomingPublish,
    MqttClient,
    MqttClientOptions,
    ProtocolVersion,
    PublishMilestone,
    PublishOptions,
    QoS,
    Subscription,
)


async def exercise(protocol: ProtocolVersion) -> None:
    client = MqttClient(
        MqttClientOptions(
            protocol=protocol,
            broker_host=os.environ["RUMQTTC_TEST_HOST"],
            broker_port=int(os.environ["RUMQTTC_TEST_PORT"]),
            client_id=f"python-wheel-{protocol.value}",
            ack_mode=AckMode.MANUAL,
        )
    )
    connected = await client.connect()
    assert connected.protocol is protocol

    events = client.events()
    assert isinstance(await anext(events), Connected)

    for qos, milestone in (
        (QoS.AT_MOST_ONCE, PublishMilestone.QOS0_FLUSHED),
        (QoS.AT_LEAST_ONCE, PublishMilestone.QOS1_ACKNOWLEDGED),
        (QoS.EXACTLY_ONCE, PublishMilestone.QOS2_COMPLETED),
    ):
        completion = await client.publish(
            "rumqttc/python/wheel",
            memoryview(bytearray(b"\x00python-wheel\x00")),
            PublishOptions(qos=qos),
        )
        assert completion.milestone is milestone

    await client.subscribe([Subscription("rumqttc/native/incoming", QoS.AT_LEAST_ONCE)])
    incoming = await anext(events)
    assert isinstance(incoming, IncomingPublish)
    assert incoming.payload == b"\x00native\x00"
    assert incoming.acknowledgement is not None
    await incoming.acknowledgement.ack()

    await client.unsubscribe(["rumqttc/native/incoming"])
    await client.close()


async def main() -> None:
    await exercise(ProtocolVersion.MQTT_3_1_1)
    await exercise(ProtocolVersion.MQTT_5_0)


asyncio.run(main())
