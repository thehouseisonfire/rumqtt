use std::error::Error;
use std::future::Future;
use std::pin::Pin;

/// Error returned by a [`SessionStore`] backend.
pub type SessionStoreError = Box<dyn Error + Send + Sync>;

/// Boxed async result used by [`SessionStore`] methods.
pub type SessionStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SessionStoreError>> + Send + 'a>>;

/// Storage identity for a persisted client session.
///
/// The `scope` is application-defined and should identify the deployment
/// boundary in which an MQTT `client_id` is unique, such as a broker cluster,
/// tenant, environment, or connection profile. The default empty scope is only
/// appropriate when the store instance is already scoped by configuration, such
/// as a dedicated file directory for one connection profile.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionStoreKey {
    scope: String,
    client_id: String,
}

impl SessionStoreKey {
    /// Creates a new session store key.
    pub fn new(scope: impl Into<String>, client_id: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            client_id: client_id.into(),
        }
    }

    /// Returns the application-defined storage scope.
    #[must_use]
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// Returns the MQTT Client Identifier.
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
}

/// Durable storage for MQTT 5 client session checkpoints.
///
/// `SessionStore` persists MQTT protocol recovery state that has already been
/// admitted into the client state machine. This includes in-flight `QoS`
/// flows, packet identifier ownership and progress, pending
/// SUBSCRIBE/UNSUBSCRIBE protocol state, and incoming `QoS` 2 state.
///
/// Implementations must make `save` and `clear` crash-consistent. A later
/// `load` should return either the previous complete checkpoint, the new
/// complete checkpoint, or no checkpoint, never a partially written session. If
/// a store future is cancelled before completion, the commit status may be
/// indeterminate, but the same no-torn-checkpoint rule still applies.
///
/// rumqttc provides canonical [`PersistedSession::encode`] and
/// [`PersistedSession::decode`] helpers for backends that want to store an
/// opaque crate-owned byte format. Applications can still encode the public
/// model with their own storage and compatibility policy if needed.
///
/// Exactly one active [`crate::EventLoop`] may own and modify a given
/// [`SessionStoreKey`] at a time. The core `SessionStore` abstraction does not
/// provide leases, fencing, compare-and-swap, or active/passive failover
/// coordination.
///
/// rumqttc calls [`SessionStore::clear`] when local session state is explicitly
/// reset, when the broker reports that no previous session is present, or when
/// the effective MQTT session expiry is zero at disconnect. A failed `load`,
/// `save`, or `clear` is surfaced as [`crate::ConnectionError::SessionStore`].
///
/// This is not a durable application outbox. Requests accepted by the client
/// but not yet admitted into MQTT protocol state are retried across ordinary
/// live reconnects while the same `EventLoop` remains alive, but they are not
/// part of this checkpoint and can be lost if the process exits, crashes, or
/// the `EventLoop` is dropped. Applications that need every submitted request
/// to survive process restart must keep their own durable outbound queue.
pub trait SessionStore: std::fmt::Debug + Send + Sync + 'static {
    /// Loads the last complete session checkpoint for `key`.
    fn load<'a>(
        &'a self,
        key: &'a SessionStoreKey,
    ) -> SessionStoreFuture<'a, Option<PersistedSession>>;

    /// Saves a complete session checkpoint.
    ///
    /// Return only after the checkpoint is durable enough for the backend's
    /// crash-consistency guarantees.
    fn save<'a>(
        &'a self,
        key: &'a SessionStoreKey,
        session: &'a PersistedSession,
    ) -> SessionStoreFuture<'a, ()>;

    /// Clears any checkpoint associated with `key`.
    ///
    /// This should make a following `load(key)` return `None` once the
    /// clear operation completes and is durable enough for the backend's
    /// crash-consistency guarantees.
    fn clear<'a>(&'a self, key: &'a SessionStoreKey) -> SessionStoreFuture<'a, ()>;
}

