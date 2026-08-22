from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum, IntEnum
from typing import TypeAlias


class ProtocolVersion(str, Enum):
    MQTT_3_1_1 = "3.1.1"
    MQTT_5_0 = "5.0"


class QoS(IntEnum):
    AT_MOST_ONCE = 0
    AT_LEAST_ONCE = 1
    EXACTLY_ONCE = 2


class AckMode(str, Enum):
    AUTOMATIC = "automatic"
    MANUAL = "manual"


class ConnectionPhase(str, Enum):
    ATTEMPT = "attempt"
    ESTABLISHED = "established"


class OutgoingActivity(str, Enum):
    PUBLISH = "publish"
    SUBSCRIBE = "subscribe"
    UNSUBSCRIBE = "unsubscribe"
    ACKNOWLEDGEMENT = "acknowledgement"
    PING = "ping"
    DISCONNECT = "disconnect"
    AWAIT_ACKNOWLEDGEMENT = "awaitAcknowledgement"
    OTHER = "other"


class PublishMilestone(str, Enum):
    QOS0_FLUSHED = "qos0Flushed"
    QOS1_ACKNOWLEDGED = "qos1Acknowledged"
    QOS2_COMPLETED = "qos2Completed"


class RetainForwardRule(IntEnum):
    ON_EVERY_SUBSCRIBE = 0
    ON_NEW_SUBSCRIBE = 1
    NEVER = 2


@dataclass(frozen=True, slots=True)
class TcpTransport:
    pass


@dataclass(frozen=True, slots=True)
class TlsOptions:
    ca: bytes | bytearray | memoryview | None = None
    client_certificate: bytes | bytearray | memoryview | None = None
    private_key: bytes | bytearray | memoryview | None = None


@dataclass(frozen=True, slots=True)
class TlsTransport:
    tls: TlsOptions = field(default_factory=TlsOptions)


@dataclass(frozen=True, slots=True)
class WebSocketTransport:
    url: str


@dataclass(frozen=True, slots=True)
class WssTransport:
    url: str
    tls: TlsOptions = field(default_factory=TlsOptions)


Transport: TypeAlias = TcpTransport | TlsTransport | WebSocketTransport | WssTransport


@dataclass(frozen=True, slots=True)
class MqttClientOptions:
    protocol: ProtocolVersion
    broker_host: str
    broker_port: int
    client_id: str
    transport: Transport = field(default_factory=TcpTransport)
    keep_alive: float = 60.0
    connection_timeout: float = 5.0
    username: str | None = None
    password: bytes | bytearray | memoryview | None = None
    request_capacity: int = 10
    event_capacity: int = 256
    event_delivery_timeout: float = 5.0
    ack_mode: AckMode = AckMode.AUTOMATIC
    incoming_packet_size_limit: int = 10 * 1024
    emit_outgoing_events: bool = False
    clean_session: bool | None = None
    clean_start: bool | None = None
    session_expiry_interval: int | None = None


@dataclass(frozen=True, slots=True)
class V5PublishProperties:
    response_topic: str | None = None
    correlation_data: bytes | bytearray | memoryview | None = None
    content_type: str | None = None
    payload_format_indicator: int | None = None
    topic_alias: int | None = None
    message_expiry_interval: int | None = None
    user_properties: tuple[tuple[str, str], ...] = ()


@dataclass(frozen=True, slots=True)
class PublishOptions:
    qos: QoS = QoS.AT_MOST_ONCE
    retain: bool = False
    properties: V5PublishProperties | None = None


@dataclass(frozen=True, slots=True)
class V5SubscriptionOptions:
    no_local: bool = False
    retain_as_published: bool = False
    retain_forward_rule: RetainForwardRule = RetainForwardRule.ON_EVERY_SUBSCRIBE


@dataclass(frozen=True, slots=True)
class Subscription:
    filter: str
    qos: QoS = QoS.AT_MOST_ONCE
    options: V5SubscriptionOptions | None = None


@dataclass(frozen=True, slots=True)
class SubscribeOptions:
    subscription_identifier: int | None = None
    user_properties: tuple[tuple[str, str], ...] = ()


@dataclass(frozen=True, slots=True)
class UnsubscribeOptions:
    user_properties: tuple[tuple[str, str], ...] = ()


@dataclass(frozen=True, slots=True)
class ConnectResult:
    protocol: ProtocolVersion
    session_present: bool


@dataclass(frozen=True, slots=True)
class AdmissionResult:
    operation_id: int


@dataclass(frozen=True, slots=True)
class PublishCompletion(AdmissionResult):
    milestone: PublishMilestone


@dataclass(frozen=True, slots=True)
class SubscribeResult:
    granted: bool
    qos: QoS | None = None
    broker_reason: int | None = None


@dataclass(frozen=True, slots=True)
class SubscribeCompletion(AdmissionResult):
    results: tuple[SubscribeResult, ...]


@dataclass(frozen=True, slots=True)
class UnsubscribeResult:
    status: str
    broker_reason: int | None = None


@dataclass(frozen=True, slots=True)
class UnsubscribeCompletion(AdmissionResult):
    results: tuple[UnsubscribeResult, ...] | None


@dataclass(frozen=True, slots=True)
class ClientDiagnostics:
    connected: bool
    disconnecting: bool
    pending_requests: int
    queued_requests: int
    inflight_publishes: int
    max_inflight_publishes: int
    pending_subscribes: int
    pending_unsubscribes: int
    outbound_drained: bool


@dataclass(frozen=True, slots=True)
class V5IncomingPublishProperties:
    response_topic: str | None
    correlation_data: bytes | None
    content_type: str | None
    payload_format_indicator: int | None
    topic_alias: int | None
    subscription_identifiers: tuple[int, ...]
    message_expiry_interval: int | None
    user_properties: tuple[tuple[str, str], ...]
