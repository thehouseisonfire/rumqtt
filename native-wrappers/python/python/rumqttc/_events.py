from __future__ import annotations

from dataclasses import dataclass
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
class Connected:
    protocol: ProtocolVersion
    session_present: bool


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


@dataclass(frozen=True, slots=True)
class Closed:
    graceful: bool


@dataclass(frozen=True, slots=True)
class DriverError:
    error: MqttError


MqttEvent: TypeAlias = Connected | Disconnected | IncomingPublish | Outgoing | Closed | DriverError
