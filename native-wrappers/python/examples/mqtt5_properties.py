from __future__ import annotations

import asyncio

from rumqttc import (
    MqttClient,
    MqttClientOptions,
    ProtocolVersion,
    PublishOptions,
    RetainForwardRule,
    SubscribeOptions,
    Subscription,
    UnsubscribeOptions,
    V5PublishProperties,
    V5SubscriptionOptions,
)


async def main() -> None:
    async with MqttClient(
        MqttClientOptions(ProtocolVersion.MQTT_5_0, "localhost", 1883, "rumqttc-properties")
    ) as client:
        await client.publish(
            "requests/device",
            b"status",
            PublishOptions(
                properties=V5PublishProperties(
                    response_topic="responses/device",
                    correlation_data=b"request-1",
                    content_type="application/octet-stream",
                    payload_format_indicator=0,
                    message_expiry_interval=30,
                    user_properties=(("source", "example"),),
                )
            ),
        )
        await client.subscribe(
            [
                Subscription(
                    "responses/+",
                    options=V5SubscriptionOptions(
                        no_local=True,
                        retain_as_published=True,
                        retain_forward_rule=RetainForwardRule.ON_NEW_SUBSCRIBE,
                    ),
                )
            ],
            options=SubscribeOptions(subscription_identifier=7, user_properties=(("scope", "packet"),)),
        )
        await client.unsubscribe(
            ["responses/+"],
            options=UnsubscribeOptions(user_properties=(("reason", "cleanup"),)),
        )


asyncio.run(main())
