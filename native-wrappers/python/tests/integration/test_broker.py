from __future__ import annotations

import asyncio
import os
from pathlib import Path

import pytest
from rumqttc import (
    AckMode,
    ClientClosedError,
    Closed,
    Connected,
    Disconnected,
    IncomingPublish,
    MqttClient,
    MqttClientOptions,
    MqttError,
    ProtocolVersion,
    PublishMilestone,
    PublishOptions,
    QoS,
    Subscription,
    TlsOptions,
    TlsTransport,
    V5PublishProperties,
    WebSocketTransport,
    WssTransport,
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


@pytest.mark.asyncio
@pytest.mark.parametrize("protocol", [ProtocolVersion.MQTT_3_1_1, ProtocolVersion.MQTT_5_0])
async def test_cancelled_event_wait_does_not_consume_the_next_event(protocol: ProtocolVersion) -> None:
    topic = f"rumqttc/python/cancelled-event/{protocol.value}"
    client = MqttClient(
        MqttClientOptions(
            protocol=protocol,
            broker_host=os.environ["RUMQTTC_TEST_HOST"],
            broker_port=int(os.environ["RUMQTTC_TEST_PORT"]),
            client_id=f"python-cancel-event-{protocol.value}",
        )
    )
    await client.connect()
    events = client.events()
    assert isinstance(await anext(events), Connected)
    await client.subscribe([Subscription(topic, QoS.AT_LEAST_ONCE)])

    for sequence in range(20):
        pending = asyncio.ensure_future(anext(events))
        await asyncio.sleep(0)
        pending.cancel()
        with pytest.raises(asyncio.CancelledError):
            await pending

        payload = sequence.to_bytes(4, "big")
        await client.publish(topic, payload, PublishOptions(qos=QoS.AT_LEAST_ONCE))
        incoming = await asyncio.wait_for(anext(events), timeout=2)
        assert isinstance(incoming, IncomingPublish)
        assert incoming.payload == payload

    await client.close()


def broker_options(protocol: ProtocolVersion, client_id: str, **changes: object) -> MqttClientOptions:
    values: dict[str, object] = {
        "protocol": protocol,
        "broker_host": os.environ["RUMQTTC_TEST_HOST"],
        "broker_port": int(os.environ["RUMQTTC_TEST_PORT"]),
        "client_id": client_id,
    }
    values.update(changes)
    return MqttClientOptions(**values)  # type: ignore[arg-type]


@pytest.mark.asyncio
@pytest.mark.parametrize("protocol", [ProtocolVersion.MQTT_3_1_1, ProtocolVersion.MQTT_5_0])
async def test_connect_is_coalesced_repeatable_and_cancellation_is_waiter_local(protocol: ProtocolVersion) -> None:
    client = MqttClient(broker_options(protocol, f"python-connect-{protocol.value}"))
    waiters = [asyncio.create_task(client.connect()) for _ in range(12)]
    waiters[0].cancel()
    with pytest.raises(asyncio.CancelledError):
        await waiters[0]
    results = await asyncio.gather(*waiters[1:])
    assert all(result == results[0] for result in results)
    assert await client.connect() == results[0]
    await client.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("protocol", [ProtocolVersion.MQTT_3_1_1, ProtocolVersion.MQTT_5_0])
async def test_established_interruption_emits_ordered_reconnect_events(protocol: ProtocolVersion) -> None:
    client = MqttClient(broker_options(protocol, f"python-reconnect-{protocol.value}"))
    await client.connect()
    events = client.events()
    assert isinstance(await anext(events), Connected)
    await client.publish("rumqttc/native/interrupt", b"interrupt")
    disconnected = await asyncio.wait_for(anext(events), timeout=3)
    reconnected = await asyncio.wait_for(anext(events), timeout=3)
    assert isinstance(disconnected, Disconnected)
    assert disconnected.phase.value == "established"
    assert isinstance(reconnected, Connected)
    await client.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("protocol", [ProtocolVersion.MQTT_3_1_1, ProtocolVersion.MQTT_5_0])
async def test_initial_attempt_failure_recovers_without_replacing_connect(protocol: ProtocolVersion) -> None:
    client = MqttClient(broker_options(protocol, f"python-attempt-recovery-{protocol.value}"))
    connecting = asyncio.create_task(client.connect())
    await asyncio.sleep(0)
    events = client.events()
    disconnected = await asyncio.wait_for(anext(events), timeout=3)
    connected = await asyncio.wait_for(anext(events), timeout=3)
    assert isinstance(disconnected, Disconnected)
    assert disconnected.phase.value == "attempt"
    assert isinstance(connected, Connected)
    result = await asyncio.wait_for(connecting, timeout=1)
    assert result.protocol is protocol
    await client.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("protocol", [ProtocolVersion.MQTT_3_1_1, ProtocolVersion.MQTT_5_0])
async def test_manual_ack_rejects_double_stale_and_terminal_use(protocol: ProtocolVersion) -> None:
    client = MqttClient(broker_options(protocol, f"python-ack-rejection-{protocol.value}", ack_mode=AckMode.MANUAL))
    await client.connect()
    events = client.events()
    await anext(events)
    await client.subscribe([Subscription("rumqttc/native/incoming", QoS.AT_LEAST_ONCE)])
    incoming = await asyncio.wait_for(anext(events), timeout=2)
    assert isinstance(incoming, IncomingPublish) and incoming.acknowledgement is not None
    accepted, rejected = await asyncio.gather(
        incoming.acknowledgement.ack(), incoming.acknowledgement.ack(), return_exceptions=True
    )
    assert (accepted is None) != (rejected is None)
    assert any(isinstance(value, MqttError) for value in (accepted, rejected))

    await client.subscribe([Subscription("rumqttc/native/incoming", QoS.AT_LEAST_ONCE)])
    stale = await asyncio.wait_for(anext(events), timeout=2)
    assert isinstance(stale, IncomingPublish) and stale.acknowledgement is not None
    await client.publish("rumqttc/native/interrupt", b"")
    while not isinstance(await asyncio.wait_for(anext(events), timeout=3), Connected):
        pass
    with pytest.raises(MqttError):
        await stale.acknowledgement.ack()

    await client.subscribe([Subscription("rumqttc/native/incoming", QoS.AT_LEAST_ONCE)])
    terminal = await asyncio.wait_for(anext(events), timeout=2)
    assert isinstance(terminal, IncomingPublish) and terminal.acknowledgement is not None
    await client.close_now()
    with pytest.raises(ClientClosedError):
        await terminal.acknowledgement.ack()


@pytest.mark.asyncio
@pytest.mark.parametrize("protocol", [ProtocolVersion.MQTT_3_1_1, ProtocolVersion.MQTT_5_0])
async def test_automatic_acknowledgement_has_no_public_token(protocol: ProtocolVersion) -> None:
    client = MqttClient(broker_options(protocol, f"python-auto-ack-{protocol.value}", ack_mode=AckMode.AUTOMATIC))
    await client.connect()
    events = client.events()
    await anext(events)
    await client.subscribe(
        [
            Subscription("rumqttc/native/automatic/qos1", QoS.AT_LEAST_ONCE),
            Subscription("rumqttc/native/automatic/qos2", QoS.EXACTLY_ONCE),
        ]
    )
    received: set[str] = set()
    while len(received) < 2:
        event = await asyncio.wait_for(anext(events), timeout=2)
        if isinstance(event, IncomingPublish):
            assert event.acknowledgement is None
            received.add(event.topic)
    await client.close()


@pytest.mark.asyncio
async def test_event_overflow_has_stable_terminal_error_and_ends_iteration() -> None:
    client = MqttClient(
        broker_options(
            ProtocolVersion.MQTT_5_0,
            "python-overflow",
            event_capacity=1,
            event_delivery_timeout=0.05,
        )
    )
    await client.connect()
    events = client.events()
    await anext(events)
    pending = asyncio.create_task(
        client.publish("rumqttc/native/stall", b"pending", PublishOptions(qos=QoS.AT_LEAST_ONCE))
    )
    await client.subscribe([Subscription("rumqttc/native/overflow")])
    await asyncio.sleep(0.15)
    with pytest.raises(MqttError) as failure:
        while True:
            await anext(events)
    assert failure.value.code == "EVENT_BUFFER_OVERFLOW"
    with pytest.raises(MqttError) as pending_failure:
        await asyncio.wait_for(pending, timeout=2)
    assert pending_failure.value.code == "EVENT_BUFFER_OVERFLOW"
    assert pending_failure.value.operation_id is not None
    assert pending_failure.value.ambiguous
    with pytest.raises(StopAsyncIteration):
        await anext(events)
    await client.close_now()


@pytest.mark.asyncio
async def test_tls_trust_and_hostname_verification() -> None:
    ca = os.environ["RUMQTTC_TEST_CA_PEM"].encode()
    wrong_ca = os.environ["RUMQTTC_TEST_WRONG_CA_PEM"].encode()
    port = int(os.environ["RUMQTTC_TEST_TLS_PORT"])
    trusted = MqttClient(
        MqttClientOptions(
            protocol=ProtocolVersion.MQTT_5_0,
            broker_host="localhost",
            broker_port=port,
            client_id="python-tls-trusted",
            transport=TlsTransport(TlsOptions(ca=ca)),
        )
    )
    await trusted.connect()
    await trusted.close()

    for client_id, host, trust in (
        ("python-tls-untrusted", "localhost", wrong_ca),
        ("python-tls-hostname", "127.0.0.1", ca),
    ):
        rejected = MqttClient(
            MqttClientOptions(
                protocol=ProtocolVersion.MQTT_5_0,
                broker_host=host,
                broker_port=port,
                client_id=client_id,
                transport=TlsTransport(TlsOptions(ca=trust)),
                connection_timeout=1,
            )
        )
        connecting = asyncio.create_task(rejected.connect())
        await asyncio.sleep(0.2)
        assert not connecting.done()
        await rejected.close_now()
        with pytest.raises(ClientClosedError):
            await connecting


@pytest.mark.asyncio
@pytest.mark.parametrize("protocol", [ProtocolVersion.MQTT_3_1_1, ProtocolVersion.MQTT_5_0])
async def test_websocket_and_wss_connect_with_the_mqtt_subprotocol(protocol: ProtocolVersion) -> None:
    ca = os.environ["RUMQTTC_TEST_CA_PEM"].encode()
    transports = (
        (
            int(os.environ["RUMQTTC_TEST_WS_PORT"]),
            WebSocketTransport(f"ws://localhost:{os.environ['RUMQTTC_TEST_WS_PORT']}/mqtt"),
        ),
        (
            int(os.environ["RUMQTTC_TEST_WSS_PORT"]),
            WssTransport(
                f"wss://localhost:{os.environ['RUMQTTC_TEST_WSS_PORT']}/mqtt",
                TlsOptions(ca=ca),
            ),
        ),
    )
    for index, (port, transport) in enumerate(transports):
        client = MqttClient(
            MqttClientOptions(
                protocol=protocol,
                broker_host="localhost",
                broker_port=port,
                client_id=f"python-websocket-{protocol.value}-{index}",
                transport=transport,
            )
        )
        await client.connect()
        await client.publish("websocket/binary", b"\x00websocket\x00")
        await client.close()


@pytest.mark.asyncio
async def test_wss_rejects_untrusted_and_hostname_mismatched_certificates() -> None:
    port = int(os.environ["RUMQTTC_TEST_WSS_PORT"])
    ca = os.environ["RUMQTTC_TEST_CA_PEM"].encode()
    wrong_ca = os.environ["RUMQTTC_TEST_WRONG_CA_PEM"].encode()
    for client_id, host, trust in (
        ("python-wss-untrusted", "localhost", wrong_ca),
        ("python-wss-hostname", "127.0.0.1", ca),
    ):
        client = MqttClient(
            MqttClientOptions(
                protocol=ProtocolVersion.MQTT_5_0,
                broker_host=host,
                broker_port=port,
                client_id=client_id,
                transport=WssTransport(f"wss://{host}:{port}/mqtt", TlsOptions(ca=trust)),
                connection_timeout=1,
            )
        )
        connecting = asyncio.create_task(client.connect())
        await asyncio.sleep(0.2)
        assert not connecting.done()
        await client.close_now()
        with pytest.raises(ClientClosedError):
            await connecting


@pytest.mark.asyncio
async def test_mutual_tls_requires_and_accepts_the_paired_client_identity() -> None:
    port = int(os.environ["RUMQTTC_TEST_MTLS_PORT"])
    ca = os.environ["RUMQTTC_TEST_CA_PEM"].encode()
    certificate = os.environ["RUMQTTC_TEST_CLIENT_CERT_PEM"].encode()
    private_key = os.environ["RUMQTTC_TEST_CLIENT_KEY_PEM"].encode()
    authenticated = MqttClient(
        MqttClientOptions(
            protocol=ProtocolVersion.MQTT_5_0,
            broker_host="localhost",
            broker_port=port,
            client_id="python-mtls-authenticated",
            transport=TlsTransport(TlsOptions(ca=ca, client_certificate=certificate, private_key=private_key)),
        )
    )
    await authenticated.connect()
    await authenticated.close()

    unauthenticated = MqttClient(
        MqttClientOptions(
            protocol=ProtocolVersion.MQTT_5_0,
            broker_host="localhost",
            broker_port=port,
            client_id="python-mtls-missing-identity",
            transport=TlsTransport(TlsOptions(ca=ca)),
            connection_timeout=1,
        )
    )
    connecting = asyncio.create_task(unauthenticated.connect())
    await asyncio.sleep(0.2)
    assert not connecting.done()
    await unauthenticated.close_now()
    with pytest.raises(ClientClosedError):
        await connecting


@pytest.mark.asyncio
async def test_malformed_tls_material_is_a_configuration_error() -> None:
    malformed = MqttClient(
        MqttClientOptions(
            protocol=ProtocolVersion.MQTT_5_0,
            broker_host="localhost",
            broker_port=int(os.environ["RUMQTTC_TEST_TLS_PORT"]),
            client_id="python-malformed-tls",
            transport=TlsTransport(TlsOptions(ca=b"not a certificate")),
        )
    )
    with pytest.raises(MqttError) as failure:
        await malformed.connect()
    assert failure.value.code == "TLS"
    assert failure.value.kind.value == "tls"
    await malformed.close_now()


@pytest.mark.asyncio
async def test_mqtt5_broker_rejection_preserves_every_exception_attribute() -> None:
    client = MqttClient(broker_options(ProtocolVersion.MQTT_5_0, "python-broker-rejection"))
    await client.connect()
    with pytest.raises(MqttError) as failure:
        await client.publish(
            "rumqttc/native/reject",
            b"rejected",
            PublishOptions(qos=QoS.AT_LEAST_ONCE),
        )
    error = failure.value
    assert error.code == "BROKER_REJECTED"
    assert error.kind.value == "protocol"
    assert error.operation_id is not None
    assert error.broker_reason == 0x87
    assert error.retryable is False
    assert error.delivery.value == "rejected"
    assert error.ambiguous is False
    await client.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("protocol", [ProtocolVersion.MQTT_3_1_1, ProtocolVersion.MQTT_5_0])
async def test_concurrent_and_repeated_close_share_the_terminal_outcome(protocol: ProtocolVersion) -> None:
    client = MqttClient(broker_options(protocol, f"python-concurrent-close-{protocol.value}"))
    await client.connect()
    events = client.events()
    await anext(events)
    await asyncio.gather(*(client.close() for _ in range(8)))
    await client.close()
    terminal = [event async for event in events]
    assert isinstance(terminal[-1], Closed)
    assert terminal[-1].graceful


@pytest.mark.asyncio
async def test_zero_timeout_and_cancelled_graceful_close_escalate_to_immediate() -> None:
    for suffix, cancel in (("zero", False), ("cancel", True)):
        client = MqttClient(broker_options(ProtocolVersion.MQTT_5_0, f"python-close-{suffix}"))
        await client.connect()
        events = client.events()
        await anext(events)
        pending = asyncio.create_task(
            client.publish("rumqttc/native/stall", b"pending", PublishOptions(qos=QoS.AT_LEAST_ONCE))
        )
        await asyncio.sleep(0)
        closing = asyncio.create_task(client.close()) if cancel else asyncio.create_task(client.close(timeout=0))
        if cancel:
            await asyncio.sleep(0)
            closing.cancel()
            with pytest.raises(asyncio.CancelledError):
                await closing
        else:
            with pytest.raises(MqttError) as failure:
                await closing
            assert failure.value.kind.value == "timeout"
            assert failure.value.ambiguous
        with pytest.raises(MqttError):
            await asyncio.wait_for(pending, timeout=2)
        terminal = [event async for event in events]
        assert isinstance(terminal[-1], Closed)
        assert not terminal[-1].graceful


@pytest.mark.asyncio
async def test_concurrent_and_repeated_immediate_close_is_idempotent() -> None:
    client = MqttClient(broker_options(ProtocolVersion.MQTT_5_0, "python-concurrent-immediate"))
    await client.connect()
    events = client.events()
    await anext(events)
    await asyncio.gather(*(client.close_now() for _ in range(8)))
    await client.close_now()
    terminal = [event async for event in events]
    assert isinstance(terminal[-1], Closed)
    assert not terminal[-1].graceful


@pytest.mark.asyncio
async def test_mqtt5_publish_admission_waits_for_reconnect_capabilities() -> None:
    client = MqttClient(broker_options(ProtocolVersion.MQTT_5_0, "python-capability-reconnect"))
    await client.connect()
    events = client.events()
    await anext(events)
    await client.publish("rumqttc/native/interrupt", b"")
    assert isinstance(await asyncio.wait_for(anext(events), timeout=2), Disconnected)

    gated = [
        asyncio.create_task(client.enqueue_publish("capability/qos1", b"1", PublishOptions(qos=QoS.AT_LEAST_ONCE))),
        asyncio.create_task(client.enqueue_publish("capability/qos2", b"2", PublishOptions(qos=QoS.EXACTLY_ONCE))),
        asyncio.create_task(client.enqueue_publish("capability/retain", b"r", PublishOptions(retain=True))),
        asyncio.create_task(
            client.enqueue_publish(
                "capability/alias",
                b"a",
                PublishOptions(properties=V5PublishProperties(topic_alias=1)),
            )
        ),
    ]
    neutral = asyncio.create_task(client.enqueue_publish("capability/qos0", b"0"))
    await asyncio.wait_for(neutral, timeout=0.15)
    assert all(not operation.done() for operation in gated)
    assert isinstance(await asyncio.wait_for(anext(events), timeout=2), Connected)
    await asyncio.wait_for(asyncio.gather(*gated), timeout=2)
    await client.close()


@pytest.mark.asyncio
async def test_mqtt5_empty_publish_topic_reuses_only_a_mapped_alias() -> None:
    client = MqttClient(broker_options(ProtocolVersion.MQTT_5_0, "python-capability-alias-reuse"))
    await client.connect()
    aliased = PublishOptions(qos=QoS.AT_LEAST_ONCE, properties=V5PublishProperties(topic_alias=1))

    with pytest.raises(MqttError) as unmapped:
        await client.publish("", b"unmapped", aliased)
    assert unmapped.value.kind.value == "admission"
    assert unmapped.value.delivery.value == "notAdmitted"

    await client.publish("rumqttc/python/aliased", b"mapping", aliased)
    await client.publish("", b"publish-reuse", aliased)
    await client.enqueue_publish("", b"enqueue-reuse", aliased)
    await client.close()


if __name__ == "__main__":
    test_root = Path(__file__).parents[1]
    raise SystemExit(pytest.main(["-q", str(Path(__file__)), str(test_root / "test_lifecycle.py")]))