/// Backend-neutral MQTT 5 session checkpoint.
///
/// This type contains protocol state required to resume local client ownership
/// of in-flight `QoS` and control-packet flows after recreating an `EventLoop`.
/// Requests in [`PersistedSession::replay`] retain their packet identifiers and
/// MQTT replay semantics across restoration.
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
    /// `clean_start` value the checkpoint was created under.
    pub clean_start: bool,
    /// Effective Session Expiry Interval governing this checkpoint.
    ///
    /// This is normally the CONNECT value, including any server override
    /// received in CONNACK. A checkpoint saved after a graceful client
    /// DISCONNECT records its Session Expiry Interval override instead.
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
    ///
    /// These requests have already been admitted into MQTT protocol state and
    /// retain their packet identifiers across restoration.
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
    ///
    /// This is the conservative recovery representation, not the live
    /// first-send packet, which remains `DUP=0`. New checkpoints emit `true`,
    /// and restore treats `false` from an older compatible checkpoint as
    /// `true`. See the persistence design documentation for the
    /// crash-before-send qualification.
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

const SESSION_MAGIC: &[u8; 7] = b"RMQSESS";
const SESSION_PROTOCOL: u8 = 5;
const SESSION_CODEC_VERSION: u16 = 1;
const MAX_BINARY_LEN: usize = 268_435_455;
const MAX_COLLECTION_LEN: usize = 1_000_000;

/// Error returned when a persisted session cannot be canonically encoded.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SessionEncodeError {
    #[error("field '{field}' is too large to encode: {len} bytes")]
    FieldTooLarge { field: &'static str, len: usize },
}

