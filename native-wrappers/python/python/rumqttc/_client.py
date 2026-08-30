from __future__ import annotations

import asyncio
import atexit
import base64
import contextlib
import json
import math
import sys
import time
import warnings
import weakref
from collections.abc import AsyncIterator, Sequence
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlsplit

from . import _native
from ._errors import ClientClosedError, ClientStateError, ConfigurationError, DeliveryStatus, ErrorKind, error_from_data
from ._events import (
    Acknowledgement,
    Closed,
    Connected,
    Disconnected,
    DriverError,
    IncomingPublish,
    MqttEvent,
    Outgoing,
)
from ._types import (
    AckMode,
    AdmissionResult,
    ClientDiagnostics,
    ConnectionPhase,
    ConnectResult,
    MqttClientOptions,
    OutgoingActivity,
    ProtocolVersion,
    PublishCompletion,
    PublishMilestone,
    PublishOptions,
    QoS,
    RetainForwardRule,
    SubscribeCompletion,
    SubscribeOptions,
    SubscribeResult,
    Subscription,
    TcpTransport,
    TlsOptions,
    TlsTransport,
    UnsubscribeCompletion,
    UnsubscribeOptions,
    UnsubscribeResult,
    V5IncomingPublishProperties,
    V5PublishProperties,
    V5SubscriptionOptions,
    WebSocketTransport,
    WssTransport,
)

_live_clients: weakref.WeakSet[MqttClient] = weakref.WeakSet()
_finalizing = False


@dataclass(frozen=True, slots=True)
class _CheckedPublishOptions:
    encoded: str | None
    has_topic_alias: bool
    payload_format_indicator: int | None


def _abandon(native: _native.NativeMqttClient) -> None:
    if not _finalizing:
        warnings.warn("MqttClient was garbage-collected while open", ResourceWarning, stacklevel=2)
    native.abandon()


def _shutdown_clients() -> None:
    global _finalizing
    _finalizing = True
    clients = list(_live_clients)
    for client in clients:
        client._native.abandon()
    deadline = time.monotonic() + 5.0
    for client in clients:
        client._native.cleanup(max(0, int((deadline - time.monotonic()) * 1_000)))


atexit.register(_shutdown_clients)


def _state_error(code: str, message: str) -> ClientStateError:
    return ClientStateError(
        message,
        code=code,
        kind=ErrorKind.ADMISSION,
        retryable=False,
        delivery=DeliveryStatus.NOT_ADMITTED,
    )


def _closed_error() -> ClientClosedError:
    return ClientClosedError(
        "client is closing or closed",
        code="CLIENT_CLOSED",
        kind=ErrorKind.SHUTDOWN,
        retryable=False,
        delivery=DeliveryStatus.NOT_ADMITTED,
    )


def _integer(value: object, name: str, maximum: int, *, nonzero: bool = False) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"{name} must be an integer")
    if value < (1 if nonzero else 0) or value > maximum:
        qualifier = "nonzero and " if nonzero else ""
        raise ValueError(f"{name} must be {qualifier}between 0 and {maximum}")
    return value


def _boolean(value: object, name: str) -> bool:
    if not isinstance(value, bool):
        raise TypeError(f"{name} must be bool")
    return value


def _string(value: object, name: str) -> str:
    if not isinstance(value, str):
        raise TypeError(f"{name} must be str")
    return value


def _mqtt_string(value: object, name: str, *, allow_empty: bool = True) -> str:
    result = _string(value, name)
    try:
        encoded = result.encode("utf-8")
    except UnicodeEncodeError as error:
        raise ValueError(f"{name} must be well-formed UTF-8") from error
    if not allow_empty and not result:
        raise ValueError(f"{name} must not be empty")
    if len(encoded) > 2**16 - 1:
        raise ValueError(f"{name} exceeds the MQTT UTF-8 string limit")
    if "\x00" in result:
        raise ValueError(f"{name} must not contain U+0000")
    return result


