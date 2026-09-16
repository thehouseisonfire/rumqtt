use crate::error::response_error;
use base64::Engine as _;
use rumqttc_wrapper_core::{
    AckToken, ConnectionPhase, OutgoingActivity, ProtocolVersion, WrapperEvent,
};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct AckRegistry {
    next: AtomicU64,
    tokens: Mutex<HashMap<u64, AckToken>>,
}
impl AckRegistry {
    pub(crate) fn insert(&self, t: AckToken) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, t);
        id
    }
    pub(crate) fn claim(&self, id: u64) -> Option<AckClaim<'_>> {
        let token = self
            .tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)?;
        Some(AckClaim {
            registry: self,
            id,
            token,
            committed: false,
        })
    }
    fn clear(&self) {
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

pub struct AckClaim<'a> {
    registry: &'a AckRegistry,
    id: u64,
    token: AckToken,
    committed: bool,
}

impl AckClaim<'_> {
    pub(crate) const fn token(&self) -> AckToken {
        self.token
    }

    pub(crate) const fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for AckClaim<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.registry
                .tokens
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(self.id, self.token);
        }
    }
}
pub fn encode(event: WrapperEvent, acks: &AckRegistry) -> String {
    let value = match event {
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
            acks.clear();
            json!({"type":"connected","protocol":match protocol{ProtocolVersion::V4=>"3.1.1",ProtocolVersion::V5=>"5.0"},"sessionPresent":session_present,"details":connack_details(details)})
        }
        WrapperEvent::Disconnected { phase, error } => {
            acks.clear();
            json!({"type":"disconnected","phase":match phase{ConnectionPhase::Attempt=>"attempt",ConnectionPhase::Established=>"established"},"error":error_value(&error),"reconnecting":true})
        }
        WrapperEvent::IncomingPublish(v) => {
            let mut m = Map::from_iter([
                (
                    "topicBase64".into(),
                    json!(base64::engine::general_purpose::STANDARD.encode(&v.topic)),
                ),
                (
                    "payloadBase64".into(),
                    json!(base64::engine::general_purpose::STANDARD.encode(&v.payload)),
                ),
                ("qos".into(), json!(v.qos as u8)),
                ("retain".into(), json!(v.retain)),
                ("duplicate".into(), json!(v.duplicate)),
            ]);
            if let Some(t) = v.ack_token {
                m.insert("ackId".into(), json!(acks.insert(t).to_string()));
            }
            if let Some(p) = v.v5_properties {
                m.insert("properties".into(),json!({"responseTopic":p.response_topic,"correlationDataBase64":p.correlation_data.map(|x|base64::engine::general_purpose::STANDARD.encode(x)),"contentType":p.content_type,"payloadFormatIndicator":p.payload_format_indicator,"topicAlias":p.topic_alias,"subscriptionIdentifiers":p.subscription_identifiers,"messageExpiryInterval":p.message_expiry_interval,"userProperties":p.user_properties}));
            }
            json!({"type":"publish","message":m})
        }
        WrapperEvent::Outgoing(v) => {
            json!({"type":"outgoing","packetId":v.packet_id,"packet":match v.activity{OutgoingActivity::Publish=>"publish",OutgoingActivity::Subscribe=>"subscribe",OutgoingActivity::Unsubscribe=>"unsubscribe",OutgoingActivity::Acknowledgement=>"acknowledgement",OutgoingActivity::Ping=>"ping",OutgoingActivity::Disconnect=>"disconnect",OutgoingActivity::AwaitAcknowledgement=>"awaitAcknowledgement",OutgoingActivity::Other=>"other"}})
        }
        WrapperEvent::GracefulShutdownCompleted => {
            acks.clear();
            json!({"type":"closed","graceful":true})
        }
        WrapperEvent::ImmediateShutdownCompleted => {
            acks.clear();
            json!({"type":"closed","graceful":false})
        }
        WrapperEvent::DriverTerminated(e) => {
            acks.clear();
            json!({"type":"driverError","error":error_value(&e)})
        }
    };
    value.to_string()
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
fn error_value(e: &rumqttc_wrapper_core::Error) -> Value {
    serde_json::from_str::<Value>(&response_error(e,None)).ok().and_then(|v|v.get("error").cloned()).unwrap_or_else(||json!({"code":"INTERNAL_PANIC","kind":"internal","message":"event conversion failed","retryable":false,"delivery":"notApplicable","ambiguous":false}))
}
