from __future__ import annotations

from typing import cast

from rumqttc import Authentication, BrokerDisconnect, ConnectionRejected, MqttClient, Outgoing, Redirect
from rumqttc._client import _event


def test_connack_binary_and_ordered_properties_are_owned_and_redacted() -> None:
    wire = {
        "type": "connectionRejected",
        "details": {
            "reasonCode": 135,
            "properties": {
                "authenticationDataBase64": "AP8=",
                "reasonString": "",
                "userProperties": [["key", "one"], ["key", ""]],
                "receiveMaximum": 19,
            },
        },
    }
    event = _event(cast(MqttClient, None), wire)
    assert isinstance(event, ConnectionRejected)
    assert event.details.reason_code == 135
    properties = event.details.v5_properties
    assert properties is not None
    assert properties.authentication_data == b"\x00\xff"
    assert properties.reason_string == ""
    assert properties.user_properties == (("key", "one"), ("key", ""))
    assert properties.receive_maximum == 19
    assert "\\xff" not in repr(event)


def test_supplemental_events_decode_without_publish_fallback() -> None:
    client = cast(MqttClient, None)
    disconnect = _event(client, {"type": "brokerDisconnect", "reasonCode": 137, "userProperties": [["k", ""]]})
    assert isinstance(disconnect, BrokerDisconnect)
    assert disconnect.user_properties == (("k", ""),)
    auth = _event(client, {"type": "authentication", "method": "test", "exchange": "initial", "stage": "started"})
    assert isinstance(auth, Authentication)
    redirect = _event(
        client,
        {
            "type": "redirect",
            "source": "connAck",
            "reason": "serverMoved",
            "target": {"type": "tcp", "host": "localhost", "port": 1883},
        },
    )
    assert isinstance(redirect, Redirect)
    assert redirect.target is not None
    assert redirect.target.host == "localhost"
    outgoing = _event(client, {"type": "outgoing", "packet": "publish", "packetId": 42})
    assert isinstance(outgoing, Outgoing)
    assert outgoing.packet_id == 42