def _topic(value: object, name: str = "topic", *, allow_empty: bool = False) -> str:
    result = _mqtt_string(value, name, allow_empty=allow_empty)
    if "+" in result or "#" in result:
        raise ValueError(f"{name} must not contain topic-filter wildcards")
    return result


def _topic_filter(value: object, name: str = "topic filter") -> str:
    result = _mqtt_string(value, name, allow_empty=False)
    for index, level in enumerate(result.split("/")):
        if "#" in level and (level != "#" or index != len(result.split("/")) - 1):
            raise ValueError(f"{name} uses '#' outside the final complete level")
        if "+" in level and level != "+":
            raise ValueError(f"{name} uses '+' outside a complete level")
    if result.startswith("$share/"):
        parts = result.split("/", 2)
        if len(parts) != 3 or not parts[1] or "+" in parts[1] or "#" in parts[1] or not parts[2]:
            raise ValueError(f"{name} contains an invalid shared-subscription prefix")
    return result


def _user_properties(value: object, name: str) -> tuple[tuple[str, str], ...]:
    if isinstance(value, (str, bytes, bytearray, memoryview)) or not isinstance(value, Sequence):
        raise TypeError(f"{name} must be a sequence of string pairs")
    checked: list[tuple[str, str]] = []
    for item in value:
        if isinstance(item, (str, bytes, bytearray, memoryview)) or not isinstance(item, Sequence) or len(item) != 2:
            raise TypeError(f"{name} must contain two-item string pairs")
        checked.append(
            (
                _mqtt_string(item[0], f"{name} name"),
                _mqtt_string(item[1], f"{name} value"),
            )
        )
    return tuple(checked)


def _websocket_url(value: object, name: str, scheme: str) -> str:
    result = _string(value, name)
    try:
        parsed = urlsplit(result)
        port = parsed.port
    except ValueError as error:
        raise ValueError(f"{name} is malformed") from error
    if parsed.scheme != scheme or parsed.hostname is None or port == 0:
        raise ValueError(f"{name} must be an absolute {scheme}:// URL with a valid host")
    return result


def _qos(value: object) -> QoS:
    if isinstance(value, bool):
        raise TypeError("qos must be QoS")
    try:
        return QoS(value)
    except (TypeError, ValueError) as error:
        raise ValueError("qos must be QoS.AT_MOST_ONCE, AT_LEAST_ONCE, or EXACTLY_ONCE") from error


def _seconds(value: object, name: str, *, nonzero: bool = False, integral: bool = False) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise TypeError(f"{name} must be a number of seconds")
    result = float(value)
    if not math.isfinite(result) or result < 0 or (nonzero and result == 0):
        raise ValueError(f"{name} must be a finite {'positive' if nonzero else 'nonnegative'} value")
    if integral and not result.is_integer():
        raise ValueError(f"{name} must be an integral number of seconds")
    if result > (2**64 - 1) / 1_000:
        raise ValueError(f"{name} exceeds the native duration range")
    return result


def _bytes(value: bytes | bytearray | memoryview, name: str) -> bytes:
    try:
        return bytes(memoryview(value))
    except (TypeError, ValueError) as error:
        raise TypeError(f"{name} must be bytes-like") from error


def _limited_binary(value: bytes | bytearray | memoryview, name: str) -> bytes:
    result = _bytes(value, name)
    if len(result) > 2**16 - 1:
        raise ValueError(f"{name} exceeds the MQTT binary-data limit")
    return result


def _b64(value: bytes | bytearray | memoryview | None, name: str) -> str | None:
    return None if value is None else base64.b64encode(_bytes(value, name)).decode("ascii")


