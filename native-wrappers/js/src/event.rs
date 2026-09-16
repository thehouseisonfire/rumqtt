use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use rumqttc_wrapper_core::{
    AckToken, ConnectionPhase, OutgoingActivity, ProtocolVersion, WrapperEvent,
};
use serde_json::{Map, Value, json};

use crate::error::response_error;

#[derive(Default)]
pub struct AckRegistry {
    next: AtomicU64,
    tokens: Mutex<HashMap<u64, AckToken>>,
}

impl AckRegistry {
    pub(crate) fn insert(&self, token: AckToken) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, token);
        id
    }

    pub(crate) fn take(&self, id: u64) -> Option<AckToken> {
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)
    }

    fn clear(&self) {
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

pub fn encode(event: WrapperEvent, acknowledgements: &AckRegistry) -> String {
    let event = match event {
        WrapperEvent::ConnectionRejected(details) => {
            json!({ "type": "connectionRejected", "details": connack_details(details) })
        }
        WrapperEvent::BrokerDisconnect(event) => json!({
            "type": "brokerDisconnect", "reasonCode": event.reason_code,
            "sessionExpiryInterval": event.session_expiry_interval,
            "reasonString": event.reason_string, "userProperties": event.user_properties,
            "serverReference": event.server_reference,
        }),
        WrapperEvent::Redirect(event) => json!({
            "type": "redirect", "serverReference": event.server_reference,
            "source": if event.source == rumqttc_wrapper_core::RedirectSource::ConnAck { "connack" } else { "disconnect" },
            "reason": if event.reason == rumqttc_wrapper_core::RedirectReason::UseAnotherServer { 0x9c } else { 0x9d },
            "target": event.target.map(|target| match target {
                rumqttc_wrapper_core::BrokerTarget::Tcp { host, port } => json!({"type":"tcp", "host":host, "port":port}),
                rumqttc_wrapper_core::BrokerTarget::WebSocket { url } => json!({"type":"websocket", "url":url}),
                rumqttc_wrapper_core::BrokerTarget::Unix { path } => json!({"type":"unix", "path":path}),
            }),
            "failure": event.failure.map(|failure| format!("{failure:?}")),
        }),
        WrapperEvent::Authentication(event) => json!({
            "type": "authentication", "method": event.method,
            "exchange": if event.exchange == rumqttc_wrapper_core::AuthExchange::Initial { "initial" } else { "reauthentication" },
            "stage": event.stage.as_str(), "failure": event.failure.map(|failure| failure.to_string()),
        }),
        WrapperEvent::Connected {
            protocol,
            session_present,
            details,
        } => {
            acknowledgements.clear();
            json!({
                "type": "connected",
                "protocol": protocol_name(protocol),
                "sessionPresent": session_present,
                "details": connack_details(details),
            })
        }
        WrapperEvent::Disconnected { phase, error } => {
            acknowledgements.clear();
            json!({
                "type": "disconnected",
                "phase": match phase { ConnectionPhase::Attempt => "attempt", ConnectionPhase::Established => "established" },
                "error": error_value(&error),
                "reconnecting": true,
            })
        }
        WrapperEvent::IncomingPublish(publish) => {
            let mut message = Map::from_iter([
                (
                    "topicBase64".to_owned(),
                    json!(base64::engine::general_purpose::STANDARD.encode(&publish.topic)),
                ),
                (
                    "payloadBase64".to_owned(),
                    json!(base64::engine::general_purpose::STANDARD.encode(&publish.payload)),
                ),
                ("qos".to_owned(), json!(publish.qos as u8)),
                ("retain".to_owned(), json!(publish.retain)),
                ("duplicate".to_owned(), json!(publish.duplicate)),
            ]);
            if let Some(token) = publish.ack_token {
                message.insert(
                    "ackId".to_owned(),
                    json!(acknowledgements.insert(token).to_string()),
                );
            }
            if let Some(properties) = publish.v5_properties {
                let mut value = Map::from_iter([
                    (
                        "subscriptionIdentifiers".to_owned(),
                        json!(properties.subscription_identifiers),
                    ),
                    (
                        "userProperties".to_owned(),
                        json!(properties.user_properties),
                    ),
                ]);
                insert_optional(
                    &mut value,
                    "responseTopic",
                    properties.response_topic.map(Value::String),
                );
                insert_optional(
                    &mut value,
                    "correlationDataBase64",
                    properties.correlation_data.map(|data| {
                        Value::String(base64::engine::general_purpose::STANDARD.encode(data))
                    }),
                );
                insert_optional(
                    &mut value,
                    "contentType",
                    properties.content_type.map(Value::String),
                );
                insert_optional(
                    &mut value,
                    "payloadFormatIndicator",
                    properties.payload_format_indicator.map(Value::from),
                );
                insert_optional(
                    &mut value,
                    "topicAlias",
                    properties.topic_alias.map(Value::from),
                );
                insert_optional(
                    &mut value,
                    "messageExpiryInterval",
                    properties.message_expiry_interval.map(Value::from),
                );
                message.insert("properties".to_owned(), Value::Object(value));
            }
            json!({ "type": "publish", "message": message })
        }
        WrapperEvent::Outgoing(activity) => {
            json!({ "type": "outgoing", "packet": outgoing(activity.activity), "packetId": activity.packet_id })
        }
        WrapperEvent::GracefulShutdownCompleted => {
            acknowledgements.clear();
            json!({ "type": "closed", "graceful": true })
        }
        WrapperEvent::ImmediateShutdownCompleted => {
            acknowledgements.clear();
            json!({ "type": "closed", "graceful": false })
        }
        WrapperEvent::DriverTerminated(error) => {
            acknowledgements.clear();
            json!({ "type": "driverError", "error": error_value(&error) })
        }
    };
    event.to_string()
}

fn connack_details(details: rumqttc_wrapper_core::ConnAckDetails) -> Value {
    json!({
        "reasonCode": details.reason_code,
        "properties": details.v5_properties.map(|p| json!({
            "sessionExpiryInterval": p.session_expiry_interval,
            "receiveMaximum": p.receive_maximum,
            "maximumQos": p.maximum_qos,
            "retainAvailable": p.retain_available,
            "maximumPacketSize": p.maximum_packet_size,
            "assignedClientIdentifier": p.assigned_client_identifier,
            "topicAliasMaximum": p.topic_alias_maximum,
            "reasonString": p.reason_string,
            "wildcardSubscriptionAvailable": p.wildcard_subscription_available,
            "subscriptionIdentifiersAvailable": p.subscription_identifiers_available,
            "sharedSubscriptionAvailable": p.shared_subscription_available,
            "serverKeepAlive": p.server_keep_alive,
            "responseInformation": p.response_information,
            "serverReference": p.server_reference,
            "authenticationMethod": p.authentication_method,
            "authenticationDataBase64": p.authentication_data.map(|value| base64::engine::general_purpose::STANDARD.encode(value)),
            "userProperties": p.user_properties,
        })),
    })
}

fn insert_optional(object: &mut Map<String, Value>, name: &str, value: Option<Value>) {
    if let Some(value) = value {
        object.insert(name.to_owned(), value);
    }
}

fn error_value(error: &rumqttc_wrapper_core::Error) -> Value {
    serde_json::from_str::<Value>(&response_error(error, None))
        .ok()
        .and_then(|value| value.get("error").cloned())
        .unwrap_or_else(|| {
            json!({
                "code": "INTERNAL_PANIC",
                "kind": "internal",
                "message": "event error conversion failed",
                "retryable": false,
                "delivery": "notApplicable",
                "ambiguous": false,
            })
        })
}

const fn protocol_name(protocol: ProtocolVersion) -> &'static str {
    match protocol {
        ProtocolVersion::V4 => "3.1.1",
        ProtocolVersion::V5 => "5.0",
    }
}

