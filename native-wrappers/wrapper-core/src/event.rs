use bytes::Bytes;

use crate::{Error, ProtocolVersion, QoS};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AckToken {
    pub(crate) client: u64,
    pub(crate) generation: u64,
    pub(crate) serial: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
/// MQTT 5 properties observed on a broker-originated PUBLISH packet.
pub struct V5IncomingPublishProperties {
    pub response_topic: Option<String>,
    pub correlation_data: Option<Bytes>,
    pub content_type: Option<String>,
    pub payload_format_indicator: Option<u8>,
    pub topic_alias: Option<u16>,
    pub subscription_identifiers: Vec<usize>,
    pub message_expiry_interval: Option<u32>,
    pub user_properties: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingPublish {
    pub topic: Bytes,
    pub payload: Bytes,
    pub qos: QoS,
    pub retain: bool,
    pub duplicate: bool,
    pub ack_token: Option<AckToken>,
    pub v5_properties: Option<V5IncomingPublishProperties>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionPhase {
    Attempt,
    Established,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutgoingActivity {
    Publish,
    Subscribe,
    Unsubscribe,
    Acknowledgement,
    Ping,
    Disconnect,
    AwaitAcknowledgement,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutgoingEvent {
    pub activity: OutgoingActivity,
    /// Zero is represented as None (`QoS` 0 has no packet identifier).
    pub packet_id: Option<u16>,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct V5ConnAckProperties {
    pub session_expiry_interval: Option<u32>,
    pub receive_maximum: Option<u16>,
    pub maximum_qos: Option<u8>,
    pub retain_available: Option<u8>,
    pub maximum_packet_size: Option<u32>,
    pub assigned_client_identifier: Option<String>,
    pub topic_alias_maximum: Option<u16>,
    pub reason_string: Option<String>,
    pub wildcard_subscription_available: Option<u8>,
    pub subscription_identifiers_available: Option<u8>,
    pub shared_subscription_available: Option<u8>,
    pub server_keep_alive: Option<u16>,
    pub response_information: Option<String>,
    pub server_reference: Option<String>,
    pub authentication_method: Option<String>,
    pub authentication_data: Option<Bytes>,
    pub user_properties: Vec<(String, String)>,
}

impl std::fmt::Debug for V5ConnAckProperties {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("V5ConnAckProperties")
            .field("session_expiry_interval", &self.session_expiry_interval)
            .field("receive_maximum", &self.receive_maximum)
            .field("maximum_packet_size", &self.maximum_packet_size)
            .field(
                "authentication_data",
                &self.authentication_data.as_ref().map(|_| "[REDACTED]"),
            )
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnAckDetails {
    pub reason_code: u8,
    pub v5_properties: Option<Box<V5ConnAckProperties>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagnosticsSnapshot {
    pub connected: bool,
    pub disconnecting: bool,
    pub pending_requests: usize,
    pub queued_requests: usize,
    pub inflight_publishes: u16,
    pub max_inflight_publishes: u16,
    pub pending_subscribes: usize,
    pub pending_unsubscribes: usize,
    pub outbound_drained: bool,
}

#[derive(Clone, Debug)]
pub enum WrapperEvent {
    /// A negative CONNACK. Connection recovery is reported separately.
    ConnectionRejected(ConnAckDetails),
    Authentication(crate::AuthEvent),
    Redirect(crate::RedirectEvent),
    BrokerDisconnect(crate::V5DisconnectOptions),
    Connected {
        protocol: ProtocolVersion,
        session_present: bool,
        details: ConnAckDetails,
    },
    Disconnected {
        phase: ConnectionPhase,
        error: Error,
    },
    IncomingPublish(Box<IncomingPublish>),
    Outgoing(OutgoingEvent),
    GracefulShutdownCompleted,
    ImmediateShutdownCompleted,
    DriverTerminated(Error),
}
