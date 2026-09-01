from __future__ import annotations

import asyncio
import gc
import inspect
import json
import math
import threading
import typing
import weakref

import pytest
from rumqttc import (
    Acknowledgement,
    ClientClosedError,
    ClientStateError,
    ConfigurationError,
    Connected,
    Disconnected,
    MqttClient,
    MqttClientOptions,
    ProtocolVersion,
    PublishOptions,
    SubscribeOptions,
    Subscription,
    TcpTransport,
    TlsOptions,
    TlsTransport,
    UnsubscribeOptions,
    V5PublishProperties,
    V5SubscriptionOptions,
    WebSocketTransport,
    WssTransport,
)


def options(**changes: object) -> MqttClientOptions:
    values: dict[str, object] = {
        "protocol": ProtocolVersion.MQTT_5_0,
        "broker_host": "127.0.0.1",
        "broker_port": 1883,
        "client_id": "test",
        "transport": TcpTransport(),
    }
    values.update(changes)
    return MqttClientOptions(**values)  # type: ignore[arg-type]


@pytest.mark.parametrize("value", [True, 0, -1, 65536])
def test_port_validation(value: object) -> None:
    with pytest.raises((TypeError, ValueError)):
        MqttClient(options(broker_port=value))


@pytest.mark.parametrize("value", [math.nan, math.inf, -1.0, 0.5])
def test_keep_alive_validation(value: float) -> None:
    with pytest.raises(ValueError):
        MqttClient(options(keep_alive=value))


def test_v4_password_requires_username() -> None:
    with pytest.raises(ValueError, match="requires a username"):
        MqttClient(options(protocol=ProtocolVersion.MQTT_3_1_1, password=b"secret"))


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("broker_host", ""),
        ("request_capacity", True),
        ("request_capacity", 0),
        ("event_capacity", True),
        ("event_capacity", 0),
        ("incoming_packet_size_limit", True),
        ("incoming_packet_size_limit", 0),
        ("keep_alive", True),
        ("keep_alive", 65_536),
        ("connection_timeout", 0),
        ("connection_timeout", math.inf),
        ("event_delivery_timeout", 0),
        ("event_delivery_timeout", 0.0009),
        ("emit_outgoing_events", 1),
        ("ack_mode", "manual"),
        ("protocol", "5.0"),
        ("transport", object()),
        ("password", b"x" * 65_536),
        ("clean_session", False),
        ("clean_start", 1),
        ("session_expiry_interval", True),
        ("session_expiry_interval", 2**32),
    ],
    ids=lambda value: value if isinstance(value, str) else type(value).__name__,
)
def test_every_bounded_client_option_is_validated(field: str, value: object) -> None:
    with pytest.raises((TypeError, ValueError)):
        MqttClient(options(**{field: value}))


def test_each_protocol_rejects_the_other_versions_session_fields() -> None:
    with pytest.raises(ValueError, match="MQTT 5 session"):
        MqttClient(options(protocol=ProtocolVersion.MQTT_3_1_1, clean_start=False))
    with pytest.raises(ValueError, match="MQTT 5 session"):
        MqttClient(options(protocol=ProtocolVersion.MQTT_3_1_1, session_expiry_interval=0))


@pytest.mark.parametrize(
    "transport",
    [
        WebSocketTransport("http://localhost/mqtt"),
        WebSocketTransport("ws://"),
        WebSocketTransport("ws://localhost:65536/mqtt"),
        WssTransport("ws://localhost/mqtt"),
        TlsTransport(TlsOptions(client_certificate=b"certificate")),
        TlsTransport(TlsOptions(private_key=b"key")),
    ],
)
def test_transport_combinations_are_validated(transport: object) -> None:
    with pytest.raises((TypeError, ValueError)):
        MqttClient(options(transport=transport))


def test_native_configuration_failure_uses_wrapper_hierarchy() -> None:
    with pytest.raises(ConfigurationError) as failure:
        MqttClient(options(client_id="invalid\x00identifier"))
    assert failure.value.code == "CONFIGURATION_INVALID"


