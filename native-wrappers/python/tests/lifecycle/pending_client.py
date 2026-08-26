from __future__ import annotations

import asyncio

from rumqttc import MqttClient, MqttClientOptions, ProtocolVersion


async def main() -> None:
    client = MqttClient(
        MqttClientOptions(
            protocol=ProtocolVersion.MQTT_5_0,
            broker_host="127.0.0.1",
            broker_port=1,
            client_id="python-pending-loop-close",
            connection_timeout=1,
        )
    )
    connecting = asyncio.create_task(client.connect())
    await asyncio.sleep(0)
    iterator = client.events()
    await asyncio.wait_for(anext(iterator), timeout=2)
    event_wait = asyncio.ensure_future(anext(iterator))
    await asyncio.sleep(0)
    # asyncio.run closes the loop with connection and event observations pending. Module cleanup
    # must disable delivery and join native ownership without relying on that loop.
    assert not connecting.done()
    assert not event_wait.done()


asyncio.run(main())
