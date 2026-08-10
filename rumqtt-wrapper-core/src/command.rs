use std::time::Duration;

use bytes::Bytes;

use crate::{AckToken, QoS, V5PublishProperties};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishCommand {
    pub topic: String,
    pub payload: Bytes,
    pub qos: QoS,
    pub retain: bool,
    pub v5_properties: Option<V5PublishProperties>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subscription {
    pub filter: String,
    pub qos: QoS,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscribeCommand {
    pub filters: Vec<Subscription>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Publish(PublishCommand),
    Subscribe(SubscribeCommand),
    Unsubscribe(Vec<String>),
    Acknowledge(AckToken),
    GracefulDisconnect { timeout: Option<Duration> },
    ImmediateDisconnect,
    Diagnostics,
}
