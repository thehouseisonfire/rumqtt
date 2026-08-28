from __future__ import annotations

import asyncio
import os
import sys

from rumqttc import MqttClient, MqttClientOptions, ProtocolVersion


async def main() -> None:
    client = MqttClient(
        MqttClientOptions(
            protocol=ProtocolVersion.MQTT_5_0,
            broker_host=os.environ["RUMQTTC_TEST_HOST"],
            broker_port=int(os.environ["RUMQTTC_TEST_PORT"]),
            client_id="python-live-exit",
        )
    )
    await client.connect()
    # Deliberately close the loop with a live native client. The interpreter
    # cleanup registry must signal and join its driver without scheduling work.


asyncio.run(main())
sys.exit(0)
