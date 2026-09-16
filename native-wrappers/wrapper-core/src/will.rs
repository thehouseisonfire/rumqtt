use bytes::Bytes;

use crate::config::validate_mqtt_utf8_string;
use crate::{Error, ProtocolVersion, QoS, Result};

/// Owned Last Will, fixed before the first connection attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastWillConfig {
    pub topic: String,
    pub payload: Bytes,
    pub qos: QoS,
    pub retain: bool,
    pub protocol: LastWillProtocolOptions,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LastWillProtocolOptions {
    #[default]
    VersionNeutral,
    V5(V5WillProperties),
}

/// Will properties are distinct from outgoing PUBLISH properties. Singleton
/// properties occur at most once by construction; user properties retain order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct V5WillProperties {
    pub will_delay_interval: Option<u32>,
    pub payload_format_indicator: Option<u8>,
    pub message_expiry_interval: Option<u32>,
    pub content_type: Option<String>,
    pub response_topic: Option<String>,
    pub correlation_data: Option<Bytes>,
    pub user_properties: Vec<(String, String)>,
}

impl LastWillConfig {
    pub(crate) fn validate(&self, protocol: ProtocolVersion) -> Result<()> {
        validate_topic(&self.topic, "will topic")?;
        validate_binary(&self.payload, "will payload")?;
        if let LastWillProtocolOptions::V5(properties) = &self.protocol {
            if protocol != ProtocolVersion::V5 {
                return Err(Error::configuration(
                    "MQTT 5 will properties require MQTT 5",
                ));
            }
            match properties.payload_format_indicator {
                None | Some(0) => {}
                Some(1) if std::str::from_utf8(&self.payload).is_ok() => {}
                Some(_) => {
                    return Err(Error::configuration(
                        "invalid will payload format or UTF-8 payload",
                    ));
                }
            }
            if let Some(value) = &properties.content_type {
                validate_mqtt_utf8_string(value, "will content type")?;
            }
            if let Some(value) = &properties.response_topic {
                validate_topic(value, "will response topic")?;
            }
            if let Some(value) = &properties.correlation_data {
                validate_binary(value, "will correlation data")?;
            }
            validate_user_properties(&properties.user_properties)?;
        }
        Ok(())
    }
}

pub fn validate_topic(value: &str, name: &str) -> Result<()> {
    validate_mqtt_utf8_string(value, name)?;
    if value.is_empty() || value.contains(['+', '#']) {
        return Err(Error::configuration(format!(
            "{name} must be nonempty and contain no wildcards"
        )));
    }
    Ok(())
}

pub fn validate_binary(value: &[u8], name: &str) -> Result<()> {
    if value.len() > usize::from(u16::MAX) {
        return Err(Error::configuration(format!("{name} exceeds 65535 bytes")));
    }
    Ok(())
}

pub fn validate_user_properties(values: &[(String, String)]) -> Result<()> {
    for (key, value) in values {
        validate_mqtt_utf8_string(key, "user property key")?;
        validate_mqtt_utf8_string(value, "user property value")?;
    }
    Ok(())
}
