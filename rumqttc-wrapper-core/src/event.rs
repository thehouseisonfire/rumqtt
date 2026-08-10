use bytes::Bytes;

use crate::{Error, ProtocolVersion, QoS};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AckToken {
    pub(crate) client: u64,
    pub(crate) generation: u64,
    pub(crate) serial: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct V5PublishProperties {
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
    pub v5_properties: Option<V5PublishProperties>,
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
    Connected {
        protocol: ProtocolVersion,
        session_present: bool,
    },
    Disconnected {
        phase: ConnectionPhase,
        error: Error,
    },
    IncomingPublish(Box<IncomingPublish>),
    Outgoing(OutgoingActivity),
    GracefulShutdownCompleted,
    DriverTerminated(Error),
}
