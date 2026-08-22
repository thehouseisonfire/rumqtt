from __future__ import annotations

import asyncio
import os

import pytest
from rumqttc import (
    AckMode,
    Closed,
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

pytestmark = pytest.mark.skipif("RUMQTTC_TEST_PORT" not in os.environ, reason="broker fixture is not running")


@pytest.mark.asyncio
@pytest.mark.parametrize("immediate", [False, True])
async def test_retained_iterator_drains_events_after_shutdown(*, immediate: bool) -> None:
    client = MqttClient(
        MqttClientOptions(
            protocol=ProtocolVersion.MQTT_5_0,
            broker_host=os.environ["RUMQTTC_TEST_HOST"],
            broker_port=int(os.environ["RUMQTTC_TEST_PORT"]),
            client_id=f"python-shutdown-{immediate}",
        )
    )
    await client.connect()
    events = client.events()

    if immediate:
        await client.close_now()
    else:
        await client.close()

    drained = [event async for event in events]
    assert isinstance(drained[0], Connected)
    assert isinstance(drained[-1], Closed)
    assert drained[-1].graceful is not immediate


@pytest.mark.asyncio
@pytest.mark.parametrize("protocol", [ProtocolVersion.MQTT_3_1_1, ProtocolVersion.MQTT_5_0])
async def test_protocol_behavior(protocol: ProtocolVersion) -> None:
    client = MqttClient(
        MqttClientOptions(
            protocol=protocol,
            broker_host=os.environ["RUMQTTC_TEST_HOST"],
            broker_port=int(os.environ["RUMQTTC_TEST_PORT"]),
            client_id=f"python-{protocol.value}",
            ack_mode=AckMode.MANUAL,
        )
    )
    connected = await client.connect()
    assert connected.protocol is protocol

    async for event in client.events():
        assert isinstance(event, Connected)
        break
    events = client.events()

    for qos, milestone in (
        (QoS.AT_MOST_ONCE, PublishMilestone.QOS0_FLUSHED),
        (QoS.AT_LEAST_ONCE, PublishMilestone.QOS1_ACKNOWLEDGED),
        (QoS.EXACTLY_ONCE, PublishMilestone.QOS2_COMPLETED),
    ):
        completion = await client.publish(
            "rumqttc/python/publish",
            memoryview(bytearray(b"\x00python\x00")),
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


if __name__ == "__main__":
    asyncio.run(test_retained_iterator_drains_events_after_shutdown(immediate=False))
    asyncio.run(test_retained_iterator_drains_events_after_shutdown(immediate=True))
    asyncio.run(test_protocol_behavior(ProtocolVersion.MQTT_3_1_1))
    asyncio.run(test_protocol_behavior(ProtocolVersion.MQTT_5_0))
