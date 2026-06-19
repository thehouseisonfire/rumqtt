use std::error::Error;
use std::future::Future;
use std::pin::Pin;

/// Error returned by a [`SessionStore`] backend.
pub type SessionStoreError = Box<dyn Error + Send + Sync>;

/// Boxed async result used by [`SessionStore`] methods.
pub type SessionStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SessionStoreError>> + Send + 'a>>;

/// Durable storage for MQTT 5 client session state.
///
/// Implementations must make `save` crash-consistent. A later `load` should
/// return either the last complete checkpoint or no checkpoint, never a
/// partially written session. The crate intentionally does not prescribe a
/// serialization format; applications can encode [`PersistedSession`] with the
/// storage and compatibility policy they need.
pub trait SessionStore: std::fmt::Debug + Send + Sync + 'static {
    /// Loads the last complete session checkpoint for `client_id`.
    fn load<'a>(&'a self, client_id: &'a str) -> SessionStoreFuture<'a, Option<PersistedSession>>;

    /// Saves a complete session checkpoint.
    ///
    /// Return only after the checkpoint is durable enough for the backend's
    /// crash-consistency guarantees.
    fn save<'a>(&'a self, session: &'a PersistedSession) -> SessionStoreFuture<'a, ()>;

    /// Clears any checkpoint associated with `client_id`.
    fn clear<'a>(&'a self, client_id: &'a str) -> SessionStoreFuture<'a, ()>;
}

/// Backend-neutral MQTT 5 session checkpoint.
///
/// This type contains protocol state required to resume local client ownership
/// of in-flight `QoS` and control-packet flows after recreating an `EventLoop`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedSession {
    /// Persistence model version emitted by this crate.
    pub format_version: u16,
    /// MQTT Client Identifier this checkpoint belongs to.
    pub client_id: String,
    /// `clean_start` value the checkpoint was created under.
    pub clean_start: bool,
    /// CONNECT Session Expiry Interval associated with this checkpoint.
    pub session_expiry_interval: Option<u32>,
    /// Configured outgoing inflight upper limit, if any.
    pub outgoing_inflight_upper_limit: Option<u16>,
    /// Acknowledgement mode associated with incoming `QoS` state.
    pub ack_mode: PersistedAckMode,
    /// Last allocated client packet identifier.
    pub last_pkid: u16,
    /// Last contiguous completed outgoing publish packet identifier.
    pub last_puback: u16,
    /// Outgoing publish/control requests that must be replayed after reconnect.
    pub replay: Vec<PersistedRequest>,
    /// Incoming `QoS` 2 publishes for which PUBREC has already been sent.
    pub incoming_qos2: Vec<PersistedIncomingQos2>,
}

/// Persisted representation of [`crate::AckMode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistedAckMode {
    /// Incoming acknowledgements are emitted automatically by the client.
    Automatic,
    /// Incoming acknowledgements are controlled by the application.
    Manual,
}

/// Persisted outbound protocol request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersistedRequest {
    /// `QoS` 1/2 PUBLISH awaiting acknowledgement.
    Publish(PersistedPublish),
    /// `QoS` 2 PUBREL awaiting PUBCOMP.
    PubRel(PersistedPubRel),
    /// SUBSCRIBE awaiting SUBACK.
    Subscribe(PersistedSubscribe),
    /// UNSUBSCRIBE awaiting UNSUBACK.
    Unsubscribe(PersistedUnsubscribe),
}

/// Persisted PUBLISH packet state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedPublish {
    /// MQTT DUP flag to use when replaying the publish.
    pub dup: bool,
    /// Publish `QoS`.
    pub qos: PersistedQoS,
    /// MQTT retain flag.
    pub retain: bool,
    /// Topic bytes.
    pub topic: Vec<u8>,
    /// Packet identifier.
    pub pkid: u16,
    /// Publish payload.
    pub payload: Vec<u8>,
    /// MQTT 5 publish properties.
    pub properties: Option<PersistedPublishProperties>,
}

/// Persisted PUBREL packet state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedPubRel {
    /// Packet identifier.
    pub pkid: u16,
}

