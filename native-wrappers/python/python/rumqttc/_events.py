from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, TypeAlias

from ._errors import MqttError
from ._types import (
    ConnectionPhase,
    OutgoingActivity,
    ProtocolVersion,
    QoS,
    V5IncomingPublishProperties,
)

if TYPE_CHECKING:
    from ._client import MqttClient


class Acknowledgement:
    __slots__ = ("_ack_id", "_client")

    def __init__(self, client: MqttClient, ack_id: int) -> None:
        self._client = client
        self._ack_id = ack_id

    async def ack(self) -> None:
        await self._client._acknowledge(self._ack_id)


@dataclass(frozen=True, slots=True)
class V5ConnAckProperties:
    session_expiry_interval: int | None = None
    receive_maximum: int | None = None
    maximum_qos: int | None = None
    retain_available: int | None = None
    maximum_packet_size: int | None = None
    assigned_client_identifier: str | None = None
    topic_alias_maximum: int | None = None
    reason_string: str | None = None
    wildcard_subscription_available: int | None = None
    subscription_identifiers_available: int | None = None
    shared_subscription_available: int | None = None
    server_keep_alive: int | None = None
    response_information: str | None = None
    server_reference: str | None = None
    authentication_method: str | None = None
    authentication_data: bytes | None = field(default=None, repr=False)
    user_properties: tuple[tuple[str, str], ...] = ()


@dataclass(frozen=True, slots=True)
class ConnAckDetails:
    reason_code: int
    v5_properties: V5ConnAckProperties | None = None


@dataclass(frozen=True, slots=True)
class Connected:
    protocol: ProtocolVersion
    session_present: bool
    details: ConnAckDetails | None = None


@dataclass(frozen=True, slots=True)
class ConnectionRejected:
    details: ConnAckDetails


@dataclass(frozen=True, slots=True)
class BrokerDisconnect:
    reason_code: int
    session_expiry_interval: int | None = None
    reason_string: str | None = None
    user_properties: tuple[tuple[str, str], ...] = ()
    server_reference: str | None = None


@dataclass(frozen=True, slots=True)
class Authentication:
    method: str
    exchange: str
    stage: str
    failure: str | None = None


@dataclass(frozen=True, slots=True)
class RedirectTarget:
    type: str
    host: str | None = None
    port: int | None = None
    url: str | None = None
    path: str | None = None


@dataclass(frozen=True, slots=True)
class Redirect:
    source: str
    reason: int
    server_reference: str | None
    target: RedirectTarget | None
    failure: str | None


@dataclass(frozen=True, slots=True)
class Disconnected:
    phase: ConnectionPhase
    error: MqttError
    reconnecting: bool = True


@dataclass(frozen=True, slots=True)
class IncomingPublish:
    topic: str
    payload: bytes
    qos: QoS
    retain: bool
    duplicate: bool
    properties: V5IncomingPublishProperties | None
    acknowledgement: Acknowledgement | None


@dataclass(frozen=True, slots=True)
class Outgoing:
    activity: OutgoingActivity
    packet_id: int | None = None


@dataclass(frozen=True, slots=True)
class Closed:
    graceful: bool


@dataclass(frozen=True, slots=True)
class DriverError:
    error: MqttError


MqttEvent: TypeAlias = (
    Connected
    | ConnectionRejected
    | BrokerDisconnect
    | Authentication
    | Redirect
    | Disconnected
    | IncomingPublish
    | Outgoing
    | Closed
    | DriverError
)