def _tls(tls: TlsOptions) -> dict[str, Any]:
    if not isinstance(tls, TlsOptions):
        raise TypeError("tls must be TlsOptions")
    if (tls.client_certificate is None) != (tls.private_key is None):
        raise ValueError("TLS client certificate and private key must be supplied together")
    return {
        "caBase64": _b64(tls.ca, "TLS CA"),
        "clientCertificateBase64": _b64(tls.client_certificate, "TLS client certificate"),
        "privateKeyBase64": _b64(tls.private_key, "TLS private key"),
    }


def _config(options: MqttClientOptions) -> str:
    if not isinstance(options, MqttClientOptions):
        raise TypeError("options must be MqttClientOptions")
    port = _integer(options.broker_port, "broker_port", 2**16 - 1, nonzero=True)
    request_capacity = _integer(options.request_capacity, "request_capacity", sys.maxsize, nonzero=True)
    event_capacity = _integer(options.event_capacity, "event_capacity", sys.maxsize, nonzero=True)
    incoming_limit = _integer(
        options.incoming_packet_size_limit,
        "incoming_packet_size_limit",
        2**32 - 1,
        nonzero=True,
    )
    keep_alive = _seconds(options.keep_alive, "keep_alive", integral=True)
    if keep_alive > 2**16 - 1:
        raise ValueError("keep_alive exceeds the MQTT two-byte seconds limit")
    connection_timeout = _seconds(options.connection_timeout, "connection_timeout", nonzero=True, integral=True)
    event_timeout = _seconds(options.event_delivery_timeout, "event_delivery_timeout", nonzero=True)
    if event_timeout < 0.001:
        raise ValueError("event_delivery_timeout must be at least one millisecond")
    host = _string(options.broker_host, "broker_host")
    if not host or "\x00" in host:
        raise ValueError("broker_host must be nonempty and must not contain U+0000")
    # wrapper-core performs MQTT UTF-8 semantic checks for CONNECT strings so failures retain the
    # public ConfigurationError hierarchy. Type checking remains at the Python boundary.
    client_id = _string(options.client_id, "client_id")
    username = None if options.username is None else _string(options.username, "username")
    if not isinstance(options.ack_mode, AckMode):
        raise TypeError("ack_mode must be AckMode")
    emit_outgoing = _boolean(options.emit_outgoing_events, "emit_outgoing_events")
    transport: dict[str, Any]
    if isinstance(options.transport, TcpTransport):
        transport = {"kind": "tcp"}
    elif isinstance(options.transport, TlsTransport):
        transport = {"kind": "tls", **_tls(options.transport.tls)}
    elif isinstance(options.transport, WebSocketTransport):
        url = _websocket_url(options.transport.url, "WebSocket URL", "ws")
        transport = {"kind": "websocket", "url": url}
    elif isinstance(options.transport, WssTransport):
        url = _websocket_url(options.transport.url, "WSS URL", "wss")
        transport = {"kind": "wss", "url": url, **_tls(options.transport.tls)}
    else:
        raise TypeError("transport has an unsupported type")
    if options.protocol is ProtocolVersion.MQTT_3_1_1:
        if options.clean_start is not None or options.session_expiry_interval is not None:
            raise ValueError("MQTT 5 session options require protocol MQTT_5_0")
        if options.password is not None and options.username is None:
            raise ValueError("an MQTT 3.1.1 password requires a username")
    elif options.protocol is ProtocolVersion.MQTT_5_0:
        if options.clean_session is not None:
            raise ValueError("clean_session is only valid for MQTT 3.1.1")
    else:
        raise TypeError("protocol must be ProtocolVersion")
    expiry = options.session_expiry_interval
    if expiry is not None:
        expiry = _integer(expiry, "session_expiry_interval", 2**32 - 1)
    for value, name in ((options.clean_session, "clean_session"), (options.clean_start, "clean_start")):
        if value is not None:
            _boolean(value, name)
    password = None if options.password is None else _bytes(options.password, "password")
    if password is not None and len(password) > 2**16 - 1:
        raise ValueError("password exceeds the MQTT binary-data limit")
    return json.dumps(
        {
            "protocol": options.protocol.value,
            "brokerHost": host,
            "brokerPort": port,
            "clientId": client_id,
            "transport": transport,
            "keepAliveSeconds": int(keep_alive),
            "connectionTimeoutSeconds": int(connection_timeout),
            "username": username,
            "passwordBase64": _b64(password, "password"),
            "requestCapacity": request_capacity,
            "eventCapacity": event_capacity,
            "eventDeliveryTimeoutMs": int(event_timeout * 1_000),
            "ackMode": options.ack_mode.value,
            "incomingPacketSizeLimit": incoming_limit,
            "emitOutgoingEvents": emit_outgoing,
            "cleanSession": options.clean_session,
            "cleanStart": options.clean_start,
            "sessionExpiryInterval": expiry,
        }
    )


