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
        WrapperEvent::Connected {
            protocol,
            session_present,
        } => {
            acks.clear();
            json!({"type":"connected","protocol":match protocol{ProtocolVersion::V4=>"3.1.1",ProtocolVersion::V5=>"5.0"},"sessionPresent":session_present})
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
            json!({"type":"outgoing","packet":match v{OutgoingActivity::Publish=>"publish",OutgoingActivity::Subscribe=>"subscribe",OutgoingActivity::Unsubscribe=>"unsubscribe",OutgoingActivity::Acknowledgement=>"acknowledgement",OutgoingActivity::Ping=>"ping",OutgoingActivity::Disconnect=>"disconnect",OutgoingActivity::AwaitAcknowledgement=>"awaitAcknowledgement",OutgoingActivity::Other=>"other"}})
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
fn error_value(e: &rumqttc_wrapper_core::Error) -> Value {
    serde_json::from_str::<Value>(&response_error(e,None)).ok().and_then(|v|v.get("error").cloned()).unwrap_or_else(||json!({"code":"INTERNAL_PANIC","kind":"internal","message":"event conversion failed","retryable":false,"delivery":"notApplicable","ambiguous":false}))
}
