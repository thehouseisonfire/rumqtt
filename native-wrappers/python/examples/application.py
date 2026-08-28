from __future__ import annotations

import asyncio
import contextlib

from rumqttc import AckMode, IncomingPublish, MqttClient, MqttClientOptions, ProtocolVersion


async def consume_events(client: MqttClient) -> None:
    async for event in client.events():
        if isinstance(event, IncomingPublish):
            print(event.topic, event.payload)
            if event.acknowledgement is not None:
                await event.acknowledgement.ack()


async def wait_for_cleanup(task: asyncio.Task[None]) -> asyncio.CancelledError | None:
    cancellation: asyncio.CancelledError | None = None
    while not task.done():
        try:
            await asyncio.shield(task)
        except asyncio.CancelledError as error:
            # Keep waiting without forwarding current or repeated cancellation to cleanup.
            if cancellation is None:
                cancellation = error
        except Exception:
            break
    return cancellation


async def main() -> None:
    client = MqttClient(
        MqttClientOptions(
            protocol=ProtocolVersion.MQTT_5_0,
            broker_host="localhost",
            broker_port=1883,
            client_id="rumqttc-application",
            ack_mode=AckMode.MANUAL,
        )
    )
    consumer: asyncio.Task[None] | None = None
    try:
        await client.connect()
        consumer = asyncio.create_task(consume_events(client))
        await client.publish("example/outgoing", b"payload")
    finally:
        cancellation: asyncio.CancelledError | None = None
        try:
            close_task = asyncio.create_task(client.close())
            cancellation = await wait_for_cleanup(close_task)
            with contextlib.suppress(Exception, asyncio.CancelledError):
                close_task.result()
        finally:
            if consumer is not None:
                consumer.cancel()
                consumer_cancellation = await wait_for_cleanup(consumer)
                if cancellation is None:
                    cancellation = consumer_cancellation
                with contextlib.suppress(asyncio.CancelledError):
                    consumer.result()
        if cancellation is not None:
            raise cancellation


if __name__ == "__main__":
    asyncio.run(main())