def _response(raw: str) -> dict[str, Any]:
    response: dict[str, Any] = json.loads(raw)
    if not response.get("ok"):
        raise error_from_data(response["error"])
    return response


def _tracked_completion(
    admission: tuple[str, _native.NativeCompletion | None],
) -> _native.NativeCompletion:
    response, completion = admission
    _response(response)
    if completion is None:
        raise RuntimeError("native admission succeeded without a completion handle")
    return completion


async def _connect_native(
    native: _native.NativeMqttClient,
    client_ref: weakref.ReferenceType[MqttClient],
) -> ConnectResult:
    response = _response(await native.connect())
    result = ConnectResult(ProtocolVersion(response["protocol"]), bool(response["sessionPresent"]))
    client = client_ref()
    if client is not None:
        client._connected = True
    return result


def _observe_connect_result(task: asyncio.Future[ConnectResult]) -> None:
    # Retrieving an orphaned task's exception prevents an event-loop diagnostic. Active waiters
    # still receive the same result or exception when they await the shared task.
    if not task.cancelled():
        task.exception()


def _observe_acknowledgement_result(task: asyncio.Future[None]) -> None:
    # A caller may stop awaiting an admitted acknowledgement, but its worker must continue to
    # hold admission capacity until native completion. Observe any eventual exception because
    # there may no longer be a Python waiter to retrieve it.
    if not task.cancelled():
        task.exception()


async def _wait_for_connect(task: asyncio.Task[ConnectResult]) -> ConnectResult:
    if task.done():
        return task.result()

    waiter = asyncio.get_running_loop().create_future()

    def relay_result(completed: asyncio.Future[ConnectResult]) -> None:
        if waiter.done():
            return
        if completed.cancelled():
            waiter.cancel()
        elif (error := completed.exception()) is not None:
            waiter.set_exception(error)
        else:
            waiter.set_result(completed.result())

    task.add_done_callback(relay_result)
    try:
        # Cancellation applies only to this caller's relay future. The shared connection task
        # continues reconnecting for other callers and for a client that remains referenced.
        return await waiter
    finally:
        task.remove_done_callback(relay_result)


class _EventIterator(AsyncIterator[MqttEvent]):
    def __init__(self, client: MqttClient) -> None:
        self._client: MqttClient | None = client

    def __aiter__(self) -> _EventIterator:
        return self

    async def __anext__(self) -> MqttEvent:
        client = self._client
        if client is None:
            raise StopAsyncIteration
        try:
            return await client._next_event()
        except StopAsyncIteration:
            self.close()
            raise

    def close(self) -> None:
        client, self._client = self._client, None
        if client is not None:
            active = client._event_iterator
            if active is not None and active() is self:
                client._event_iterator = None

    def __del__(self) -> None:
        self.close()