const fn outgoing(activity: OutgoingActivity) -> &'static str {
    match activity {
        OutgoingActivity::Publish => "publish",
        OutgoingActivity::Subscribe => "subscribe",
        OutgoingActivity::Unsubscribe => "unsubscribe",
        OutgoingActivity::Acknowledgement => "acknowledgement",
        OutgoingActivity::Ping => "ping",
        OutgoingActivity::Disconnect => "disconnect",
        OutgoingActivity::AwaitAcknowledgement => "awaitAcknowledgement",
        OutgoingActivity::Other => "other",
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use rumqttc_wrapper_core::{IncomingPublish, QoS, V5IncomingPublishProperties};

    use super::*;

    fn publish(properties: Option<V5IncomingPublishProperties>) -> WrapperEvent {
        WrapperEvent::IncomingPublish(Box::new(IncomingPublish {
            topic: Bytes::from_static(b"topic"),
            payload: Bytes::new(),
            qos: QoS::AtMostOnce,
            retain: false,
            duplicate: false,
            ack_token: None,
            v5_properties: properties,
        }))
    }

    #[test]
    fn v4_publish_omits_mqtt5_properties() {
        let value: Value = serde_json::from_str(&encode(publish(None), &AckRegistry::default()))
            .expect("event JSON");
        assert!(value["message"].get("properties").is_none());
        assert!(value["message"].get("ackId").is_none());
    }

    #[test]
    fn v5_publish_omits_absent_optional_properties() {
        let value: Value = serde_json::from_str(&encode(
            publish(Some(V5IncomingPublishProperties::default())),
            &AckRegistry::default(),
        ))
        .expect("event JSON");
        let properties = value["message"]["properties"]
            .as_object()
            .expect("MQTT 5 properties object");
        assert!(!properties.contains_key("contentType"));
        assert!(!properties.contains_key("correlationDataBase64"));
        assert_eq!(properties["subscriptionIdentifiers"], json!([]));
        assert_eq!(properties["userProperties"], json!([]));
    }
}
