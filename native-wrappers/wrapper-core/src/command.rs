use std::time::Duration;

use bytes::Bytes;

use crate::{AckToken, QoS};

/// MQTT 5 properties legal on a client-originated PUBLISH packet.
///
/// Subscription Identifiers are intentionally absent because MQTT clients cannot send them in a
/// PUBLISH packet.
///
/// ```compile_fail
/// use rumqttc_wrapper_core::V5OutgoingPublishProperties;
///
/// let _ = V5OutgoingPublishProperties {
///     subscription_identifiers: vec![7],
///     ..V5OutgoingPublishProperties::default()
/// };
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct V5OutgoingPublishProperties {
    pub response_topic: Option<String>,
    pub correlation_data: Option<Bytes>,
    pub content_type: Option<String>,
    pub payload_format_indicator: Option<u8>,
    pub topic_alias: Option<u16>,
    pub message_expiry_interval: Option<u32>,
    pub user_properties: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishCommand {
    pub topic: String,
    pub payload: Bytes,
    pub qos: QoS,
    pub retain: bool,
    pub protocol: PublishProtocolOptions,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PublishProtocolOptions {
    #[default]
    VersionNeutral,
    V5(V5OutgoingPublishProperties),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subscription {
    pub filter: String,
    pub qos: QoS,
    pub protocol: SubscriptionProtocolOptions,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SubscriptionProtocolOptions {
    #[default]
    VersionNeutral,
    V5(V5SubscriptionOptions),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct V5SubscriptionOptions {
    pub no_local: bool,
    pub retain_as_published: bool,
    pub retain_forward_rule: V5RetainForwardRule,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum V5RetainForwardRule {
    #[default]
    OnEverySubscribe,
    OnNewSubscribe,
    Never,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscribeCommand {
    pub filters: Vec<Subscription>,
    pub protocol: SubscribeProtocolOptions,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SubscribeProtocolOptions {
    #[default]
    VersionNeutral,
    V5(V5SubscribeProperties),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct V5SubscribeProperties {
    pub subscription_identifier: Option<usize>,
    pub user_properties: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsubscribeCommand {
    pub filters: Vec<String>,
    pub protocol: UnsubscribeProtocolOptions,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UnsubscribeProtocolOptions {
    #[default]
    VersionNeutral,
    V5(V5UnsubscribeProperties),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct V5UnsubscribeProperties {
    pub user_properties: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Publish(PublishCommand),
    Subscribe(SubscribeCommand),
    Unsubscribe(UnsubscribeCommand),
    Acknowledge(AckToken),
    GracefulDisconnect { timeout: Option<Duration> },
    ImmediateDisconnect,
    Diagnostics,
}