class MqttClient:
    def __init__(self, options: MqttClientOptions) -> None:
        configuration = _config(options)
        try:
            self._native = _native.NativeMqttClient(configuration)
        except ValueError as error:
            raise ConfigurationError(
                str(error),
                code="CONFIGURATION_INVALID",
                kind=ErrorKind.CONFIGURATION,
                retryable=False,
            ) from error
        self._loop: asyncio.AbstractEventLoop | None = None
        self._protocol = options.protocol
        self._connect_task: asyncio.Task[ConnectResult] | None = None
        self._connected = False
        self._closing = False
        self._event_iterator: weakref.ReferenceType[_EventIterator] | None = None
        self._admission = asyncio.Semaphore(options.request_capacity)
        # ACKs retain an independent bounded lane so publish saturation cannot deadlock protocol
        # progress, while an incoming burst still cannot create unbounded native ACK observations.
        self._ack_admission = asyncio.Semaphore(options.request_capacity)
        self._acknowledgement_workers: set[asyncio.Task[None]] = set()
        self._finalizer = weakref.finalize(self, _abandon, self._native)
        _live_clients.add(self)

    def _bind_loop(self) -> asyncio.AbstractEventLoop:
        try:
            loop = asyncio.get_running_loop()
        except RuntimeError as error:
            raise _state_error("NO_RUNNING_EVENT_LOOP", "operation requires a running asyncio event loop") from error
        if self._loop is None:
            self._loop = loop
        elif self._loop is not loop:
            raise _state_error("WRONG_EVENT_LOOP", "client belongs to a different asyncio event loop")
        return loop

    async def connect(self) -> ConnectResult:
        self._bind_loop()
        if self._closing:
            raise _closed_error()
        if self._connect_task is None:
            self._connect_task = asyncio.create_task(_connect_native(self._native, weakref.ref(self)))
            self._connect_task.add_done_callback(_observe_connect_result)
        return await _wait_for_connect(self._connect_task)

    def _require_connected(self) -> None:
        self._bind_loop()
        if self._closing:
            raise _closed_error()
        if not self._connected:
            raise _state_error("CLIENT_NOT_CONNECTED", "connect() must complete before this operation")

    async def enqueue_publish(
        self,
        topic: str,
        payload: bytes | bytearray | memoryview | str,
        options: PublishOptions | None = None,
    ) -> AdmissionResult:
        self._require_connected()
        checked_options = _publish_options(options, self._protocol)
        checked_topic = _topic(
            topic,
            "publish topic",
            allow_empty=self._protocol is ProtocolVersion.MQTT_5_0 and checked_options.has_topic_alias,
        )
        checked_payload = _publish_payload(payload, checked_options.payload_format_indicator)
        async with self._admission:
            response = _response(
                await self._native.enqueue_publish(checked_topic, checked_payload, checked_options.encoded)
            )
        return AdmissionResult(int(response["operationId"]))

    async def publish(
        self,
        topic: str,
        payload: bytes | bytearray | memoryview | str,
        options: PublishOptions | None = None,
    ) -> PublishCompletion:
        self._require_connected()
        checked_options = _publish_options(options, self._protocol)
        checked_topic = _topic(
            topic,
            "publish topic",
            allow_empty=self._protocol is ProtocolVersion.MQTT_5_0 and checked_options.has_topic_alias,
        )
        checked_payload = _publish_payload(payload, checked_options.payload_format_indicator)
        async with self._admission:
            completion = _tracked_completion(
                await self._native.publish(checked_topic, checked_payload, checked_options.encoded)
            )
        response = _response(await completion.wait())
        result = response["result"]
        return PublishCompletion(int(response["operationId"]), PublishMilestone(result["milestone"]))

    async def subscribe(
        self,
        subscriptions: Sequence[Subscription],
        *,
        options: SubscribeOptions | None = None,
    ) -> SubscribeCompletion:
        self._require_connected()
        if isinstance(subscriptions, (str, bytes, bytearray, memoryview)) or not isinstance(subscriptions, Sequence):
            raise TypeError("subscriptions must be a sequence of Subscription values")
        filters = [_subscription(value, self._protocol) for value in subscriptions]
        if not filters:
            raise ValueError("subscriptions must not be empty")
        if options is not None and not isinstance(options, SubscribeOptions):
            raise TypeError("options must be SubscribeOptions")
        if options is not None and self._protocol is ProtocolVersion.MQTT_3_1_1:
            raise ValueError("SUBSCRIBE properties require protocol MQTT_5_0")
        identifier = None
        if options is not None and options.subscription_identifier is not None:
            identifier = _integer(
                options.subscription_identifier,
                "subscription_identifier",
                268_435_455,
                nonzero=True,
            )
        packet = (
            None
            if options is None
            else {
                "subscriptionIdentifier": identifier,
                "userProperties": _user_properties(options.user_properties, "SUBSCRIBE user_properties"),
            }
        )
        async with self._admission:
            completion = _tracked_completion(await self._native.subscribe(json.dumps(filters), _json_optional(packet)))
        response = _response(await completion.wait())
        results = tuple(
            SubscribeResult(
                bool(item["granted"]), QoS(item["qos"]) if item["granted"] else None, item.get("brokerReason")
            )
            for item in response["result"]["results"]
        )
        return SubscribeCompletion(int(response["operationId"]), results)

    async def unsubscribe(
        self,
        filters: Sequence[str],
        *,
        options: UnsubscribeOptions | None = None,
    ) -> UnsubscribeCompletion:
        self._require_connected()
        if options is not None and not isinstance(options, UnsubscribeOptions):
            raise TypeError("options must be UnsubscribeOptions")
        if options is not None and self._protocol is ProtocolVersion.MQTT_3_1_1:
            raise ValueError("UNSUBSCRIBE properties require protocol MQTT_5_0")
        if isinstance(filters, str):
            raise TypeError("filters must be a sequence of strings, not str")
        if not isinstance(filters, Sequence):
            raise TypeError("filters must be a sequence of strings")
        checked_filters = [_topic_filter(value, "unsubscribe filter") for value in filters]
        if not checked_filters:
            raise ValueError("filters must not be empty")
        packet = (
            None
            if options is None
            else {"userProperties": _user_properties(options.user_properties, "UNSUBSCRIBE user_properties")}
        )
        async with self._admission:
            completion = _tracked_completion(
                await self._native.unsubscribe(json.dumps(checked_filters), _json_optional(packet))
            )
        response = _response(await completion.wait())
        values = response["result"].get("results")
        results = (
            None if values is None else tuple(UnsubscribeResult(v["status"], v.get("brokerReason")) for v in values)
        )
        return UnsubscribeCompletion(int(response["operationId"]), results)

    def events(self) -> AsyncIterator[MqttEvent]:
        self._bind_loop()
        if self._closing:
            raise _closed_error()
        if not self._connected and self._connect_task is None:
            raise _state_error("CLIENT_NOT_CONNECTED", "connect() must be started before consuming events")
        active = None if self._event_iterator is None else self._event_iterator()
        if active is not None:
            raise _state_error("EVENT_ITERATOR_ACTIVE", "only one event iterator may be active")
        iterator = _EventIterator(self)
        self._event_iterator = weakref.ref(iterator)
        return iterator

    async def _next_event(self) -> MqttEvent:
        self._bind_loop()
        response = _response(await self._native.next_event())
        if response["done"]:
            raise StopAsyncIteration
        return _event(self, response["event"])

    async def _acknowledge(self, ack_id: int) -> None:
        self._require_connected()
        # Manual acknowledgements use the native control lane. Keeping them out of the Python
        # request semaphore prevents saturated publish admission from blocking protocol progress.
        await self._ack_admission.acquire()
        try:
            worker = asyncio.create_task(self._complete_acknowledgement(ack_id))
        except BaseException:
            self._ack_admission.release()
            raise
        # asyncio keeps only weak references to tasks. Retain cancellation-independent workers
        # until their native completions settle.
        self._acknowledgement_workers.add(worker)
        worker.add_done_callback(self._acknowledgement_workers.discard)
        worker.add_done_callback(_observe_acknowledgement_result)
        # Caller cancellation must not cancel the worker: admitted native work cannot be
        # cancelled, and its permit remains owned by the worker until completion settles.
        await asyncio.shield(worker)

    async def _complete_acknowledgement(self, ack_id: int) -> None:
        try:
            completion = _tracked_completion(await self._native.acknowledge(ack_id))
            _response(await completion.wait())
        finally:
            self._ack_admission.release()

    async def diagnostics(self) -> ClientDiagnostics:
        self._require_connected()
        response = _response(await self._native.diagnostics())["result"]
        return ClientDiagnostics(
            response["connected"],
            response["disconnecting"],
            response["pendingRequests"],
            response["queuedRequests"],
            response["inflightPublishes"],
            response["maxInflightPublishes"],
            response["pendingSubscribes"],
            response["pendingUnsubscribes"],
            response["outboundDrained"],
        )

    async def close(self, *, timeout: float | None = None) -> None:
        self._bind_loop()
        value = 5.0 if timeout is None else _seconds(timeout, "timeout")
        self._closing = True
        timeout_ms = int(value * 1_000)
        if self._connected:
            _response(await self._native.close(timeout_ms))
        else:
            # There is no MQTT session to drain before the initial CONNACK. Immediate shutdown
            # also prevents a connection-attempt error from racing graceful-close completion.
            _response(await self._native.close_now(timeout_ms))
        self._finalizer.detach()

    async def close_now(self) -> None:
        self._bind_loop()
        self._closing = True
        _response(await self._native.close_now(5_000))
        self._finalizer.detach()

    async def __aenter__(self) -> MqttClient:
        await self.connect()
        return self

    async def __aexit__(self, exc_type: object, exc: BaseException | None, traceback: object) -> None:
        try:
            await self.close()
        except BaseException as shutdown_error:
            if exc is None:
                raise
            warnings.warn(
                f"MQTT cleanup failed while another exception was active: {shutdown_error}",
                RuntimeWarning,
                stacklevel=2,
            )
            with contextlib.suppress(BaseException):
                await self.close_now()


