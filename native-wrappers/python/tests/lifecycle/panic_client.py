from __future__ import annotations

import asyncio
import json
import os
import sys

from rumqttc import DriverError, MqttClient, MqttClientOptions, MqttError, ProtocolVersion, PublishOptions, QoS


async def exercise(boundary: str, protocol: ProtocolVersion) -> None:
    client = MqttClient(
        MqttClientOptions(
            protocol=protocol,
            broker_host=os.environ["RUMQTTC_TEST_HOST"],
            broker_port=int(os.environ["RUMQTTC_TEST_PORT"]),
            client_id=f"python-panic-{boundary}-{protocol.value}",
        )
    )
    await client.connect()
    events = client.events()
    await anext(events)
    pending = asyncio.create_task(
        client.publish("rumqttc/native/stall", b"pending", PublishOptions(qos=QoS.AT_LEAST_ONCE))
    )
    await asyncio.sleep(0)

    method = getattr(client._native, f"_inject_{boundary}_panic", None)
    if method is None:
        raise RuntimeError("panic_client.py requires a native extension built with panic-testing")
    response = json.loads(await method())
    assert response["error"]["code"] == "INTERNAL_PANIC"

    try:
        await asyncio.wait_for(pending, timeout=2)
    except MqttError as error:
        assert error.code == "INTERNAL_PANIC"
        assert error.kind.value == "internal"
        assert error.ambiguous
        assert error.operation_id is not None
    else:
        raise AssertionError("pending operation survived the injected panic")

    while True:
        event = await asyncio.wait_for(anext(events), timeout=2)
        if isinstance(event, DriverError):
            assert event.error.code == "INTERNAL_PANIC"
            break
    try:
        await anext(events)
    except StopAsyncIteration:
        pass
    else:
        raise AssertionError("event iterator did not terminate after the panic")

    # Panic containment terminates the driver, but the Python client still owns its join handle.
    # Reconcile that ownership before interpreter teardown so LSan observes the terminated
    # driver's channels and pending operations being released.
    await client.close_now()


async def main() -> None:
    boundary = sys.argv[1]
    for protocol in (ProtocolVersion.MQTT_3_1_1, ProtocolVersion.MQTT_5_0):
        await exercise(boundary, protocol)


asyncio.run(main())