@pytest.mark.parametrize("field", ["client_id", "username"])
@pytest.mark.parametrize(
    "value",
    [b"bytes", "bad\x00value", "\ud800", pytest.param("é" * 32_768, id="oversized-utf8")],
)
def test_connect_strings_validate_type_encoding_and_encoded_length(field: str, value: object) -> None:
    with pytest.raises((ConfigurationError, TypeError, ValueError)):
        MqttClient(options(**{field: value}))


@pytest.mark.parametrize("field", ["client_id", "username"])
def test_connect_strings_accept_the_mqtt_encoded_length_boundary(field: str) -> None:
    client = MqttClient(options(**{field: "x" * 65_535}))
    client._native.abandon()
    client._finalizer.detach()


def test_events_require_a_running_loop() -> None:
    client = MqttClient(options())
    with pytest.raises(ClientStateError, match="running asyncio"):
        client.events()


@pytest.mark.asyncio
async def test_operations_require_connect() -> None:
    client = MqttClient(options())
    with pytest.raises(ClientStateError, match=r"connect\(\)"):
        await client.publish("topic", b"payload")
    await client.close_now()


@pytest.mark.asyncio
async def test_events_drain_initial_connection_retries() -> None:
    async def reject_connection(_reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        writer.close()
        await writer.wait_closed()

    server = await asyncio.start_server(reject_connection, "127.0.0.1", 0)
    port = server.sockets[0].getsockname()[1]
    try:
        client = MqttClient(
            options(
                broker_port=port,
                connection_timeout=1,
                event_capacity=1,
                event_delivery_timeout=1,
            )
        )
        connecting = asyncio.create_task(client.connect())
        await asyncio.sleep(0)
        events = client.events()

        for _ in range(2):
            event = await asyncio.wait_for(anext(events), timeout=5)
            assert isinstance(event, Disconnected)
            assert event.reconnecting
        assert not connecting.done()

        await client.close_now()
        with pytest.raises(ClientClosedError):
            await connecting
    finally:
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
@pytest.mark.parametrize("timeout", [-1, math.nan, math.inf, "invalid"])
async def test_invalid_close_timeout_does_not_close_client(timeout: object) -> None:
    client = MqttClient(options())
    with pytest.raises((TypeError, ValueError)):
        await client.close(timeout=timeout)  # type: ignore[arg-type]

    with pytest.raises(ClientStateError) as failure:
        await client.publish("topic", b"payload")
    assert failure.value.code == "CLIENT_NOT_CONNECTED"
    await client.close_now()


@pytest.mark.asyncio
@pytest.mark.parametrize("immediate", [False, True])
async def test_shutdown_synchronizes_with_scheduled_connect_start(*, immediate: bool) -> None:
    client = MqttClient(options())
    connecting = asyncio.create_task(client.connect())
    await asyncio.sleep(0)

    if immediate:
        await client.close_now()
    else:
        await client.close()
    with pytest.raises(ClientClosedError):
        await asyncio.wait_for(connecting, timeout=1)


@pytest.mark.asyncio
async def test_cancelled_connect_does_not_retain_abandoned_client() -> None:
    client = MqttClient(options(broker_port=1))
    reference = weakref.ref(client)
    connecting = asyncio.create_task(client.connect())
    await asyncio.sleep(0)
    assert client._connect_task is not None

    connecting.cancel()
    with pytest.raises(asyncio.CancelledError):
        await connecting
    assert client._connect_task is not None and not client._connect_task.cancelled()
    del connecting

    with pytest.warns(ResourceWarning, match="garbage-collected while open"):
        del client
        for _ in range(10):
            gc.collect()
            if reference() is None:
                break
            await asyncio.sleep(0)
    assert reference() is None

    # Let immediate native shutdown resolve the now-orphaned internal task. Its done callback must
    # observe the terminal exception rather than emitting "Task exception was never retrieved".
    await asyncio.sleep(0.05)


@pytest.mark.asyncio
async def test_concurrent_connect_waiters_cancel_independently() -> None:
    client = MqttClient(options(broker_port=1))
    waiters = [asyncio.create_task(client.connect()) for _ in range(10)]
    await asyncio.sleep(0)
    for waiter in waiters[::2]:
        waiter.cancel()
    cancelled = await asyncio.gather(*waiters[::2], return_exceptions=True)
    assert all(isinstance(result, asyncio.CancelledError) for result in cancelled)

    await client.close_now()
    active = await asyncio.gather(*waiters[1::2], return_exceptions=True)
    assert all(isinstance(result, ClientClosedError) for result in active)


@pytest.mark.asyncio
async def test_abandoned_event_iterator_releases_single_consumer(monkeypatch: pytest.MonkeyPatch) -> None:
    client = MqttClient(options())
    client._connected = True

    async def next_event() -> Connected:
        return Connected(ProtocolVersion.MQTT_5_0, False)

    monkeypatch.setattr(client, "_next_event", next_event)
    async for event in client.events():
        assert isinstance(event, Connected)
        break

    gc.collect()
    replacement = client.events()
    del replacement
    gc.collect()
    await client.close_now()


@pytest.mark.asyncio
async def test_tracked_completion_releases_admission_permit(monkeypatch: pytest.MonkeyPatch) -> None:
    publish_waiting = asyncio.Event()
    finish_publish = asyncio.Event()
    acknowledgement_admitted = asyncio.Event()

    class PendingPublish:
        async def wait(self) -> str:
            publish_waiting.set()
            await finish_publish.wait()
            return '{"ok":true,"operationId":"1","result":{"milestone":"qos1Acknowledged"}}'

    class ImmediateAcknowledgement:
        async def wait(self) -> str:
            return '{"ok":true,"operationId":"2","result":{"type":"acknowledged"}}'

    class NativeStub:
        async def publish(self, topic: str, payload: bytes, options: str | None) -> tuple[str, PendingPublish]:
            return '{"ok":true,"operationId":"1"}', PendingPublish()

        async def acknowledge(self, acknowledgement_id: int) -> tuple[str, ImmediateAcknowledgement]:
            acknowledgement_admitted.set()
            return '{"ok":true,"operationId":"2"}', ImmediateAcknowledgement()

    client = MqttClient(options(request_capacity=1))
    client._connected = True
    with monkeypatch.context() as patch:
        patch.setattr(client, "_native", NativeStub())
        publishing = asyncio.create_task(client.publish("outgoing", b"payload"))
        try:
            await asyncio.wait_for(publish_waiting.wait(), timeout=1)

            await asyncio.wait_for(client._acknowledge(7), timeout=1)
            assert acknowledgement_admitted.is_set()
        finally:
            finish_publish.set()
            await asyncio.gather(publishing, return_exceptions=True)
    await client.close_now()


@pytest.mark.asyncio
async def test_acknowledgement_bypasses_saturated_request_admission(monkeypatch: pytest.MonkeyPatch) -> None:
    publish_admission_waiting = asyncio.Event()
    finish_publish_admission = asyncio.Event()
    acknowledgement_admitted = asyncio.Event()

    class ImmediatePublish:
        async def wait(self) -> str:
            return '{"ok":true,"operationId":"1","result":{"milestone":"qos1Acknowledged"}}'

    class ImmediateAcknowledgement:
        async def wait(self) -> str:
            return '{"ok":true,"operationId":"2","result":{"type":"acknowledged"}}'

    class NativeStub:
        async def publish(self, topic: str, payload: bytes, options: str | None) -> tuple[str, ImmediatePublish]:
            publish_admission_waiting.set()
            await finish_publish_admission.wait()
            return '{"ok":true,"operationId":"1"}', ImmediatePublish()

        async def acknowledge(self, acknowledgement_id: int) -> tuple[str, ImmediateAcknowledgement]:
            acknowledgement_admitted.set()
            return '{"ok":true,"operationId":"2"}', ImmediateAcknowledgement()

    client = MqttClient(options(request_capacity=1))
    client._connected = True
    with monkeypatch.context() as patch:
        patch.setattr(client, "_native", NativeStub())
        publishing = asyncio.create_task(client.publish("outgoing", b"payload"))
        try:
            await asyncio.wait_for(publish_admission_waiting.wait(), timeout=1)
            await asyncio.wait_for(client._acknowledge(7), timeout=1)
            assert acknowledgement_admitted.is_set()
        finally:
            finish_publish_admission.set()
            await asyncio.gather(publishing, return_exceptions=True)
    await client.close_now()


@pytest.mark.asyncio
async def test_cancelled_acknowledgement_retains_admission_until_native_completion(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    first_waiting = asyncio.Event()
    finish_first = asyncio.Event()
    second_admitted = asyncio.Event()
    admitted: list[int] = []

    class Completion:
        def __init__(self, acknowledgement_id: int) -> None:
            self.acknowledgement_id = acknowledgement_id

        async def wait(self) -> str:
            if self.acknowledgement_id == 1:
                first_waiting.set()
                await finish_first.wait()
            return '{"ok":true,"operationId":"1","result":{"type":"acknowledged"}}'

    class NativeStub:
        async def acknowledge(self, acknowledgement_id: int) -> tuple[str, Completion]:
            admitted.append(acknowledgement_id)
            if acknowledgement_id == 2:
                second_admitted.set()
            return '{"ok":true,"operationId":"1"}', Completion(acknowledgement_id)

    client = MqttClient(options(request_capacity=1))
    client._connected = True
    with monkeypatch.context() as patch:
        patch.setattr(client, "_native", NativeStub())
        first = asyncio.create_task(client._acknowledge(1))
        await asyncio.wait_for(first_waiting.wait(), timeout=1)
        first.cancel()
        with pytest.raises(asyncio.CancelledError):
            await first

        second = asyncio.create_task(client._acknowledge(2))
        await asyncio.sleep(0)
        assert admitted == [1]
        assert not second_admitted.is_set()

        finish_first.set()
        await asyncio.wait_for(second_admitted.wait(), timeout=1)
        await asyncio.wait_for(second, timeout=1)
        assert admitted == [1, 2]
    await client.close_now()


@pytest.mark.asyncio
async def test_unsubscribe_rejects_bare_string() -> None:
    client = MqttClient(options())
    client._connected = True
    with pytest.raises(TypeError, match="sequence of strings"):
        await client.unsubscribe("topic")
    await client.close_now()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "topic",
    ["", "bad/+", "bad/#", "bad\x00topic", pytest.param("x" * 65_536, id="oversized")],
)
async def test_publish_topics_fail_before_native_admission(topic: str) -> None:
    client = MqttClient(options())
    client._connected = True
    with pytest.raises(ValueError):
        await client.publish(topic, b"payload")
    await client.close_now()


@pytest.mark.asyncio
@pytest.mark.parametrize("operation", ["publish", "enqueue_publish"])
async def test_mqtt5_empty_aliased_topic_reaches_native_admission(
    operation: str, monkeypatch: pytest.MonkeyPatch
) -> None:
    calls: list[tuple[str, bytes, str | None]] = []

    class Completion:
        async def wait(self) -> str:
            return '{"ok":true,"operationId":"1","result":{"milestone":"qos0Flushed"}}'

    class NativeStub:
        async def publish(self, topic: str, payload: bytes, value: str | None) -> tuple[str, Completion]:
            calls.append((topic, payload, value))
            return '{"ok":true,"operationId":"1"}', Completion()

        async def enqueue_publish(self, topic: str, payload: bytes, value: str | None) -> str:
            calls.append((topic, payload, value))
            return '{"ok":true,"operationId":"1"}'

    client = MqttClient(options())
    client._connected = True
    with monkeypatch.context() as patch:
        patch.setattr(client, "_native", NativeStub())
        publish = getattr(client, operation)
        await publish("", b"payload", PublishOptions(properties=V5PublishProperties(topic_alias=1)))

    assert len(calls) == 1
    topic, payload, encoded_options = calls[0]
    assert topic == ""
    assert payload == b"payload"
    assert encoded_options is not None
    assert json.loads(encoded_options)["properties"]["topicAlias"] == 1
    await client.close_now()


@pytest.mark.asyncio
@pytest.mark.parametrize("operation", ["publish", "enqueue_publish"])
async def test_text_payload_with_invalid_utf8_fails_before_native_admission(
    operation: str, monkeypatch: pytest.MonkeyPatch
) -> None:
    called = False

    class NativeStub:
        async def publish(self, topic: str, payload: bytes, value: str | None) -> object:
            nonlocal called
            called = True
            raise AssertionError

        enqueue_publish = publish

    client = MqttClient(options())
    client._connected = True
    with monkeypatch.context() as patch:
        patch.setattr(client, "_native", NativeStub())
        publish = getattr(client, operation)
        with pytest.raises(ValueError, match="well-formed UTF-8"):
            await publish(
                "topic",
                b"\xff",
                PublishOptions(properties=V5PublishProperties(payload_format_indicator=1)),
            )

    assert not called
    await client.close_now()


@pytest.mark.asyncio
@pytest.mark.parametrize("topic_filter", ["", "a/#/b", "a+", "a/+b", "$share//topic", "bad\x00filter"])
async def test_topic_filters_fail_before_native_admission(topic_filter: str) -> None:
    client = MqttClient(options())
    client._connected = True
    with pytest.raises(ValueError):
        await client.subscribe([Subscription(topic_filter)])
    with pytest.raises(ValueError):
        await client.unsubscribe([topic_filter])
    await client.close_now()


@pytest.mark.asyncio
async def test_no_local_shared_subscription_fails_before_native_admission(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    called = False

    class NativeStub:
        async def subscribe(self, filters: str, value: str | None) -> object:
            nonlocal called
            called = True
            raise AssertionError

    client = MqttClient(options())
    client._connected = True
    with monkeypatch.context() as patch:
        patch.setattr(client, "_native", NativeStub())
        with pytest.raises(ValueError, match="no_local must be false"):
            await client.subscribe([Subscription("$share/group/topic", options=V5SubscriptionOptions(no_local=True))])

    assert not called
    await client.close_now()


@pytest.mark.asyncio
async def test_publish_and_subscription_fields_reject_bool_and_bad_property_shapes() -> None:
    client = MqttClient(options())
    client._connected = True
    with pytest.raises(TypeError):
        await client.publish("topic", b"payload", PublishOptions(qos=True))  # type: ignore[arg-type]
    with pytest.raises(TypeError):
        await client.publish("topic", b"payload", PublishOptions(retain=1))  # type: ignore[arg-type]
    with pytest.raises(TypeError):
        await client.publish("topic", b"payload", PublishOptions(properties={}))  # type: ignore[arg-type]
    with pytest.raises(TypeError):
        await client.publish(
            "topic",
            b"payload",
            PublishOptions(properties=V5PublishProperties(topic_alias=True)),  # type: ignore[arg-type]
        )
    with pytest.raises(TypeError):
        await client.publish(
            "topic",
            b"payload",
            PublishOptions(properties=V5PublishProperties(user_properties=(("key", 1),))),  # type: ignore[arg-type]
        )
    with pytest.raises(TypeError):
        await client.subscribe(
            [
                Subscription(
                    "topic",
                    options=V5SubscriptionOptions(retain_forward_rule=True),  # type: ignore[arg-type]
                )
            ]
        )
    with pytest.raises((TypeError, ValueError)):
        await client.subscribe([Subscription("topic", qos=True)])  # type: ignore[arg-type]
    with pytest.raises(TypeError):
        await client.subscribe([Subscription("topic", options={})])  # type: ignore[arg-type]
    await client.close_now()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "properties",
    [
        V5PublishProperties(response_topic="bad/+"),
        V5PublishProperties(correlation_data=b"x" * 65_536),
        V5PublishProperties(content_type="bad\x00type"),
        V5PublishProperties(payload_format_indicator=2),
        V5PublishProperties(topic_alias=0),
        V5PublishProperties(message_expiry_interval=True),  # type: ignore[arg-type]
        V5PublishProperties(message_expiry_interval=2**32),
        V5PublishProperties(user_properties=(("bad\x00key", "value"),)),
    ],
)
async def test_every_publish_property_is_validated_before_admission(properties: V5PublishProperties) -> None:
    client = MqttClient(options())
    client._connected = True
    with pytest.raises((TypeError, ValueError)):
        await client.publish("topic", b"payload", PublishOptions(properties=properties))
    await client.close_now()


@pytest.mark.asyncio
async def test_subscribe_and_unsubscribe_property_scopes_validate_all_fields() -> None:
    client = MqttClient(options())
    client._connected = True
    with pytest.raises(TypeError):
        await client.subscribe(
            [Subscription("topic", options=V5SubscriptionOptions(no_local=1))]  # type: ignore[arg-type]
        )
    with pytest.raises(TypeError):
        await client.subscribe(
            [Subscription("topic", options=V5SubscriptionOptions(retain_as_published=1))]  # type: ignore[arg-type]
        )
    with pytest.raises(ValueError):
        await client.subscribe([Subscription("topic")], options=SubscribeOptions(subscription_identifier=0))
    with pytest.raises(TypeError):
        await client.subscribe(
            [Subscription("topic")],
            options=SubscribeOptions(user_properties=(("key", 1),)),  # type: ignore[arg-type]
        )
    with pytest.raises(ValueError):
        await client.unsubscribe(["topic"], options=UnsubscribeOptions(user_properties=(("bad\x00key", "value"),)))
    await client.close_now()


@pytest.mark.asyncio
async def test_mqtt5_command_scopes_are_rejected_for_mqtt311() -> None:
    client = MqttClient(options(protocol=ProtocolVersion.MQTT_3_1_1))
    client._connected = True
    with pytest.raises(ValueError, match="PUBLISH properties"):
        await client.publish("topic", b"payload", PublishOptions(properties=V5PublishProperties()))
    with pytest.raises(ValueError, match="per-filter"):
        await client.subscribe([Subscription("topic", options=V5SubscriptionOptions())])
    with pytest.raises(ValueError, match="SUBSCRIBE properties"):
        await client.subscribe([Subscription("topic")], options=SubscribeOptions())
    with pytest.raises(ValueError, match="UNSUBSCRIBE properties"):
        await client.unsubscribe(["topic"], options=UnsubscribeOptions())
    await client.close_now()


async def cancellable_operation(client: MqttClient, operation: str) -> object:
    if operation == "publish":
        return await client.publish("topic", b"payload")
    if operation == "subscribe":
        return await client.subscribe([Subscription("topic")])
    if operation == "unsubscribe":
        return await client.unsubscribe(["topic"])
    return await Acknowledgement(client, 7).ack()


@pytest.mark.asyncio
@pytest.mark.parametrize("operation", ["publish", "subscribe", "unsubscribe"])
async def test_cancellation_while_waiting_for_python_admission_does_not_call_native(
    operation: str, monkeypatch: pytest.MonkeyPatch
) -> None:
    called = False

    class NativeStub:
        async def publish(self, topic: str, payload: bytes, value: str | None) -> object:
            nonlocal called
            called = True
            raise AssertionError

        subscribe = publish
        unsubscribe = publish

    client = MqttClient(options(request_capacity=1))
    client._connected = True
    with monkeypatch.context() as patch:
        patch.setattr(client, "_native", NativeStub())
        await client._admission.acquire()
        pending = asyncio.create_task(cancellable_operation(client, operation))
        await asyncio.sleep(0)
        pending.cancel()
        with pytest.raises(asyncio.CancelledError):
            await pending
        assert not called
        client._admission.release()
    await client.close_now()


@pytest.mark.asyncio
@pytest.mark.parametrize("operation", ["publish", "subscribe", "unsubscribe", "acknowledge"])
async def test_cancellation_after_native_admission_drops_only_the_python_waiter(
    operation: str, monkeypatch: pytest.MonkeyPatch
) -> None:
    waiting = asyncio.Event()
    finish = asyncio.Event()

    class Completion:
        async def wait(self) -> str:
            waiting.set()
            await finish.wait()
            result = {
                "publish": {"milestone": "qos0Flushed"},
                "subscribe": {"results": [{"granted": True, "qos": 0}]},
                "unsubscribe": {"results": None},
                "acknowledge": {"type": "acknowledged"},
            }[operation]
            return '{"ok":true,"operationId":"1","result":' + json.dumps(result) + "}"

    class NativeStub:
        async def publish(self, topic: str, payload: bytes, value: str | None) -> tuple[str, Completion]:
            return '{"ok":true,"operationId":"1"}', Completion()

        async def subscribe(self, filters: str, value: str | None) -> tuple[str, Completion]:
            return '{"ok":true,"operationId":"1"}', Completion()

        async def unsubscribe(self, filters: str, value: str | None) -> tuple[str, Completion]:
            return '{"ok":true,"operationId":"1"}', Completion()

        async def acknowledge(self, acknowledgement_id: int) -> tuple[str, Completion]:
            return '{"ok":true,"operationId":"1"}', Completion()

    client = MqttClient(options())
    client._connected = True
    with monkeypatch.context() as patch:
        patch.setattr(client, "_native", NativeStub())
        pending = asyncio.create_task(cancellable_operation(client, operation))
        await asyncio.wait_for(waiting.wait(), timeout=1)
        pending.cancel()
        with pytest.raises(asyncio.CancelledError):
            await pending
        finish.set()
        await asyncio.sleep(0)
    await client.close_now()


@pytest.mark.asyncio
@pytest.mark.parametrize("previously_connected", [False, True])
async def test_closed_client_uses_closed_error_consistently(*, previously_connected: bool) -> None:
    client = MqttClient(options())
    client._connected = previously_connected
    await client.close_now()

    with pytest.raises(ClientClosedError) as connect_failure:
        await client.connect()
    assert connect_failure.value.code == "CLIENT_CLOSED"

    with pytest.raises(ClientClosedError) as publish_failure:
        await client.publish("topic", b"payload")
    assert publish_failure.value.code == "CLIENT_CLOSED"

    with pytest.raises(ClientClosedError) as events_failure:
        client.events()
    assert events_failure.value.code == "CLIENT_CLOSED"


def test_client_rejects_use_from_another_loop() -> None:
    client = MqttClient(options())

    async def bind() -> None:
        with pytest.raises(ClientStateError, match=r"connect\(\)"):
            client.events()

    async def use_elsewhere() -> None:
        with pytest.raises(ClientStateError, match="different asyncio"):
            await client.close_now()

    asyncio.run(bind())
    asyncio.run(use_elsewhere())


def test_public_exports_and_callable_annotations_form_a_runtime_contract() -> None:
    import rumqttc

    assert rumqttc.__all__ == sorted(rumqttc.__all__)
    assert len(rumqttc.__all__) == len(set(rumqttc.__all__))
    assert all(not name.startswith("_") and hasattr(rumqttc, name) for name in rumqttc.__all__)

    for method_name in (
        "__init__",
        "connect",
        "enqueue_publish",
        "publish",
        "subscribe",
        "unsubscribe",
        "events",
        "diagnostics",
        "close",
        "close_now",
        "__aenter__",
        "__aexit__",
    ):
        method = getattr(MqttClient, method_name)
        hints = typing.get_type_hints(method)
        signature = inspect.signature(method)
        assert "return" in hints
        assert all(parameter == "self" or parameter in hints for parameter in signature.parameters)


def test_manually_created_loop_and_threadsafe_scheduling_use_the_owner_loop() -> None:
    loop = asyncio.new_event_loop()
    client = MqttClient(options())

    async def bind() -> None:
        with pytest.raises(ClientStateError, match=r"connect\(\)"):
            client.events()

    loop.run_until_complete(bind())
    result: list[BaseException | None] = []

    def schedule() -> None:
        future = asyncio.run_coroutine_threadsafe(client.close_now(), loop)
        try:
            future.result(timeout=2)
        except BaseException as error:
            result.append(error)
        else:
            result.append(None)

    thread = threading.Thread(target=schedule)
    thread.start()
    while thread.is_alive():
        loop.run_until_complete(asyncio.sleep(0))
    thread.join(timeout=1)
    loop.close()
    assert result == [None]