def _payload(payload: bytes | bytearray | memoryview | str) -> bytes:
    if isinstance(payload, str):
        return payload.encode()
    return _bytes(payload, "payload")


def _publish_payload(payload: bytes | bytearray | memoryview | str, payload_format_indicator: int | None) -> bytes:
    result = _payload(payload)
    if payload_format_indicator == 1:
        try:
            result.decode("utf-8")
        except UnicodeDecodeError as error:
            raise ValueError("payload must be well-formed UTF-8 when payload_format_indicator is 1") from error
    return result


def _json_optional(value: object | None) -> str | None:
    return None if value is None else json.dumps(value)


def _publish_options(options: PublishOptions | None, protocol: ProtocolVersion) -> _CheckedPublishOptions:
    if options is None:
        return _CheckedPublishOptions(None, False, None)
    if not isinstance(options, PublishOptions):
        raise TypeError("options must be PublishOptions")
    properties = options.properties
    qos = _qos(options.qos)
    retain = _boolean(options.retain, "retain")
    if properties is not None and not isinstance(properties, V5PublishProperties):
        raise TypeError("properties must be V5PublishProperties")
    if properties is not None and protocol is ProtocolVersion.MQTT_3_1_1:
        raise ValueError("PUBLISH properties require protocol MQTT_5_0")
    topic_alias = (
        None
        if properties is None or properties.topic_alias is None
        else _integer(properties.topic_alias, "topic_alias", 2**16 - 1, nonzero=True)
    )
    payload_format_indicator = (
        None
        if properties is None or properties.payload_format_indicator is None
        else _integer(properties.payload_format_indicator, "payload_format_indicator", 1)
    )
    encoded = (
        None
        if properties is None
        else {
            "responseTopic": None
            if properties.response_topic is None
            else _topic(properties.response_topic, "response_topic"),
            "correlationData": None
            if properties.correlation_data is None
            else list(_limited_binary(properties.correlation_data, "correlation_data")),
            "contentType": None
            if properties.content_type is None
            else _mqtt_string(properties.content_type, "content_type"),
            "payloadFormatIndicator": payload_format_indicator,
            "topicAlias": topic_alias,
            "messageExpiryInterval": None
            if properties.message_expiry_interval is None
            else _integer(properties.message_expiry_interval, "message_expiry_interval", 2**32 - 1),
            "userProperties": _user_properties(properties.user_properties, "PUBLISH user_properties"),
        }
    )
    return _CheckedPublishOptions(
        json.dumps({"qos": int(qos), "retain": retain, "properties": encoded}),
        topic_alias is not None,
        payload_format_indicator,
    )