/// Error returned when a canonical persisted session byte stream cannot be decoded.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SessionDecodeError {
    #[error("invalid persisted session magic")]
    InvalidMagic,
    #[error("persisted session protocol marker {actual} does not match MQTT 5")]
    WrongProtocol { actual: u8 },
    #[error("unsupported persisted session codec version {actual}")]
    UnsupportedCodecVersion { actual: u16 },
    #[error("persisted session ended while reading {field}")]
    Truncated { field: &'static str },
    #[error("persisted session has trailing bytes")]
    TrailingBytes,
    #[error("invalid persisted session UTF-8 in {field}")]
    InvalidUtf8 { field: &'static str },
    #[error("invalid persisted session enum tag {tag} for {field}")]
    InvalidEnumTag { field: &'static str, tag: u8 },
    #[error("field '{field}' exceeds decode limit: {len}")]
    LimitExceeded { field: &'static str, len: usize },
}

impl PersistedSession {
    /// Encodes this checkpoint with rumqttc's canonical binary session format.
    ///
    /// The byte stream contains its own magic, MQTT protocol marker, and codec
    /// version. Storage backends can persist these bytes as an opaque value.
    ///
    /// # Errors
    ///
    /// Returns [`SessionEncodeError::FieldTooLarge`] if a persisted string,
    /// binary value, or collection exceeds the session format limits.
    pub fn encode(&self) -> Result<Vec<u8>, SessionEncodeError> {
        let mut writer = SessionWriter { bytes: Vec::new() };
        writer.bytes.extend_from_slice(SESSION_MAGIC);
        writer.u8(SESSION_PROTOCOL);
        writer.u16(SESSION_CODEC_VERSION);
        writer.u16(self.format_version);
        writer.string("client_id", &self.client_id)?;
        writer.bool(self.clean_start);
        writer.option_u32(self.session_expiry_interval);
        writer.option_u16(self.outgoing_inflight_upper_limit);
        writer.u8(encode_ack_mode(self.ack_mode));
        writer.u16(self.last_pkid);
        writer.u16(self.last_puback);
        writer.vec("replay", &self.replay, write_request)?;
        writer.vec("incoming_qos2", &self.incoming_qos2, |writer, incoming| {
            writer.u16(incoming.pkid);
            Ok(())
        })?;
        Ok(writer.bytes)
    }

    /// Decodes a checkpoint emitted by [`PersistedSession::encode`].
    ///
    /// # Errors
    ///
    /// Returns [`SessionDecodeError`] if the checkpoint is malformed,
    /// unsupported, truncated, or exceeds the session format limits.
    pub fn decode(bytes: &[u8]) -> Result<Self, SessionDecodeError> {
        let mut reader = SessionReader { bytes, position: 0 };
        reader.magic()?;
        let protocol = reader.u8("protocol")?;
        if protocol != SESSION_PROTOCOL {
            return Err(SessionDecodeError::WrongProtocol { actual: protocol });
        }
        let codec_version = reader.u16("codec_version")?;
        if codec_version != SESSION_CODEC_VERSION {
            return Err(SessionDecodeError::UnsupportedCodecVersion {
                actual: codec_version,
            });
        }

        let session = Self {
            format_version: reader.u16("format_version")?,
            client_id: reader.string("client_id")?,
            clean_start: reader.bool("clean_start")?,
            session_expiry_interval: reader.option_u32("session_expiry_interval")?,
            outgoing_inflight_upper_limit: reader.option_u16("outgoing_inflight_upper_limit")?,
            ack_mode: decode_ack_mode(reader.u8("ack_mode")?)?,
            last_pkid: reader.u16("last_pkid")?,
            last_puback: reader.u16("last_puback")?,
            replay: reader.vec("replay", read_request)?,
            incoming_qos2: reader.vec("incoming_qos2", |reader| {
                Ok(PersistedIncomingQos2 {
                    pkid: reader.u16("incoming_qos2.pkid")?,
                })
            })?,
        };
        reader.finish()?;
        Ok(session)
    }
}

struct SessionWriter {
    bytes: Vec<u8>,
}

impl SessionWriter {
    fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn option_u16(&mut self, value: Option<u16>) {
        self.bool(value.is_some());
        if let Some(value) = value {
            self.u16(value);
        }
    }

    fn option_u32(&mut self, value: Option<u32>) {
        self.bool(value.is_some());
        if let Some(value) = value {
            self.u32(value);
        }
    }

    fn option_usize(&mut self, value: Option<usize>) {
        self.bool(value.is_some());
        if let Some(value) = value {
            self.u64(value as u64);
        }
    }

    fn bytes(&mut self, field: &'static str, value: &[u8]) -> Result<(), SessionEncodeError> {
        if value.len() > MAX_BINARY_LEN {
            return Err(SessionEncodeError::FieldTooLarge {
                field,
                len: value.len(),
            });
        }
        let len = u32::try_from(value.len()).map_err(|_| SessionEncodeError::FieldTooLarge {
            field,
            len: value.len(),
        })?;
        self.u32(len);
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn string(&mut self, field: &'static str, value: &str) -> Result<(), SessionEncodeError> {
        self.bytes(field, value.as_bytes())
    }

    fn vec<T>(
        &mut self,
        field: &'static str,
        values: &[T],
        mut write: impl FnMut(&mut Self, &T) -> Result<(), SessionEncodeError>,
    ) -> Result<(), SessionEncodeError> {
        if values.len() > MAX_COLLECTION_LEN {
            return Err(SessionEncodeError::FieldTooLarge {
                field,
                len: values.len(),
            });
        }
        let len = u32::try_from(values.len()).map_err(|_| SessionEncodeError::FieldTooLarge {
            field,
            len: values.len(),
        })?;
        self.u32(len);
        for value in values {
            write(self, value)?;
        }
        Ok(())
    }
}

struct SessionReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> SessionReader<'a> {
    fn magic(&mut self) -> Result<(), SessionDecodeError> {
        let magic = self.take("magic", SESSION_MAGIC.len())?;
        if magic != SESSION_MAGIC {
            return Err(SessionDecodeError::InvalidMagic);
        }
        Ok(())
    }

    const fn finish(&self) -> Result<(), SessionDecodeError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(SessionDecodeError::TrailingBytes)
        }
    }

    fn take(&mut self, field: &'static str, len: usize) -> Result<&'a [u8], SessionDecodeError> {
        let end = self
            .position
            .checked_add(len)
            .ok_or(SessionDecodeError::LimitExceeded { field, len })?;
        let Some(bytes) = self.bytes.get(self.position..end) else {
            return Err(SessionDecodeError::Truncated { field });
        };
        self.position = end;
        Ok(bytes)
    }

    fn bool(&mut self, field: &'static str) -> Result<bool, SessionDecodeError> {
        match self.u8(field)? {
            0 => Ok(false),
            1 => Ok(true),
            tag => Err(SessionDecodeError::InvalidEnumTag { field, tag }),
        }
    }

    fn u8(&mut self, field: &'static str) -> Result<u8, SessionDecodeError> {
        Ok(self.take(field, 1)?[0])
    }

    fn u16(&mut self, field: &'static str) -> Result<u16, SessionDecodeError> {
        let bytes = self.take(field, 2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self, field: &'static str) -> Result<u32, SessionDecodeError> {
        let bytes = self.take(field, 4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self, field: &'static str) -> Result<u64, SessionDecodeError> {
        let bytes = self.take(field, 8)?;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn option_u16(&mut self, field: &'static str) -> Result<Option<u16>, SessionDecodeError> {
        if self.bool(field)? {
            Ok(Some(self.u16(field)?))
        } else {
            Ok(None)
        }
    }

    fn option_u32(&mut self, field: &'static str) -> Result<Option<u32>, SessionDecodeError> {
        if self.bool(field)? {
            Ok(Some(self.u32(field)?))
        } else {
            Ok(None)
        }
    }

    fn option_usize(&mut self, field: &'static str) -> Result<Option<usize>, SessionDecodeError> {
        if !self.bool(field)? {
            return Ok(None);
        }
        let value = self.u64(field)?;
        usize::try_from(value)
            .map(Some)
            .map_err(|_| SessionDecodeError::LimitExceeded {
                field,
                len: usize::MAX,
            })
    }

    fn bytes(&mut self, field: &'static str) -> Result<Vec<u8>, SessionDecodeError> {
        let len = self.u32(field)? as usize;
        if len > MAX_BINARY_LEN {
            return Err(SessionDecodeError::LimitExceeded { field, len });
        }
        Ok(self.take(field, len)?.to_vec())
    }

    fn string(&mut self, field: &'static str) -> Result<String, SessionDecodeError> {
        String::from_utf8(self.bytes(field)?).map_err(|_| SessionDecodeError::InvalidUtf8 { field })
    }

    fn vec<T>(
        &mut self,
        field: &'static str,
        mut read: impl FnMut(&mut Self) -> Result<T, SessionDecodeError>,
    ) -> Result<Vec<T>, SessionDecodeError> {
        let len = self.u32(field)? as usize;
        if len > MAX_COLLECTION_LEN {
            return Err(SessionDecodeError::LimitExceeded { field, len });
        }
        let mut values = Vec::with_capacity(len);
        for _ in 0..len {
            values.push(read(self)?);
        }
        Ok(values)
    }
}

fn write_request(
    writer: &mut SessionWriter,
    request: &PersistedRequest,
) -> Result<(), SessionEncodeError> {
    match request {
        PersistedRequest::Publish(publish) => {
            writer.u8(1);
            writer.bool(publish.dup);
            writer.u8(encode_qos(publish.qos));
            writer.bool(publish.retain);
            writer.bytes("publish.topic", &publish.topic)?;
            writer.u16(publish.pkid);
            writer.bytes("publish.payload", &publish.payload)?;
            write_publish_properties(writer, publish.properties.as_ref())
        }
        PersistedRequest::PubRel(pubrel) => {
            writer.u8(2);
            writer.u16(pubrel.pkid);
            Ok(())
        }
        PersistedRequest::Subscribe(subscribe) => {
            writer.u8(3);
            writer.u16(subscribe.pkid);
            writer.vec("subscribe.filters", &subscribe.filters, write_filter)?;
            write_subscribe_properties(writer, subscribe.properties.as_ref())
        }
        PersistedRequest::Unsubscribe(unsubscribe) => {
            writer.u8(4);
            writer.u16(unsubscribe.pkid);
            writer.vec(
                "unsubscribe.filters",
                &unsubscribe.filters,
                |writer, filter| writer.string("unsubscribe.filter", filter),
            )?;
            write_unsubscribe_properties(writer, unsubscribe.properties.as_ref())
        }
    }
}

fn read_request(reader: &mut SessionReader<'_>) -> Result<PersistedRequest, SessionDecodeError> {
    match reader.u8("request.type")? {
        1 => Ok(PersistedRequest::Publish(PersistedPublish {
            dup: reader.bool("publish.dup")?,
            qos: decode_qos(reader.u8("publish.qos")?)?,
            retain: reader.bool("publish.retain")?,
            topic: reader.bytes("publish.topic")?,
            pkid: reader.u16("publish.pkid")?,
            payload: reader.bytes("publish.payload")?,
            properties: read_publish_properties(reader)?,
        })),
        2 => Ok(PersistedRequest::PubRel(PersistedPubRel {
            pkid: reader.u16("pubrel.pkid")?,
        })),
        3 => Ok(PersistedRequest::Subscribe(PersistedSubscribe {
            pkid: reader.u16("subscribe.pkid")?,
            filters: reader.vec("subscribe.filters", read_filter)?,
            properties: read_subscribe_properties(reader)?,
        })),
        4 => Ok(PersistedRequest::Unsubscribe(PersistedUnsubscribe {
            pkid: reader.u16("unsubscribe.pkid")?,
            filters: reader.vec("unsubscribe.filters", |reader| {
                reader.string("unsubscribe.filter")
            })?,
            properties: read_unsubscribe_properties(reader)?,
        })),
        tag => Err(SessionDecodeError::InvalidEnumTag {
            field: "request.type",
            tag,
        }),
    }
}

fn write_filter(
    writer: &mut SessionWriter,
    filter: &PersistedFilter,
) -> Result<(), SessionEncodeError> {
    writer.string("subscribe.filter.path", &filter.path)?;
    writer.u8(encode_qos(filter.qos));
    writer.bool(filter.nolocal);
    writer.bool(filter.preserve_retain);
    writer.u8(encode_retain_forward_rule(filter.retain_forward_rule));
    Ok(())
}

fn read_filter(reader: &mut SessionReader<'_>) -> Result<PersistedFilter, SessionDecodeError> {
    Ok(PersistedFilter {
        path: reader.string("subscribe.filter.path")?,
        qos: decode_qos(reader.u8("subscribe.filter.qos")?)?,
        nolocal: reader.bool("subscribe.filter.nolocal")?,
        preserve_retain: reader.bool("subscribe.filter.preserve_retain")?,
        retain_forward_rule: decode_retain_forward_rule(
            reader.u8("subscribe.filter.retain_forward_rule")?,
        )?,
    })
}

fn write_publish_properties(
    writer: &mut SessionWriter,
    properties: Option<&PersistedPublishProperties>,
) -> Result<(), SessionEncodeError> {
    writer.bool(properties.is_some());
    let Some(properties) = properties else {
        return Ok(());
    };
    writer.option_u8(properties.payload_format_indicator);
    writer.option_u32(properties.message_expiry_interval);
    writer.option_string(
        "publish.properties.response_topic",
        properties.response_topic.as_deref(),
    )?;
    writer.option_bytes(
        "publish.properties.correlation_data",
        properties.correlation_data.as_deref(),
    )?;
    write_user_properties(
        writer,
        "publish.properties.user_properties",
        &properties.user_properties,
    )?;
    writer.vec(
        "publish.properties.subscription_identifiers",
        &properties.subscription_identifiers,
        |writer, id| {
            writer.u64(*id as u64);
            Ok(())
        },
    )?;
    writer.option_string(
        "publish.properties.content_type",
        properties.content_type.as_deref(),
    )
}

fn read_publish_properties(
    reader: &mut SessionReader<'_>,
) -> Result<Option<PersistedPublishProperties>, SessionDecodeError> {
    if !reader.bool("publish.properties")? {
        return Ok(None);
    }
    Ok(Some(PersistedPublishProperties {
        payload_format_indicator: reader
            .option_u8("publish.properties.payload_format_indicator")?,
        message_expiry_interval: reader.option_u32("publish.properties.message_expiry_interval")?,
        response_topic: reader.option_string("publish.properties.response_topic")?,
        correlation_data: reader.option_bytes("publish.properties.correlation_data")?,
        user_properties: read_user_properties(reader, "publish.properties.user_properties")?,
        subscription_identifiers: reader.vec(
            "publish.properties.subscription_identifiers",
            |reader| {
                let value = reader.u64("publish.properties.subscription_identifier")?;
                usize::try_from(value).map_err(|_| SessionDecodeError::LimitExceeded {
                    field: "publish.properties.subscription_identifier",
                    len: usize::MAX,
                })
            },
        )?,
        content_type: reader.option_string("publish.properties.content_type")?,
    }))
}

fn write_subscribe_properties(
    writer: &mut SessionWriter,
    properties: Option<&PersistedSubscribeProperties>,
) -> Result<(), SessionEncodeError> {
    writer.bool(properties.is_some());
    let Some(properties) = properties else {
        return Ok(());
    };
    writer.option_usize(properties.id);
    write_user_properties(
        writer,
        "subscribe.properties.user_properties",
        &properties.user_properties,
    )
}

fn read_subscribe_properties(
    reader: &mut SessionReader<'_>,
) -> Result<Option<PersistedSubscribeProperties>, SessionDecodeError> {
    if !reader.bool("subscribe.properties")? {
        return Ok(None);
    }
    Ok(Some(PersistedSubscribeProperties {
        id: reader.option_usize("subscribe.properties.id")?,
        user_properties: read_user_properties(reader, "subscribe.properties.user_properties")?,
    }))
}

fn write_unsubscribe_properties(
    writer: &mut SessionWriter,
    properties: Option<&PersistedUnsubscribeProperties>,
) -> Result<(), SessionEncodeError> {
    writer.bool(properties.is_some());
    let Some(properties) = properties else {
        return Ok(());
    };
    write_user_properties(
        writer,
        "unsubscribe.properties.user_properties",
        &properties.user_properties,
    )
}

fn read_unsubscribe_properties(
    reader: &mut SessionReader<'_>,
) -> Result<Option<PersistedUnsubscribeProperties>, SessionDecodeError> {
    if !reader.bool("unsubscribe.properties")? {
        return Ok(None);
    }
    Ok(Some(PersistedUnsubscribeProperties {
        user_properties: read_user_properties(reader, "unsubscribe.properties.user_properties")?,
    }))
}

fn write_user_properties(
    writer: &mut SessionWriter,
    field: &'static str,
    properties: &[(String, String)],
) -> Result<(), SessionEncodeError> {
    writer.vec(field, properties, |writer, (key, value)| {
        writer.string("user_property.key", key)?;
        writer.string("user_property.value", value)
    })
}

fn read_user_properties(
    reader: &mut SessionReader<'_>,
    field: &'static str,
) -> Result<Vec<(String, String)>, SessionDecodeError> {
    reader.vec(field, |reader| {
        Ok((
            reader.string("user_property.key")?,
            reader.string("user_property.value")?,
        ))
    })
}

impl SessionWriter {
    fn option_u8(&mut self, value: Option<u8>) {
        self.bool(value.is_some());
        if let Some(value) = value {
            self.u8(value);
        }
    }

    fn option_bytes(
        &mut self,
        field: &'static str,
        value: Option<&[u8]>,
    ) -> Result<(), SessionEncodeError> {
        self.bool(value.is_some());
        if let Some(value) = value {
            self.bytes(field, value)?;
        }
        Ok(())
    }

    fn option_string(
        &mut self,
        field: &'static str,
        value: Option<&str>,
    ) -> Result<(), SessionEncodeError> {
        self.bool(value.is_some());
        if let Some(value) = value {
            self.string(field, value)?;
        }
        Ok(())
    }
}

impl SessionReader<'_> {
    fn option_u8(&mut self, field: &'static str) -> Result<Option<u8>, SessionDecodeError> {
        if self.bool(field)? {
            Ok(Some(self.u8(field)?))
        } else {
            Ok(None)
        }
    }

    fn option_bytes(&mut self, field: &'static str) -> Result<Option<Vec<u8>>, SessionDecodeError> {
        if self.bool(field)? {
            Ok(Some(self.bytes(field)?))
        } else {
            Ok(None)
        }
    }

    fn option_string(&mut self, field: &'static str) -> Result<Option<String>, SessionDecodeError> {
        if self.bool(field)? {
            Ok(Some(self.string(field)?))
        } else {
            Ok(None)
        }
    }
}

const fn encode_ack_mode(ack_mode: PersistedAckMode) -> u8 {
    match ack_mode {
        PersistedAckMode::Automatic => 1,
        PersistedAckMode::Manual => 2,
    }
}

const fn decode_ack_mode(tag: u8) -> Result<PersistedAckMode, SessionDecodeError> {
    match tag {
        1 => Ok(PersistedAckMode::Automatic),
        2 => Ok(PersistedAckMode::Manual),
        tag => Err(SessionDecodeError::InvalidEnumTag {
            field: "ack_mode",
            tag,
        }),
    }
}

const fn encode_qos(qos: PersistedQoS) -> u8 {
    match qos {
        PersistedQoS::AtMostOnce => 0,
        PersistedQoS::AtLeastOnce => 1,
        PersistedQoS::ExactlyOnce => 2,
    }
}

const fn decode_qos(tag: u8) -> Result<PersistedQoS, SessionDecodeError> {
    match tag {
        0 => Ok(PersistedQoS::AtMostOnce),
        1 => Ok(PersistedQoS::AtLeastOnce),
        2 => Ok(PersistedQoS::ExactlyOnce),
        tag => Err(SessionDecodeError::InvalidEnumTag { field: "qos", tag }),
    }
}

const fn encode_retain_forward_rule(rule: PersistedRetainForwardRule) -> u8 {
    match rule {
        PersistedRetainForwardRule::OnEverySubscribe => 1,
        PersistedRetainForwardRule::OnNewSubscribe => 2,
        PersistedRetainForwardRule::Never => 3,
    }
}

const fn decode_retain_forward_rule(
    tag: u8,
) -> Result<PersistedRetainForwardRule, SessionDecodeError> {
    match tag {
        1 => Ok(PersistedRetainForwardRule::OnEverySubscribe),
        2 => Ok(PersistedRetainForwardRule::OnNewSubscribe),
        3 => Ok(PersistedRetainForwardRule::Never),
        tag => Err(SessionDecodeError::InvalidEnumTag {
            field: "retain_forward_rule",
            tag,
        }),
    }
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
    #[error("persisted session contains too many outgoing packet identifiers: {count}")]
    OutgoingPacketIdentifierCountOutOfRange { count: usize },
    #[error(
        "persisted last_puback {last_puback} exceeds outgoing inflight limit {max_outgoing_inflight}"
    )]
    LastPubAckOutOfRange {
        last_puback: u16,
        max_outgoing_inflight: u16,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_store_key_exposes_scope_and_client_id() {
        let key = SessionStoreKey::new("tenant-a/broker-1", "client-1");

        assert_eq!(key.scope(), "tenant-a/broker-1");
        assert_eq!(key.client_id(), "client-1");
    }

    #[test]
    fn canonical_codec_round_trips_all_v5_request_variants() {
        let session = PersistedSession {
            format_version: 1,
            client_id: "client".to_owned(),
            clean_start: false,
            session_expiry_interval: Some(60),
            outgoing_inflight_upper_limit: Some(10),
            ack_mode: PersistedAckMode::Manual,
            last_pkid: 9,
            last_puback: 3,
            replay: vec![
                PersistedRequest::Publish(PersistedPublish {
                    dup: true,
                    qos: PersistedQoS::AtLeastOnce,
                    retain: false,
                    topic: b"topic/a".to_vec(),
                    pkid: 4,
                    payload: b"payload".to_vec(),
                    properties: Some(PersistedPublishProperties {
                        payload_format_indicator: Some(1),
                        message_expiry_interval: Some(30),
                        response_topic: Some("reply".to_owned()),
                        correlation_data: Some(vec![1, 2, 3]),
                        user_properties: vec![("k".to_owned(), "v".to_owned())],
                        subscription_identifiers: vec![11],
                        content_type: Some("text/plain".to_owned()),
                    }),
                }),
                PersistedRequest::PubRel(PersistedPubRel { pkid: 5 }),
                PersistedRequest::Subscribe(PersistedSubscribe {
                    pkid: 6,
                    filters: vec![PersistedFilter {
                        path: "topic/#".to_owned(),
                        qos: PersistedQoS::ExactlyOnce,
                        nolocal: true,
                        preserve_retain: true,
                        retain_forward_rule: PersistedRetainForwardRule::OnNewSubscribe,
                    }],
                    properties: Some(PersistedSubscribeProperties {
                        id: Some(9),
                        user_properties: vec![("sub".to_owned(), "prop".to_owned())],
                    }),
                }),
                PersistedRequest::Unsubscribe(PersistedUnsubscribe {
                    pkid: 7,
                    filters: vec!["topic/a".to_owned()],
                    properties: Some(PersistedUnsubscribeProperties {
                        user_properties: vec![("unsub".to_owned(), "prop".to_owned())],
                    }),
                }),
            ],
            incoming_qos2: vec![PersistedIncomingQos2 { pkid: 8 }],
        };

        let encoded = session.encode().unwrap();
        assert_eq!(PersistedSession::decode(&encoded).unwrap(), session);
    }

    #[test]
    fn canonical_codec_has_stable_minimal_v5_vector() {
        let session = PersistedSession {
            format_version: 1,
            client_id: "c".to_owned(),
            clean_start: false,
            session_expiry_interval: Some(60),
            outgoing_inflight_upper_limit: None,
            ack_mode: PersistedAckMode::Automatic,
            last_pkid: 2,
            last_puback: 3,
            replay: Vec::new(),
            incoming_qos2: Vec::new(),
        };

        assert_eq!(
            session.encode().unwrap(),
            vec![
                b'R', b'M', b'Q', b'S', b'E', b'S', b'S', 5, 0, 1, 0, 1, 0, 0, 0, 1, b'c', 0, 1, 0,
                0, 0, 60, 0, 1, 0, 2, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0,
            ]
        );
    }

    #[test]
    fn canonical_decode_rejects_corrupt_v5_inputs() {
        assert!(matches!(
            PersistedSession::decode(b"not a session"),
            Err(SessionDecodeError::InvalidMagic)
        ));

        let mut wrong_protocol = PersistedSession {
            format_version: 1,
            client_id: "c".to_owned(),
            clean_start: false,
            session_expiry_interval: Some(60),
            outgoing_inflight_upper_limit: None,
            ack_mode: PersistedAckMode::Automatic,
            last_pkid: 2,
            last_puback: 3,
            replay: Vec::new(),
            incoming_qos2: Vec::new(),
        }
        .encode()
        .unwrap();
        wrong_protocol[7] = 4;
        assert!(matches!(
            PersistedSession::decode(&wrong_protocol),
            Err(SessionDecodeError::WrongProtocol { actual: 4 })
        ));

        let mut trailing = wrong_protocol;
        trailing[7] = 5;
        trailing.push(0);
        assert!(matches!(
            PersistedSession::decode(&trailing),
            Err(SessionDecodeError::TrailingBytes)
        ));

        let truncated = &trailing[..trailing.len() - 3];
        assert!(matches!(
            PersistedSession::decode(truncated),
            Err(SessionDecodeError::Truncated { .. })
        ));
    }
}
