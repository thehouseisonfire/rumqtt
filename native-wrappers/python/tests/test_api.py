from __future__ import annotations

import asyncio
import gc
import math
import weakref

import pytest
from rumqttc import (
    ClientClosedError,
    ClientStateError,
    ConfigurationError,
    Connected,
    Disconnected,
    MqttClient,
    MqttClientOptions,
    ProtocolVersion,
    TcpTransport,
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


def test_native_configuration_failure_uses_wrapper_hierarchy() -> None:
    with pytest.raises(ConfigurationError) as failure:
        MqttClient(options(client_id="invalid\x00identifier"))
    assert failure.value.code == "CONFIGURATION_INVALID"


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
async def test_unsubscribe_rejects_bare_string() -> None:
    client = MqttClient(options())
    client._connected = True
    with pytest.raises(TypeError, match="sequence of strings"):
        await client.unsubscribe("topic")
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