def _subscription(value: Subscription, protocol: ProtocolVersion) -> dict[str, Any]:
    if not isinstance(value, Subscription):
        raise TypeError("subscriptions must contain Subscription values")
    checked_filter = _topic_filter(value.filter, "subscription filter")
    options = value.options
    qos = _qos(value.qos)
    if options is not None and not isinstance(options, V5SubscriptionOptions):
        raise TypeError("subscription options must be V5SubscriptionOptions")
    if options is not None and protocol is ProtocolVersion.MQTT_3_1_1:
        raise ValueError("per-filter subscription options require protocol MQTT_5_0")
    no_local = False
    retain_as_published = False
    if options is not None:
        no_local = _boolean(options.no_local, "no_local")
        retain_as_published = _boolean(options.retain_as_published, "retain_as_published")
        if no_local and checked_filter.startswith("$share/"):
            raise ValueError("no_local must be false for a shared subscription")
        if isinstance(options.retain_forward_rule, bool) or not isinstance(
            options.retain_forward_rule, RetainForwardRule
        ):
            raise TypeError("retain_forward_rule must be RetainForwardRule")
    return {
        "filter": checked_filter,
        "qos": int(qos),
        "options": None
        if options is None
        else {
            "noLocal": no_local,
            "retainAsPublished": retain_as_published,
            "retainForwardRule": int(options.retain_forward_rule),
        },
    }


