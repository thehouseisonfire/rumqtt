use std::error::Error;
use std::future::Future;
use std::pin::Pin;

/// Error returned by a [`SessionStore`] backend.
pub type SessionStoreError = Box<dyn Error + Send + Sync>;

/// Boxed async result used by [`SessionStore`] methods.
pub type SessionStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SessionStoreError>> + Send + 'a>>;

/// Durable storage for MQTT 3.1.1 client session checkpoints.
///
/// Implementations must make `save` crash-consistent. A later `load` should
/// return either the last complete checkpoint or no checkpoint, never a
/// partially written session. The crate intentionally does not prescribe a
/// serialization format; applications can encode [`PersistedSession`] with the
/// storage and compatibility policy they need.
///
/// rumqttc calls [`SessionStore::clear`] when local session state is explicitly
/// reset or when the broker reports that no previous session is present. A
/// failed `load`, `save`, or `clear` is surfaced as
/// [`crate::ConnectionError::SessionStore`].
pub trait SessionStore: std::fmt::Debug + Send + Sync + 'static {
    /// Loads the last complete session checkpoint for `client_id`.
    fn load<'a>(&'a self, client_id: &'a str) -> SessionStoreFuture<'a, Option<PersistedSession>>;

    /// Saves a complete session checkpoint.
    ///
    /// Return only after the checkpoint is durable enough for the backend's
    /// crash-consistency guarantees.
    fn save<'a>(&'a self, session: &'a PersistedSession) -> SessionStoreFuture<'a, ()>;

    /// Clears any checkpoint associated with `client_id`.
    ///
    /// This should make a following `load(client_id)` return `None` once the
    /// clear operation completes.
    fn clear<'a>(&'a self, client_id: &'a str) -> SessionStoreFuture<'a, ()>;
}

/// Backend-neutral MQTT 3.1.1 session checkpoint.
///
/// This type contains protocol state required to resume local client ownership
/// of in-flight `QoS` and control-packet flows after recreating an `EventLoop`.
///
/// The structure is intentionally public so applications can serialize it with
/// their own format and compatibility policy. Restore validates that the
/// checkpoint belongs to the configured client and is compatible with packet-id
/// tracking, acknowledgement mode, and other local options; invalid checkpoint
/// contents are reported as [`SessionRestoreError`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedSession {
    /// Persistence model version emitted by this crate.
    pub format_version: u16,
    /// MQTT Client Identifier this checkpoint belongs to.
    pub client_id: String,
    /// `clean_session` value the checkpoint was created under.
    pub clean_session: bool,
    /// Configured outgoing publish inflight limit.
    pub max_inflight: u16,
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
}

/// Persisted UNSUBSCRIBE packet state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedUnsubscribe {
    /// Packet identifier.
    pub pkid: u16,
    /// Unsubscribe filters.
    pub topics: Vec<String>,
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

/// Persisted subscription filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedFilter {
    /// Filter path.
    pub path: String,
    /// Maximum `QoS` requested for this filter.
    pub qos: PersistedQoS,
}

/// Error returned when a persisted session cannot be restored under the current
/// client configuration.
///
/// These errors indicate that the store returned a checkpoint which rumqttc
/// will not replay. During `EventLoop::poll`, they are wrapped in
/// [`crate::ConnectionError::SessionRestore`].
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
    #[error(
        "persisted clean_session={persisted} does not match configured clean_session={configured}"
    )]
    CleanSessionMismatch { persisted: bool, configured: bool },
    #[error(
        "persisted max_inflight={persisted} does not match configured max_inflight={configured}"
    )]
    MaxInflightMismatch { persisted: u16, configured: u16 },
    #[error("persisted ack mode does not match configured ack mode")]
    AckModeMismatch,
    #[error("persisted request contains invalid packet identifier {0}")]
    InvalidPacketIdentifier(u16),
    #[error(
        "persisted request packet identifier {pkid} exceeds outgoing inflight limit {max_inflight}"
    )]
    OutgoingPacketIdentifierOutOfRange { pkid: u16, max_inflight: u16 },
    #[error("persisted last_puback {last_puback} exceeds outgoing inflight limit {max_inflight}")]
    LastPubAckOutOfRange { last_puback: u16, max_inflight: u16 },
}