/// Persisted SUBSCRIBE packet state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedSubscribe {
    /// Packet identifier.
    pub pkid: u16,
    /// Subscription filters.
    pub filters: Vec<PersistedFilter>,
    /// MQTT 5 subscribe properties.
    pub properties: Option<PersistedSubscribeProperties>,
}

/// Persisted UNSUBSCRIBE packet state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedUnsubscribe {
    /// Packet identifier.
    pub pkid: u16,
    /// Unsubscribe filters.
    pub filters: Vec<String>,
    /// MQTT 5 unsubscribe properties.
    pub properties: Option<PersistedUnsubscribeProperties>,
}

/// Incoming `QoS` 2 publish state persisted after PUBREC has been sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedIncomingQos2 {
    /// Packet identifier.
    pub pkid: u16,
}

/// Persisted representation of MQTT `QoS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistedQoS {
    /// At most once delivery.
    AtMostOnce,
    /// At least once delivery.
    AtLeastOnce,
    /// Exactly once delivery.
    ExactlyOnce,
}

/// Persisted subset of MQTT 5 PUBLISH properties.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedPublishProperties {
    /// Payload Format Indicator.
    pub payload_format_indicator: Option<u8>,
    /// Message Expiry Interval.
    pub message_expiry_interval: Option<u32>,
    /// Response Topic.
    pub response_topic: Option<String>,
    /// Correlation Data.
    pub correlation_data: Option<Vec<u8>>,
    /// User Properties.
    pub user_properties: Vec<(String, String)>,
    /// Subscription Identifiers.
    pub subscription_identifiers: Vec<usize>,
    /// Content Type.
    pub content_type: Option<String>,
}

/// Persisted subscription filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedFilter {
    /// Filter path.
    pub path: String,
    /// Maximum `QoS` requested for this filter.
    pub qos: PersistedQoS,
    /// MQTT 5 No Local subscription option.
    pub nolocal: bool,
    /// MQTT 5 Retain As Published subscription option.
    pub preserve_retain: bool,
    /// MQTT 5 Retain Handling subscription option.
    pub retain_forward_rule: PersistedRetainForwardRule,
}

/// Persisted representation of MQTT 5 retain handling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistedRetainForwardRule {
    /// Send retained messages on every subscribe.
    OnEverySubscribe,
    /// Send retained messages only for a new subscription.
    OnNewSubscribe,
    /// Never send retained messages for this subscribe.
    Never,
}

/// Persisted subset of MQTT 5 SUBSCRIBE properties.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedSubscribeProperties {
    /// Subscription Identifier.
    pub id: Option<usize>,
    /// User Properties.
    pub user_properties: Vec<(String, String)>,
}

/// Persisted subset of MQTT 5 UNSUBSCRIBE properties.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedUnsubscribeProperties {
    /// User Properties.
    pub user_properties: Vec<(String, String)>,
}

/// Error returned when a persisted session cannot be restored under the current
/// client configuration.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SessionRestoreError {
    #[error("unsupported persisted session format version {actual}")]
    UnsupportedFormatVersion { actual: u16 },
    #[error("persisted session belongs to client '{persisted}', not '{configured}'")]
    ClientIdMismatch {
        persisted: String,
        configured: String,
    },
    #[error("persisted clean_start={persisted} does not match configured clean_start={configured}")]
    CleanStartMismatch { persisted: bool, configured: bool },
    #[error(
        "persisted outgoing_inflight_upper_limit={persisted:?} does not match configured outgoing_inflight_upper_limit={configured:?}"
    )]
    OutgoingInflightUpperLimitMismatch {
        persisted: Option<u16>,
        configured: Option<u16>,
    },
    #[error("persisted ack mode does not match configured ack mode")]
    AckModeMismatch,
    #[error("persisted request contains invalid packet identifier {0}")]
    InvalidPacketIdentifier(u16),
    #[error(
        "persisted request packet identifier {pkid} exceeds outgoing inflight limit {max_outgoing_inflight}"
    )]
    OutgoingPacketIdentifierOutOfRange {
        pkid: u16,
        max_outgoing_inflight: u16,
    },
    #[error(
        "persisted last_puback {last_puback} exceeds outgoing inflight limit {max_outgoing_inflight}"
    )]
    LastPubAckOutOfRange {
        last_puback: u16,
        max_outgoing_inflight: u16,
    },
}