def _event(client: MqttClient, value: dict[str, Any]) -> MqttEvent:
    kind = value["type"]
    if kind == "connected":
        return Connected(ProtocolVersion(value["protocol"]), value["sessionPresent"])
    if kind == "disconnected":
        return Disconnected(ConnectionPhase(value["phase"]), error_from_data(value["error"]))
    if kind == "outgoing":
        return Outgoing(OutgoingActivity(value["packet"]))
    if kind == "closed":
        return Closed(value["graceful"])
    if kind == "driverError":
        error = error_from_data(value["error"])
        if error.code == "EVENT_BUFFER_OVERFLOW":
            raise error
        return DriverError(error)
    message = value["message"]
    properties = message.get("properties")
    incoming = (
        None
        if properties is None
        else V5IncomingPublishProperties(
            properties.get("responseTopic"),
            None
            if properties.get("correlationDataBase64") is None
            else base64.b64decode(properties["correlationDataBase64"]),
            properties.get("contentType"),
            properties.get("payloadFormatIndicator"),
            properties.get("topicAlias"),
            tuple(properties["subscriptionIdentifiers"]),
            properties.get("messageExpiryInterval"),
            tuple(tuple(pair) for pair in properties["userProperties"]),
        )
    )
    ack_id = message.get("ackId")
    return IncomingPublish(
        base64.b64decode(message["topicBase64"]).decode(),
        base64.b64decode(message["payloadBase64"]),
        QoS(message["qos"]),
        message["retain"],
        message["duplicate"],
        incoming,
        None if ack_id is None else Acknowledgement(client, int(ack_id)),
    )
