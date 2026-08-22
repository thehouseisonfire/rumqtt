from __future__ import annotations

from enum import Enum
from typing import Any


class ErrorKind(str, Enum):
    CONFIGURATION = "configuration"
    ADMISSION = "admission"
    BACKPRESSURE = "backpressure"
    NETWORK = "network"
    TLS = "tls"
    PROTOCOL = "protocol"
    AUTHENTICATION = "authentication"
    PERSISTENCE = "persistence"
    TIMEOUT = "timeout"
    SHUTDOWN = "shutdown"
    INTERNAL = "internal"


class DeliveryStatus(str, Enum):
    NOT_APPLICABLE = "notApplicable"
    NOT_ADMITTED = "notAdmitted"
    REJECTED = "rejected"
    AMBIGUOUS = "ambiguous"


class MqttError(Exception):
    def __init__(
        self,
        message: str,
        *,
        code: str,
        kind: ErrorKind,
        operation_id: int | None = None,
        broker_reason: int | None = None,
        retryable: bool | None = None,
        delivery: DeliveryStatus = DeliveryStatus.NOT_APPLICABLE,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.kind = kind
        self.operation_id = operation_id
        self.broker_reason = broker_reason
        self.retryable = retryable
        self.delivery = delivery
        self.ambiguous = delivery is DeliveryStatus.AMBIGUOUS


class ConfigurationError(MqttError):
    pass


class BackpressureError(MqttError):
    pass


class ProtocolError(MqttError):
    pass


class BrokerRejectedError(MqttError):
    pass


class ClientClosedError(MqttError):
    pass


class ClientStateError(MqttError):
    pass


def error_from_data(data: dict[str, Any]) -> MqttError:
    code = str(data.get("code", "INTERNAL"))
    kind = ErrorKind(data.get("kind", "internal"))
    error_type: type[MqttError]
    if code == "BROKER_REJECTED":
        error_type = BrokerRejectedError
    elif kind is ErrorKind.CONFIGURATION:
        error_type = ConfigurationError
    elif kind is ErrorKind.BACKPRESSURE:
        error_type = BackpressureError
    elif kind is ErrorKind.PROTOCOL:
        error_type = ProtocolError
    elif kind is ErrorKind.SHUTDOWN:
        error_type = ClientClosedError
    elif code in {"CLIENT_NOT_CONNECTED", "WRONG_EVENT_LOOP", "EVENT_ITERATOR_ACTIVE", "ACKNOWLEDGEMENT_CONSUMED"}:
        error_type = ClientStateError
    else:
        error_type = MqttError
    operation_id = data.get("operationId")
    return error_type(
        str(data.get("message", code)),
        code=code,
        kind=kind,
        operation_id=int(operation_id) if operation_id is not None else None,
        broker_reason=data.get("brokerReason"),
        retryable=data.get("retryable"),
        delivery=DeliveryStatus(data.get("delivery", "notApplicable")),
    )
