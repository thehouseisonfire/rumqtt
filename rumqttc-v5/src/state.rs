use super::mqttbytes::v5::{
    Auth, ConnAck, ConnectReturnCode, Disconnect, DisconnectReasonCode, Packet, PubAck,
    PubAckReason, PubComp, PubCompReason, PubRec, PubRecReason, PubRel, PubRelReason, Publish,
    PublishProperties, RetainForwardRule, SubAck, Subscribe, SubscribeFilter, SubscribeProperties,
    SubscribeReasonCode, UnsubAck, UnsubAckReason, Unsubscribe, UnsubscribeProperties,
};
use super::mqttbytes::{self, Error as MqttError, PacketType, QoS};
use crate::auth::{AuthLifecycle, IncomingAuthEffect};
use crate::notice::{
    AuthNoticeError, DeferredNotice, PublishNoticeTx, PublishResult, SubscribeNoticeTx,
    TrackedNoticeTx, UnsubscribeNoticeTx,
};
use crate::session::{
    PersistedAckMode, PersistedFilter, PersistedIncomingQos2, PersistedPubRel, PersistedPublish,
    PersistedPublishProperties, PersistedQoS, PersistedRequest, PersistedRetainForwardRule,
    PersistedSession, PersistedSubscribe, PersistedSubscribeProperties, PersistedUnsubscribe,
    PersistedUnsubscribeProperties, SessionRestoreError,
};
use crate::{
    AckMode, AuthContext, AuthError, AuthExchangeKind, Authenticator, MqttOptions,
    NoticeFailureReason, PublishNoticeError, TopicAliasPolicy,
};

use super::{Event, Incoming, Outgoing, Request};

use bytes::Bytes;
use fixedbitset::FixedBitSet;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::{io, time::Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingAutoTopicAlias {
    topic: Bytes,
    alias: u16,
    previous_topic: Option<Bytes>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AutoTopicAliasAction {
    Existing { original_topic: Bytes, alias: u16 },
    New(PendingAutoTopicAlias),
}

impl AutoTopicAliasAction {
    const fn pending_alias(&self) -> Option<&PendingAutoTopicAlias> {
        match self {
            Self::Existing { .. } => None,
            Self::New(pending) => Some(pending),
        }
    }

    const fn existing_alias(&self) -> Option<u16> {
        match self {
            Self::Existing { alias, .. } => Some(*alias),
            Self::New(_) => None,
        }
    }

    fn restore_for_replay(self, publish: &mut Publish) {
        match self {
            Self::Existing { original_topic, .. } => {
                publish.topic = original_topic;
                MqttState::strip_publish_topic_alias(publish);
            }
            Self::New(_) => {
                MqttState::strip_publish_topic_alias(publish);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PingState {
    Idle { collision_count: usize },
    AwaitingResp { collision_count: usize },
}

impl PingState {
    const fn new() -> Self {
        Self::Idle { collision_count: 0 }
    }

    const fn await_pingresp(self) -> bool {
        matches!(self, Self::AwaitingResp { .. })
    }

    const fn collision_count(self) -> usize {
        match self {
            Self::Idle { collision_count } | Self::AwaitingResp { collision_count } => {
                collision_count
            }
        }
    }

    const fn increment_collision_count(&mut self) -> usize {
        let collision_count = self.collision_count() + 1;
        *self = match self {
            Self::Idle { .. } => Self::Idle { collision_count },
            Self::AwaitingResp { .. } => Self::AwaitingResp { collision_count },
        };
        collision_count
    }

    const fn begin_waiting_for_resp(&mut self) {
        let collision_count = self.collision_count();
        *self = Self::AwaitingResp { collision_count };
    }

    const fn finish_waiting_for_resp(&mut self) {
        let collision_count = self.collision_count();
        *self = Self::Idle { collision_count };
    }

    const fn reset_collision_count(&mut self) {
        *self = if self.await_pingresp() {
            Self::AwaitingResp { collision_count: 0 }
        } else {
            Self::Idle { collision_count: 0 }
        };
    }

    const fn reset(&mut self) {
        *self = Self::new();
    }
}

#[derive(Clone, Debug)]
struct TopicAliasState {
    /// Server-to-client topic aliases scoped to the current network connection.
    incoming: HashMap<u16, Bytes>,
    /// Client-to-server topic aliases scoped to the current network connection.
    outgoing: HashMap<u16, Bytes>,
    /// Automatically assigned client-to-server topic aliases for the current network connection.
    auto_outgoing: HashMap<Bytes, u16>,
    next_auto: Option<u16>,
    auto_policy: TopicAliasPolicy,
    auto_lru: VecDeque<u16>,
    /// `topic_alias_maximum` RECEIVED via connack packet
    broker_max: u16,
    /// `topic_alias_maximum` SENT in the CONNECT packet.
    /// The client must reject incoming topic aliases exceeding this value.
    /// A value of 0 means the client does not accept any topic aliases.
    client_max: u16,
}

impl TopicAliasState {
    fn new(auto_policy: TopicAliasPolicy, client_max: u16) -> Self {
        Self {
            incoming: HashMap::new(),
            outgoing: HashMap::new(),
            auto_outgoing: HashMap::new(),
            next_auto: Some(1),
            auto_policy,
            auto_lru: VecDeque::new(),
            broker_max: 0,
            client_max,
        }
    }

    fn reset_connection_scoped(&mut self) {
        self.incoming.clear();
        self.outgoing.clear();
        self.auto_outgoing.clear();
        self.next_auto = Some(1);
        self.auto_lru.clear();
        self.broker_max = 0;
    }

    fn replay_aliases(&self) -> HashMap<u16, Bytes> {
        self.outgoing.clone()
    }
}

/// Errors during state handling
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// Io Error while state is passed to network
    #[error("Io error: {0:?}")]
    Io(#[from] io::Error),
    #[error("Conversion error {0:?}")]
    Coversion(#[from] core::num::TryFromIntError),
    /// Invalid state for a given operation
    #[error("Invalid state for a given operation")]
    InvalidState,
    /// Received a packet (ack) which isn't asked for
    #[error("Received unsolicited ack pkid: {0}")]
    Unsolicited(u16),
    /// Last pingreq isn't acked
    #[error("Last pingreq isn't acked")]
    AwaitPingResp,
    #[error("Timeout while waiting to resolve collision")]
    CollisionTimeout,
    #[error("A Subscribe packet must contain atleast one filter")]
    EmptySubscription,
    #[error("Mqtt serialization/deserialization error: {0}")]
    Deserialization(MqttError),
    #[error("MQTT protocol violation: {0}")]
    ProtocolViolation(ProtocolViolation),
    #[error(
        "Cannot use topic alias '{alias:?}'. It's greater than the broker's maximum of '{max:?}'."
    )]
    InvalidAlias { alias: u16, max: u16 },
    #[error("Cannot send a retained publish because the broker does not support retained messages")]
    RetainNotSupported,
    #[error(
        "Cannot send packet of size '{pkt_size:?}'. It's greater than the broker's maximum packet size of: '{max:?}'"
    )]
    OutgoingPacketTooLarge { pkt_size: u32, max: u32 },
    #[error(
        "Cannot receive packet of size '{pkt_size:?}'. It's greater than the client's maximum packet size of: '{max:?}'"
    )]
    IncomingPacketTooLarge { pkt_size: usize, max: usize },
    #[error("Server sent disconnect with reason `{reason_string:?}` and code '{reason_code:?}' ")]
    ServerDisconnect {
        reason_code: DisconnectReasonCode,
        reason_string: Option<String>,
    },
    #[error("Connection failed with reason '{reason:?}' ")]
    ConnFail { reason: ConnectReturnCode },
    #[error("Connection closed by peer abruptly")]
    ConnectionAborted,
    #[error("Authentication error: {0}")]
    AuthError(String),
    #[error("Authenticator not set")]
    AuthenticatorNotSet,
    #[error("Authenticator lock poisoned")]
    AuthenticatorLockPoisoned,
}

/// MQTT protocol-state violations detected by the client state machine.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProtocolViolation {
    #[error("server sent a second CONNACK after the connection was established")]
    DuplicateConnAck,
    #[error("server sent packet type {0:?}, which is invalid in the current client state")]
    UnexpectedIncomingPacket(PacketType),
    #[error(
        "SUBACK return code count mismatch for packet id {pkid}: expected {expected}, got {actual}"
    )]
    SubAckReturnCodeCountMismatch {
        pkid: u16,
        expected: usize,
        actual: usize,
    },
    #[error(
        "UNSUBACK reason code count mismatch for packet id {pkid}: expected {expected}, got {actual}"
    )]
    UnsubAckReasonCodeCountMismatch {
        pkid: u16,
        expected: usize,
        actual: usize,
    },
}

impl From<mqttbytes::Error> for StateError {
    fn from(value: MqttError) -> Self {
        match value {
            MqttError::OutgoingPacketTooLarge { pkt_size, max } => {
                Self::OutgoingPacketTooLarge { pkt_size, max }
            }
            e => Self::Deserialization(e),
        }
    }
}

/// Observation-only snapshot of outbound protocol state.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboundDiagnostics {
    pub inflight: u16,
    pub max_inflight: u16,
    pub publish_window_full: bool,
    pub packet_identifiers_in_use: usize,
    pub collision: bool,
    pub collision_notice: bool,
    pub pending_subscribe: usize,
    pub pending_unsubscribe: usize,
    pub outgoing_publish: usize,
    pub outgoing_publish_notices: usize,
    pub outgoing_pub_flush_attempted: usize,
    pub outgoing_puback_waiting: usize,
    pub outgoing_pubrel: usize,
    pub outgoing_pubrel_replay: usize,
    pub outgoing_pubrel_notices: usize,
    pub incoming_puback: usize,
    pub incoming_pub: usize,
    pub incoming_pubrec: usize,
    pub outbound_drained: bool,
}

#[derive(Debug)]
pub struct PendingSubscribe {
    subscribe: Subscribe,
    notice: Option<SubscribeNoticeTx>,
}

#[derive(Debug)]
pub struct PendingUnsubscribe {
    unsubscribe: Unsubscribe,
    notice: Option<UnsubscribeNoticeTx>,
}

#[derive(Debug)]
pub struct CleanRequest {
    pub(crate) request: Request,
    pub(crate) notice: Option<TrackedNoticeTx>,
    pub(crate) replay: bool,
}

#[derive(Debug, Default)]
pub struct IncomingPacketEffects {
    pub(crate) outgoing: Option<Packet>,
    pub(crate) notices: Vec<DeferredNotice>,
}

impl IncomingPacketEffects {
    const fn outgoing(outgoing: Option<Packet>) -> Self {
        Self {
            outgoing,
            notices: Vec::new(),
        }
    }

    fn with_notice(mut self, notice: DeferredNotice) -> Self {
        self.notices.push(notice);
        self
    }

    fn complete_notices(self) -> Option<Packet> {
        for notice in self.notices {
            notice.complete();
        }

        self.outgoing
    }

    #[cfg(test)]
    fn complete_notices_and_has_no_outgoing(self) -> bool {
        self.complete_notices().is_none()
    }

    #[cfg(test)]
    fn unwrap(self) -> Packet {
        self.complete_notices()
            .expect("called `IncomingPacketEffects::unwrap()` with no outgoing packet")
    }
}

/// State of the mqtt connection.
// Design: Methods will just modify the state of the object without doing any network operations
// Design: All inflight queues are maintained in a pre initialized vec with index as packet id.
// This is done for 2 reasons
// Bad acks or out of order acks aren't O(n) causing cpu spikes
// Any missing acks from the broker are detected during the next recycled use of packet ids
#[derive(Debug)]
pub struct MqttState {
    ping: PingState,
    /// Last incoming packet time
    last_incoming: Instant,
    /// Last outgoing packet time
    last_outgoing: Instant,
    /// Packet id of the last outgoing packet
    pub(crate) last_pkid: u16,
    /// Packet id of the last acked publish
    pub(crate) last_puback: u16,
    /// Number of outgoing inflight publishes
    pub(crate) inflight: u16,
    /// Outgoing `QoS` 1, 2 publishes which aren't acked yet
    pub(crate) outgoing_pub: Vec<Option<Publish>>,
    /// Notice handles for outgoing `QoS` 1, 2 publishes
    pub(crate) outgoing_pub_notice: Vec<Option<PublishNoticeTx>>,
    /// Packet ids of outgoing publishes included in a network flush attempt.
    pub(crate) outgoing_pub_flush_attempted: FixedBitSet,
    /// Packet ids acked by broker while waiting to advance last contiguous ack boundary
    pub(crate) outgoing_pub_ack: FixedBitSet,
    /// Packet ids of released `QoS` 2 publishes
    pub(crate) outgoing_rel: FixedBitSet,
    /// Packet ids of released `QoS` 2 publishes queued for replay after clean
    pub(crate) outgoing_rel_replay: FixedBitSet,
    /// Notice handles for outgoing `QoS` 2 pubrels
    pub(crate) outgoing_rel_notice: Vec<Option<PublishNoticeTx>>,
    /// Packet ids on incoming `QoS` 1 publishes waiting for PUBACK
    pub(crate) incoming_puback: FixedBitSet,
    /// Packet ids on incoming `QoS` 2 publishes
    pub(crate) incoming_pub: FixedBitSet,
    /// Packet ids on incoming `QoS` 2 publishes for which PUBREC has been sent
    pub(crate) incoming_pubrec: FixedBitSet,
    /// Packet ids currently reserved by outbound publish, pubrel, subscribe, or unsubscribe flows.
    pub(crate) outbound_pkid_in_use: FixedBitSet,
    /// Number of non-zero packet ids reserved in `outbound_pkid_in_use`.
    pub(crate) outbound_pkid_count: u16,
    /// Last collision due to broker not acking in order
    pub collision: Option<Publish>,
    /// Notice handle for the collision publish
    pub(crate) collision_notice: Option<PublishNoticeTx>,
    /// Subscribe requests waiting for `SubAck`
    pub(crate) pending_subscribe: BTreeMap<u16, PendingSubscribe>,
    /// Unsubscribe requests waiting for `UnsubAck`
    pub(crate) pending_unsubscribe: BTreeMap<u16, PendingUnsubscribe>,
    /// Buffered incoming packets
    pub events: VecDeque<Event>,
    /// Controls how incoming publish acknowledgements are handled.
    pub ack_mode: AckMode,
    topic_aliases: TopicAliasState,
    /// Whether the current network connection has already completed its initial CONNACK.
    connack_received: bool,
    /// Whether the broker supports retained messages on the current network connection.
    retain_available: bool,
    /// Maximum number of allowed inflight `QoS1` & `QoS2` requests
    pub(crate) max_outgoing_inflight: u16,
    /// Upper limit on the maximum number of allowed inflight `QoS1` & `QoS2` requests
    max_outgoing_inflight_upper_limit: u16,
    /// Authentication callback
    authenticator: Option<Arc<Mutex<dyn Authenticator>>>,
    /// Authentication lifecycle state.
    auth: AuthLifecycle,
}

/// Builder for low-level MQTT 5 protocol state.
///
/// Most users should configure clients through [`crate::MqttOptions`] and
/// construct them with [`crate::Client::builder`] or [`crate::AsyncClient::builder`].
/// This builder is intended for users driving [`MqttState`] directly.
#[derive(Debug)]
pub struct MqttStateBuilder {
    max_inflight: u16,
    ack_mode: AckMode,
    auto_topic_alias_policy: TopicAliasPolicy,
    client_topic_alias_max: u16,
    authenticator: Option<Arc<Mutex<dyn Authenticator>>>,
    authentication_method: Option<String>,
}

impl MqttStateBuilder {
    /// Create a new [`MqttState`] builder.
    #[must_use]
    pub const fn new(max_inflight: u16) -> Self {
        Self {
            max_inflight,
            ack_mode: AckMode::Automatic,
            auto_topic_alias_policy: TopicAliasPolicy::Disabled,
            client_topic_alias_max: 0,
            authenticator: None,
            authentication_method: None,
        }
    }

    /// Set how incoming publish acknowledgements are handled.
    #[must_use]
    pub const fn ack_mode(mut self, ack_mode: AckMode) -> Self {
        self.ack_mode = ack_mode;
        self
    }

    /// Set the policy used for outgoing topic alias assignment.
    #[must_use]
    pub const fn topic_alias_policy(mut self, auto_topic_alias_policy: TopicAliasPolicy) -> Self {
        self.auto_topic_alias_policy = auto_topic_alias_policy;
        self
    }

    /// Set the Topic Alias Maximum the client will advertise in CONNECT.
    ///
    /// This value is used to validate incoming topic aliases from the server.
    /// A value of 0 (the default) means the client does not accept any
    /// topic aliases, per [MQTT-3.1.2-27].
    #[must_use]
    pub const fn client_topic_alias_max(mut self, max: u16) -> Self {
        self.client_topic_alias_max = max;
        self
    }

    /// Set the Authentication Method used in the CONNECT packet.
    #[must_use]
    pub fn authentication_method(mut self, authentication_method: Option<String>) -> Self {
        self.authentication_method = authentication_method;
        self
    }

    /// Set the authentication callback used for MQTT 5 enhanced authentication.
    #[must_use]
    pub fn authenticator(mut self, authenticator: Arc<Mutex<dyn Authenticator>>) -> Self {
        self.authenticator = Some(authenticator);
        self
    }

    /// Set the authentication callback used for MQTT 5 enhanced authentication.
    #[must_use]
    pub fn auth_manager(mut self, authenticator: Arc<Mutex<dyn Authenticator>>) -> Self {
        self.authenticator = Some(authenticator);
        self
    }

    /// Build the configured [`MqttState`].
    #[must_use]
    pub fn build(self) -> MqttState {
        MqttState::new_internal(
            self.max_inflight,
            self.ack_mode,
            self.auto_topic_alias_policy,
            self.client_topic_alias_max,
            self.authentication_method,
            self.authenticator,
        )
    }
}

impl MqttState {
    const fn initial_events_capacity() -> usize {
        128
    }

    const fn full_packet_identifier_len() -> usize {
        u16::MAX as usize + 1
    }

    fn outgoing_tracking_len(max_inflight: u16) -> usize {
        usize::from(max_inflight) + 1
    }

    fn new_notice_slots_with_len(size: usize) -> Vec<Option<PublishNoticeTx>> {
        std::iter::repeat_with(|| None).take(size).collect()
    }

    fn new_notice_slots(max_inflight: u16) -> Vec<Option<PublishNoticeTx>> {
        Self::new_notice_slots_with_len(Self::outgoing_tracking_len(max_inflight))
    }

    fn clean_pending_capacity(&self) -> usize {
        self.outgoing_pub
            .iter()
            .filter(|publish| publish.is_some())
            .count()
            + usize::from(self.collision.is_some())
            + self.outgoing_rel.ones().count()
            + self.pending_subscribe.len()
            + self.pending_unsubscribe.len()
    }

    const fn next_publish_pkid_after(&self, pkid: u16) -> u16 {
        if pkid >= self.max_outgoing_inflight {
            1
        } else {
            pkid + 1
        }
    }

    fn packet_identifier_in_use(&self, pkid: u16) -> bool {
        self.outbound_pkid_in_use.contains(usize::from(pkid))
    }

    const fn validate_outgoing_pkid_bound(&self, pkid: u16) -> Result<(), StateError> {
        if pkid == 0 || pkid > self.max_outgoing_inflight {
            return Err(StateError::Unsolicited(pkid));
        }

        Ok(())
    }

    pub(crate) fn can_send_publish(&self, publish: &Publish) -> bool {
        if publish.qos == QoS::AtMostOnce {
            return true;
        }

        if self.inflight >= self.max_outgoing_inflight || self.collision.is_some() {
            return false;
        }

        if publish.pkid == 0 {
            return self.next_publish_pkid().is_some();
        }

        publish.pkid != 0
            && publish.pkid <= self.max_outgoing_inflight
            && !self.packet_identifier_in_use(publish.pkid)
    }

    pub(crate) fn can_send_replayed_publish(&self, publish: &Publish) -> bool {
        if publish.qos == QoS::AtMostOnce {
            return true;
        }

        if self.inflight >= self.max_outgoing_inflight || self.collision.is_some() {
            return false;
        }

        let pkid = publish.pkid;
        self.validate_outgoing_pkid_bound(pkid).is_ok()
            && self.packet_identifier_in_use(pkid)
            && self
                .outgoing_pub
                .get(usize::from(pkid))
                .is_some_and(Option::is_none)
            && !self.outgoing_rel.contains(usize::from(pkid))
            && !self.outgoing_rel_replay.contains(usize::from(pkid))
            && !self.pending_subscribe.contains_key(&pkid)
            && !self.pending_unsubscribe.contains_key(&pkid)
    }

    pub(crate) fn can_send_replayed_control_packet(&self, pkid: u16) -> bool {
        pkid != 0
            && self.packet_identifier_in_use(pkid)
            && !self.outgoing_rel.contains(usize::from(pkid))
            && !self.outgoing_rel_replay.contains(usize::from(pkid))
            && !self.pending_subscribe.contains_key(&pkid)
            && !self.pending_unsubscribe.contains_key(&pkid)
    }

    pub(crate) const fn control_packet_identifier_available(&self) -> bool {
        self.outbound_pkid_count < u16::MAX
    }

    /// Create a builder for low-level MQTT 5 protocol state.
    #[must_use]
    pub const fn builder(max_inflight: u16) -> MqttStateBuilder {
        MqttStateBuilder::new(max_inflight)
    }

    /// Creates new mqtt state. Same state should be used during a
    /// connection for persistent sessions while new state should
    /// instantiated for clean sessions.
    #[must_use]
    pub(crate) fn new_internal(
        max_inflight: u16,
        ack_mode: AckMode,
        auto_topic_alias_policy: TopicAliasPolicy,
        client_topic_alias_max: u16,
        authentication_method: Option<String>,
        authenticator: Option<Arc<Mutex<dyn Authenticator>>>,
    ) -> Self {
        Self {
            ping: PingState::new(),
            last_incoming: Instant::now(),
            last_outgoing: Instant::now(),
            last_pkid: 0,
            last_puback: 0,
            inflight: 0,
            // index 0 is wasted as 0 is not a valid packet id
            outgoing_pub: vec![None; usize::from(max_inflight) + 1],
            outgoing_pub_notice: Self::new_notice_slots(max_inflight),
            outgoing_pub_flush_attempted: FixedBitSet::with_capacity(usize::from(max_inflight) + 1),
            outgoing_pub_ack: FixedBitSet::with_capacity(usize::from(max_inflight) + 1),
            outgoing_rel: FixedBitSet::with_capacity(usize::from(max_inflight) + 1),
            outgoing_rel_replay: FixedBitSet::with_capacity(usize::from(max_inflight) + 1),
            outgoing_rel_notice: Self::new_notice_slots(max_inflight),
            incoming_puback: FixedBitSet::with_capacity(usize::from(u16::MAX) + 1),
            incoming_pub: FixedBitSet::with_capacity(usize::from(u16::MAX) + 1),
            incoming_pubrec: FixedBitSet::with_capacity(usize::from(u16::MAX) + 1),
            outbound_pkid_in_use: FixedBitSet::with_capacity(Self::full_packet_identifier_len()),
            outbound_pkid_count: 0,
            collision: None,
            collision_notice: None,
            pending_subscribe: BTreeMap::new(),
            pending_unsubscribe: BTreeMap::new(),
            events: VecDeque::with_capacity(Self::initial_events_capacity()),
            ack_mode,
            topic_aliases: TopicAliasState::new(auto_topic_alias_policy, client_topic_alias_max),
            connack_received: false,
            retain_available: true,
            max_outgoing_inflight: max_inflight,
            max_outgoing_inflight_upper_limit: max_inflight,
            authenticator,
            auth: AuthLifecycle::new(authentication_method),
        }
    }

    /// Set the Authentication Method used in the CONNECT packet for this state.
    ///
    /// Low-level users that send or process MQTT 5 AUTH packets through
    /// [`MqttState`] must keep this value in sync with the CONNECT packet's
    /// Authentication Method property. The event loop updates it automatically
    /// before each CONNECT attempt.
    pub fn set_authentication_method(&mut self, authentication_method: Option<String>) {
        self.auth.set_method(authentication_method);
    }

    /// Set the Topic Alias Maximum the client will advertise in CONNECT.
    ///
    /// This should be called before each connection attempt to keep the
    /// state in sync with [`crate::MqttOptions::topic_alias_max`].
    pub fn set_client_topic_alias_max(&mut self, max: Option<u16>) {
        self.topic_aliases.client_max = max.unwrap_or(0);
    }

    /// Returns whether the current connection is waiting for a `PINGRESP`.
    pub const fn await_pingresp(&self) -> bool {
        self.ping.await_pingresp()
    }

    /// Returns the number of consecutive collision pings observed.
    pub const fn collision_ping_count(&self) -> usize {
        self.ping.collision_count()
    }

    /// Returns the Topic Alias Maximum received in the current connection's `CONNACK`.
    pub const fn broker_topic_alias_max(&self) -> u16 {
        self.topic_aliases.broker_max
    }

    /// Sets the broker-advertised Topic Alias Maximum for the current connection.
    ///
    /// Low-level callers normally let [`MqttState`] update this from `CONNACK`,
    /// but tests and direct protocol drivers may need to seed it explicitly.
    pub const fn set_broker_topic_alias_max(&mut self, max: u16) {
        self.topic_aliases.broker_max = max;
    }

    pub(crate) fn begin_authentication_connect(
        &mut self,
        authentication_method: Option<String>,
    ) -> Result<Option<crate::mqttbytes::v5::AuthProperties>, StateError> {
        self.auth
            .begin_connect(authentication_method, &mut self.events);
        let Some(method) = self.auth.method().map(str::to_owned) else {
            return Ok(None);
        };
        let Some(authenticator) = self.authenticator.clone() else {
            return Ok(None);
        };
        let context = AuthContext {
            kind: AuthExchangeKind::InitialConnect,
            method: &method,
        };
        let start_result = match authenticator.lock() {
            Ok(mut locked) => locked.start(context),
            Err(poisoned) => {
                drop(poisoned);
                self.fail_auth_exchange_due_to_authenticator_lock_poisoned();
                return Err(StateError::AuthenticatorLockPoisoned);
            }
        };
        let properties = match start_result {
            Ok(properties) => properties,
            Err(err) => return Err(self.fail_authenticator(&err)),
        };
        properties
            .map(|properties| crate::auth::normalize_auth_properties(&method, Some(properties)))
            .transpose()
    }

    pub(crate) fn validate_successful_connack_authentication_method(
        &self,
        connack: &ConnAck,
    ) -> Result<(), StateError> {
        self.auth.validate_successful_connack(connack)
    }

    fn ensure_outgoing_tracking_capacity(&mut self, target_len: usize) {
        if self.outgoing_pub.len() < target_len {
            self.outgoing_pub.resize_with(target_len, || None);
        }

        if self.outgoing_pub_notice.len() < target_len {
            self.outgoing_pub_notice.resize_with(target_len, || None);
        }

        if self.outgoing_pub_flush_attempted.len() < target_len {
            self.outgoing_pub_flush_attempted.grow(target_len);
        }

        if self.outgoing_rel_notice.len() < target_len {
            self.outgoing_rel_notice.resize_with(target_len, || None);
        }

        if self.outgoing_pub_ack.len() < target_len {
            self.outgoing_pub_ack.grow(target_len);
        }

        if self.outgoing_rel.len() < target_len {
            self.outgoing_rel.grow(target_len);
        }

        if self.outgoing_rel_replay.len() < target_len {
            self.outgoing_rel_replay.grow(target_len);
        }
    }

    pub(crate) fn outbound_requests_drained(&self) -> bool {
        self.inflight == 0
            && self.collision.is_none()
            && self.collision_notice.is_none()
            && self.pending_subscribe.is_empty()
            && self.pending_unsubscribe.is_empty()
            && self.outgoing_pub.iter().all(Option::is_none)
            && self.outgoing_pub_notice.iter().all(Option::is_none)
            && self.outgoing_rel_notice.iter().all(Option::is_none)
            && self.outgoing_pub_flush_attempted.ones().next().is_none()
            && self.outgoing_pub_ack.ones().next().is_none()
            && self.outgoing_rel.ones().next().is_none()
            && self.outgoing_rel_replay.ones().next().is_none()
            && self.outbound_pkid_count == 0
    }

    #[cfg_attr(feature = "tracing", allow(dead_code))]
    pub(crate) fn outbound_drain_diagnostics(&self) -> String {
        let diagnostics = self.outbound_diagnostics();
        format!(
            "inflight={}, collision={}, collision_notice={}, pending_subscribe={}, \
             pending_unsubscribe={}, outgoing_pub={}, outgoing_pub_notice={}, \
             outgoing_rel_notice={}, outgoing_pub_flush_attempted={}, outgoing_pub_ack={}, \
             outgoing_rel={}, outgoing_rel_replay={}, incoming_pub={}",
            diagnostics.inflight,
            diagnostics.collision,
            diagnostics.collision_notice,
            diagnostics.pending_subscribe,
            diagnostics.pending_unsubscribe,
            diagnostics.outgoing_publish,
            diagnostics.outgoing_publish_notices,
            diagnostics.outgoing_pubrel_notices,
            diagnostics.outgoing_pub_flush_attempted,
            diagnostics.outgoing_puback_waiting,
            diagnostics.outgoing_pubrel,
            diagnostics.outgoing_pubrel_replay,
            diagnostics.incoming_pub
        )
    }

    fn maybe_shrink_outgoing_tracking_capacity(&mut self, target_len: usize, pending_empty: bool) {
        if !pending_empty
            || self.outgoing_pub.len() <= target_len
            || !self.outbound_requests_drained()
        {
            return;
        }

        self.outgoing_pub.truncate(target_len);
        self.outgoing_pub_notice.truncate(target_len);
        self.outgoing_rel_notice.truncate(target_len);
        self.outgoing_pub_flush_attempted = FixedBitSet::with_capacity(target_len);
        self.outgoing_pub_ack = FixedBitSet::with_capacity(target_len);
        self.outgoing_rel = FixedBitSet::with_capacity(target_len);
        self.outgoing_rel_replay = FixedBitSet::with_capacity(target_len);
        // Ensure future packet id reuse starts from the beginning of the new range.
        self.last_pkid = 0;
        self.last_puback = 0;
    }

    pub(crate) fn reconcile_outgoing_tracking_capacity(&mut self, pending_empty: bool) {
        let target_len = Self::outgoing_tracking_len(self.max_outgoing_inflight);
        self.ensure_outgoing_tracking_capacity(target_len);
        self.maybe_shrink_outgoing_tracking_capacity(target_len, pending_empty);
    }

    pub(crate) fn reset_connection_scoped_state(&mut self) {
        self.topic_aliases.reset_connection_scoped();
        self.connack_received = false;
        self.retain_available = true;
    }

    pub(crate) fn replay_topic_aliases(&self) -> HashMap<u16, Bytes> {
        self.topic_aliases.replay_aliases()
    }

    pub(crate) fn prepare_publish_for_replay_with_aliases(
        publish: &mut Publish,
        topic_aliases: &mut HashMap<u16, Bytes>,
    ) -> Result<(), PublishNoticeError> {
        let Some(alias) = Self::publish_topic_alias(publish) else {
            return Ok(());
        };

        if !publish.topic.is_empty() {
            topic_aliases.insert(alias, publish.topic.clone());
            Self::strip_publish_topic_alias(publish);
            return Ok(());
        }

        if let Some(topic) = topic_aliases.get(&alias) {
            topic.clone_into(&mut publish.topic);
            topic_aliases.insert(alias, publish.topic.clone());
            Self::strip_publish_topic_alias(publish);
            return Ok(());
        }

        Err(PublishNoticeError::TopicAliasReplayUnavailable(alias))
    }

    pub(crate) fn prepare_request_for_replay_with_aliases(
        request: &mut Request,
        topic_aliases: &mut HashMap<u16, Bytes>,
    ) -> Result<(), PublishNoticeError> {
        if let Request::Publish(publish) = request {
            Self::prepare_publish_for_replay_with_aliases(publish, topic_aliases)?;
        }

        Ok(())
    }

    pub(crate) fn clean_with_notices(&mut self) -> Vec<(Request, Option<TrackedNoticeTx>)> {
        self.clean_with_notices_internal(false)
            .into_iter()
            .map(|clean| (clean.request, clean.notice))
            .collect()
    }

    pub(crate) fn clean_with_notices_for_reconnect(&mut self) -> Vec<CleanRequest> {
        self.clean_with_notices_internal(true)
    }

    fn clean_with_notices_internal(
        &mut self,
        keep_replay_pkids_reserved: bool,
    ) -> Vec<CleanRequest> {
        let mut pending = Vec::with_capacity(self.clean_pending_capacity());
        let mut released_publish_pkids = Vec::new();
        let (first_half, second_half) = self
            .outgoing_pub
            .split_at_mut(usize::from(self.last_puback) + 1);
        let (notice_first_half, notice_second_half) = self
            .outgoing_pub_notice
            .split_at_mut(usize::from(self.last_puback) + 1);

        for (publish, notice) in second_half
            .iter_mut()
            .zip(notice_second_half.iter_mut())
            .chain(first_half.iter_mut().zip(notice_first_half.iter_mut()))
        {
            if let Some(mut publish) = publish.take() {
                if publish.qos != QoS::AtMostOnce
                    && self
                        .outgoing_pub_flush_attempted
                        .contains(usize::from(publish.pkid))
                {
                    publish.dup = true;
                }
                if !keep_replay_pkids_reserved {
                    released_publish_pkids.push(publish.pkid);
                }
                let request = Request::Publish(publish);
                pending.push(CleanRequest {
                    request,
                    notice: notice.take().map(TrackedNoticeTx::Publish),
                    replay: true,
                });
            } else {
                _ = notice.take();
            }
        }
        for pkid in released_publish_pkids {
            self.release_outbound_pkid(pkid);
        }

        if let Some(publish) = self.collision.take() {
            pending.push(CleanRequest {
                request: Request::Publish(publish),
                notice: self.collision_notice.take().map(TrackedNoticeTx::Publish),
                replay: false,
            });
        }

        // remove and collect pending releases
        for pkid in self.outgoing_rel.ones() {
            let pkid = u16::try_from(pkid).expect("fixedbitset index always fits in u16");
            let request = Request::PubRel(PubRel::new(pkid, None));
            pending.push(CleanRequest {
                request,
                notice: self.outgoing_rel_notice[usize::from(pkid)]
                    .take()
                    .map(TrackedNoticeTx::Publish),
                replay: true,
            });
        }
        self.outgoing_rel_replay = self.outgoing_rel.clone();
        self.outgoing_rel.clear();
        self.outgoing_pub_flush_attempted.clear();
        self.outgoing_pub_ack.clear();

        for (pkid, mut pending_subscribe) in std::mem::take(&mut self.pending_subscribe) {
            pending_subscribe.subscribe.pkid = pkid;
            if !keep_replay_pkids_reserved {
                self.release_outbound_pkid(pkid);
            }
            pending.push(CleanRequest {
                request: Request::Subscribe(pending_subscribe.subscribe),
                notice: pending_subscribe.notice.map(TrackedNoticeTx::Subscribe),
                replay: true,
            });
        }
        for (pkid, mut pending_unsubscribe) in std::mem::take(&mut self.pending_unsubscribe) {
            pending_unsubscribe.unsubscribe.pkid = pkid;
            if !keep_replay_pkids_reserved {
                self.release_outbound_pkid(pkid);
            }
            pending.push(CleanRequest {
                request: Request::Unsubscribe(pending_unsubscribe.unsubscribe),
                notice: pending_unsubscribe.notice.map(TrackedNoticeTx::Unsubscribe),
                replay: true,
            });
        }

        // QoS 1 receives are not part of the client session state. Incomplete QoS 2
        // receives are, so keep incoming_pub/incoming_pubrec for persistent-session reconnects.
        self.incoming_puback.clear();

        self.ping.reset();
        self.inflight = 0;
        pending
    }

    /// Marks the current in-flight outgoing `QoS` 1 and `QoS` 2 publishes as
    /// attempted on the network.
    ///
    /// Direct [`MqttState`] users should call this after handing pending
    /// publishes to their transport for a network flush. On a later
    /// [`clean`](Self::clean), marked unacknowledged publishes are replayed
    /// with `DUP=1`, as required when a client cannot know whether the broker
    /// received the original publish before the connection was lost.
    pub fn mark_outgoing_publishes_flush_attempted(&mut self) {
        for (pkid, publish) in self.outgoing_pub.iter().enumerate() {
            if publish.is_some() {
                self.outgoing_pub_flush_attempted.set(pkid, true);
            }
        }
    }

    /// Returns inflight outgoing packets and clears internal queues.
    ///
    /// MQTT 5 topic aliases are scoped to a single network connection. During
    /// cleanup, replayed publishes that only contain a topic alias are repaired
    /// with the remembered topic when possible. If the topic cannot be
    /// recovered, the publish is omitted because replaying it on a new
    /// connection would be protocol-invalid.
    pub fn clean(&mut self) -> Vec<Request> {
        let mut replay_topic_aliases = self.replay_topic_aliases();
        let mut pending = Vec::with_capacity(self.clean_pending_capacity());

        for (mut request, _) in self.clean_with_notices() {
            if Self::prepare_request_for_replay_with_aliases(
                &mut request,
                &mut replay_topic_aliases,
            )
            .is_ok()
            {
                pending.push(request);
            }
        }

        self.reset_connection_scoped_state();
        pending
    }

    pub const fn inflight(&self) -> u16 {
        self.inflight
    }

    /// Returns an observation-only snapshot of outbound protocol state.
    pub fn outbound_diagnostics(&self) -> OutboundDiagnostics {
        OutboundDiagnostics {
            inflight: self.inflight,
            max_inflight: self.max_outgoing_inflight,
            publish_window_full: self.inflight >= self.max_outgoing_inflight,
            packet_identifiers_in_use: (1..=u16::MAX)
                .filter(|&pkid| self.packet_identifier_in_use(pkid))
                .count(),
            collision: self.collision.is_some(),
            collision_notice: self.collision_notice.is_some(),
            pending_subscribe: self.pending_subscribe.len(),
            pending_unsubscribe: self.pending_unsubscribe.len(),
            outgoing_publish: self
                .outgoing_pub
                .iter()
                .filter(|publish| publish.is_some())
                .count(),
            outgoing_publish_notices: self
                .outgoing_pub_notice
                .iter()
                .filter(|notice| notice.is_some())
                .count(),
            outgoing_pub_flush_attempted: self.outgoing_pub_flush_attempted.ones().count(),
            outgoing_puback_waiting: self.outgoing_pub_ack.ones().count(),
            outgoing_pubrel: self.outgoing_rel.ones().count(),
            outgoing_pubrel_replay: self.outgoing_rel_replay.ones().count(),
            outgoing_pubrel_notices: self
                .outgoing_rel_notice
                .iter()
                .filter(|notice| notice.is_some())
                .count(),
            incoming_puback: self.incoming_puback.ones().count(),
            incoming_pub: self.incoming_pub.ones().count(),
            incoming_pubrec: self.incoming_pubrec.ones().count(),
            outbound_drained: self.outbound_requests_drained(),
        }
    }

    pub(crate) const fn retain_available(&self) -> bool {
        self.retain_available
    }

    /// Number of SUBSCRIBE requests waiting for a SUBACK.
    ///
    /// This includes requests with and without a completion notice.
    pub fn pending_subscribe_len(&self) -> usize {
        self.pending_subscribe.len()
    }

    /// Number of UNSUBSCRIBE requests waiting for an UNSUBACK.
    ///
    /// This includes requests with and without a completion notice.
    pub fn pending_unsubscribe_len(&self) -> usize {
        self.pending_unsubscribe.len()
    }

    /// Returns whether there are no SUBSCRIBE or UNSUBSCRIBE requests waiting for acknowledgement.
    pub fn pending_control_requests_is_empty(&self) -> bool {
        self.pending_subscribe.is_empty() && self.pending_unsubscribe.is_empty()
    }

    /// Number of notice-backed SUBSCRIBE requests waiting for a SUBACK.
    pub fn tracked_subscribe_len(&self) -> usize {
        self.pending_subscribe
            .values()
            .filter(|pending| pending.notice.is_some())
            .count()
    }

    /// Number of notice-backed UNSUBSCRIBE requests waiting for an UNSUBACK.
    pub fn tracked_unsubscribe_len(&self) -> usize {
        self.pending_unsubscribe
            .values()
            .filter(|pending| pending.notice.is_some())
            .count()
    }

    /// Returns whether there are no notice-backed SUBSCRIBE or UNSUBSCRIBE requests.
    pub fn tracked_requests_is_empty(&self) -> bool {
        self.tracked_subscribe_len() == 0 && self.tracked_unsubscribe_len() == 0
    }

    /// Clears all pending SUBSCRIBE and UNSUBSCRIBE protocol requests.
    ///
    /// This removes both notice-backed and notice-less control requests. Notice-backed requests are
    /// failed with `reason`; notice-less requests are dropped from protocol tracking only. Returns
    /// the total number of pending control requests removed.
    pub fn drain_tracked_requests_as_failed(&mut self, reason: NoticeFailureReason) -> usize {
        let drained = self.pending_subscribe.len() + self.pending_unsubscribe.len();
        for (pkid, pending_subscribe) in std::mem::take(&mut self.pending_subscribe) {
            self.release_outbound_pkid(pkid);
            if let Some(notice) = pending_subscribe.notice {
                notice.error(reason.subscribe_error());
            }
        }
        for (pkid, pending_unsubscribe) in std::mem::take(&mut self.pending_unsubscribe) {
            self.release_outbound_pkid(pkid);
            if let Some(notice) = pending_unsubscribe.notice {
                notice.error(reason.unsubscribe_error());
            }
        }

        drained
    }

    pub(crate) fn fail_pending_notices(&mut self) {
        for notice in &mut self.outgoing_pub_notice {
            if let Some(tx) = notice.take() {
                tx.error(PublishNoticeError::SessionReset);
            }
        }

        for notice in &mut self.outgoing_rel_notice {
            if let Some(tx) = notice.take() {
                tx.error(PublishNoticeError::SessionReset);
            }
        }

        if let Some(tx) = self.collision_notice.take() {
            tx.error(PublishNoticeError::SessionReset);
        }
        self.drain_tracked_requests_as_failed(NoticeFailureReason::SessionReset);
        self.clear_collision();
    }

    pub(crate) fn reset_session_state(&mut self) {
        self.fail_pending_notices();

        let events = std::mem::take(&mut self.events);
        let auth = self.auth.clone();
        let ack_mode = self.ack_mode;
        let auto_topic_alias_policy = self.topic_aliases.auto_policy;
        let client_topic_alias_max = self.topic_aliases.client_max;
        let authentication_method = auth.method().map(str::to_owned);
        let authenticator = self.authenticator.clone();
        let max_outgoing_inflight = self.max_outgoing_inflight_upper_limit;

        let mut reset = Self::new_internal(
            max_outgoing_inflight,
            ack_mode,
            auto_topic_alias_policy,
            client_topic_alias_max,
            authentication_method,
            authenticator,
        );
        reset.events = events;
        reset.auth = auth;
        *self = reset;
    }

    pub(crate) fn fail_auth_exchange_due_to_session_reset(&mut self) {
        self.fail_auth_exchange(
            AuthNoticeError::SessionReset,
            AuthError::Failed("authentication exchange was reset with the session".to_owned()),
        );
    }

    pub(crate) fn fail_reauth_exchange_due_to_session_reset(&mut self) {
        let Some((method, notice_error)) = self
            .auth
            .reset_reauth(AuthNoticeError::SessionReset, &mut self.events)
        else {
            return;
        };

        if let Some(authenticator) = self.authenticator.clone() {
            match authenticator.lock() {
                Ok(mut locked) => locked.failure(
                    AuthContext {
                        kind: AuthExchangeKind::Reauthentication,
                        method: &method,
                    },
                    AuthError::Failed(notice_error.to_string()),
                ),
                Err(_) => debug!(
                    "authenticator lock poisoned while failing reauth exchange due to session reset"
                ),
            }
        }
    }

    pub(crate) fn fail_auth_exchange_due_to_connection_closed(&mut self) {
        self.fail_auth_exchange(
            AuthNoticeError::ConnectionClosed,
            AuthError::Failed("connection closed before authentication completed".to_owned()),
        );
    }

    pub(crate) fn fail_auth_exchange_due_to_client_disconnect(&mut self) {
        self.fail_auth_exchange(
            AuthNoticeError::ConnectionClosed,
            AuthError::Failed("authentication aborted by client disconnect".to_owned()),
        );
    }

    fn fail_auth_exchange_due_to_authenticator_lock_poisoned(&mut self) {
        let message = StateError::AuthenticatorLockPoisoned.to_string();
        self.fail_auth_exchange(
            AuthNoticeError::AuthenticationFailed(message.clone()),
            AuthError::Failed(message),
        );
    }

    /// Consolidates handling of all outgoing mqtt packet logic. Returns a packet which should
    /// be put on to the network by the eventloop
    ///
    /// # Errors
    ///
    /// Returns an error if the outgoing request is invalid for the current
    /// client state.
    pub fn handle_outgoing_packet(
        &mut self,
        request: Request,
    ) -> Result<Option<Packet>, StateError> {
        let (packet, flush_notice) = self.handle_outgoing_packet_with_notice(request, None)?;
        if let Some(tx) = flush_notice {
            tx.success(PublishResult::Qos0Flushed);
        }
        Ok(packet)
    }

    pub(crate) fn handle_outgoing_packet_with_notice(
        &mut self,
        request: Request,
        notice: Option<TrackedNoticeTx>,
    ) -> Result<(Option<Packet>, Option<PublishNoticeTx>), StateError> {
        self.handle_outgoing_packet_with_notice_internal(request, notice, false)
    }

    pub(crate) fn handle_replayed_outgoing_packet_with_notice(
        &mut self,
        request: Request,
        notice: Option<TrackedNoticeTx>,
    ) -> Result<(Option<Packet>, Option<PublishNoticeTx>), StateError> {
        self.handle_outgoing_packet_with_notice_internal(request, notice, true)
    }

    fn handle_outgoing_packet_with_notice_internal(
        &mut self,
        request: Request,
        notice: Option<TrackedNoticeTx>,
        replay: bool,
    ) -> Result<(Option<Packet>, Option<PublishNoticeTx>), StateError> {
        fn unsupported_outgoing_request(
            request: Request,
        ) -> Result<(Option<Packet>, Option<PublishNoticeTx>), StateError> {
            let _request = request;
            Err(StateError::InvalidState)
        }

        let result = match request {
            Request::Publish(publish) => {
                let publish_notice = match notice {
                    Some(TrackedNoticeTx::Publish(notice)) => Some(notice),
                    Some(
                        TrackedNoticeTx::Subscribe(_)
                        | TrackedNoticeTx::Unsubscribe(_)
                        | TrackedNoticeTx::Auth(_),
                    )
                    | None => None,
                };
                if replay {
                    self.replay_outgoing_publish_with_notice(publish, publish_notice)?
                } else {
                    self.outgoing_publish_with_notice(publish, publish_notice)?
                }
            }
            Request::PubRel(pubrel) => {
                let publish_notice = match notice {
                    Some(TrackedNoticeTx::Publish(notice)) => Some(notice),
                    Some(
                        TrackedNoticeTx::Subscribe(_)
                        | TrackedNoticeTx::Unsubscribe(_)
                        | TrackedNoticeTx::Auth(_),
                    )
                    | None => None,
                };
                self.outgoing_pubrel_with_notice(pubrel, publish_notice)?
            }
            Request::Subscribe(subscribe) => {
                let request_notice = match notice {
                    Some(TrackedNoticeTx::Subscribe(notice)) => Some(notice),
                    Some(
                        TrackedNoticeTx::Publish(_)
                        | TrackedNoticeTx::Unsubscribe(_)
                        | TrackedNoticeTx::Auth(_),
                    )
                    | None => None,
                };
                let packet = if replay {
                    self.replay_outgoing_subscribe(subscribe, request_notice)?
                } else {
                    self.outgoing_subscribe(subscribe, request_notice)?
                };
                (packet, None)
            }
            Request::Unsubscribe(unsubscribe) => {
                let request_notice = match notice {
                    Some(TrackedNoticeTx::Unsubscribe(notice)) => Some(notice),
                    Some(
                        TrackedNoticeTx::Publish(_)
                        | TrackedNoticeTx::Subscribe(_)
                        | TrackedNoticeTx::Auth(_),
                    )
                    | None => None,
                };
                let packet = if replay {
                    self.replay_outgoing_unsubscribe(unsubscribe, request_notice)?
                } else {
                    self.outgoing_unsubscribe(unsubscribe, request_notice)?
                };
                (Some(packet), None)
            }
            Request::PingReq => (self.outgoing_ping()?, None),
            Request::Disconnect(_) | Request::DisconnectWithTimeout(_, _) => {
                unreachable!("graceful disconnect requests are handled by the event loop")
            }
            Request::DisconnectNow(disconnect) => {
                (Some(self.outgoing_disconnect(disconnect)), None)
            }
            Request::PubAck(puback) => (Some(self.outgoing_puback(puback)?), None),
            Request::PubRec(pubrec) => (Some(self.outgoing_pubrec(pubrec)?), None),
            Request::Auth(auth) => {
                let auth_notice = match notice {
                    Some(TrackedNoticeTx::Auth(notice)) => Some(notice),
                    Some(
                        TrackedNoticeTx::Publish(_)
                        | TrackedNoticeTx::Subscribe(_)
                        | TrackedNoticeTx::Unsubscribe(_),
                    )
                    | None => None,
                };
                (Some(self.outgoing_auth(auth, auth_notice)?), None)
            }
            Request::PubComp(_) | Request::PingResp | Request::SubAck(_) | Request::UnsubAck(_) => {
                unsupported_outgoing_request(request)?
            }
        };

        self.last_outgoing = Instant::now();
        Ok(result)
    }

    /// Consolidates handling of all incoming mqtt packets. Returns a `Notification` which for the
    /// user to consume and `Packet` which for the eventloop to put on the network
    /// E.g For incoming `QoS1` publish packet, this method returns (Publish, Puback). Publish packet will
    /// be forwarded to user and Pubck packet will be written to network
    ///
    /// # Errors
    ///
    /// Returns an error if the incoming packet is invalid for the current
    /// client state.
    pub fn handle_incoming_packet(
        &mut self,
        packet: Incoming,
    ) -> Result<Option<Packet>, StateError> {
        Ok(self
            .handle_incoming_packet_with_effects(packet)?
            .complete_notices())
    }

    pub(crate) fn handle_incoming_packet_with_effects(
        &mut self,
        mut packet: Incoming,
    ) -> Result<IncomingPacketEffects, StateError> {
        let events_len_before = self.events.len();
        let is_duplicate_incoming_qos2_publish = self.is_duplicate_incoming_qos2_publish(&packet);
        let effects = match &mut packet {
            Incoming::PingResp => Ok(IncomingPacketEffects::outgoing(
                self.handle_incoming_pingresp(),
            )),
            Incoming::Publish(publish) => self
                .handle_incoming_publish(publish)
                .map(IncomingPacketEffects::outgoing),
            Incoming::SubAck(suback) => self.handle_incoming_suback(suback),
            Incoming::UnsubAck(unsuback) => self.handle_incoming_unsuback(unsuback),
            Incoming::PubAck(puback) => self.handle_incoming_puback(puback),
            Incoming::PubRec(pubrec) => self.handle_incoming_pubrec(pubrec),
            Incoming::PubRel(pubrel) => self
                .handle_incoming_pubrel(pubrel)
                .map(IncomingPacketEffects::outgoing),
            Incoming::PubComp(pubcomp) => self.handle_incoming_pubcomp(pubcomp),
            Incoming::ConnAck(connack) => self
                .handle_incoming_connack(connack)
                .map(IncomingPacketEffects::outgoing),
            Incoming::Disconnect(disconn) => {
                Self::handle_incoming_disconn(disconn).map(IncomingPacketEffects::outgoing)
            }
            Incoming::Auth(auth) => self
                .handle_incoming_auth(auth)
                .map(IncomingPacketEffects::outgoing),
            _ => {
                #[cfg(not(feature = "tracing"))]
                error!("Invalid incoming packet = {packet:?}");
                Err(StateError::ProtocolViolation(
                    ProtocolViolation::UnexpectedIncomingPacket(packet.packet_type()),
                ))
            }
        };

        #[cfg(feature = "tracing")]
        if let Err(StateError::ProtocolViolation(violation)) = &effects {
            crate::instrumentation::protocol_violation(violation);
        }

        let skip_incoming_event = matches!(
            (&packet, &effects),
            (
                Incoming::Publish(_),
                Ok(IncomingPacketEffects {
                    outgoing: Some(Packet::Disconnect(_)),
                    ..
                })
            )
        ) || is_duplicate_incoming_qos2_publish;

        let queue_incoming_event_on_error = matches!(
            (&packet, &effects),
            (Incoming::ConnAck(_), Err(StateError::ConnFail { .. }))
                | (
                    Incoming::Disconnect(_),
                    Err(StateError::ServerDisconnect { .. })
                )
        );

        // Preserve original event ordering (Incoming first, derived Outgoing next)
        // without cloning the incoming packet.
        match effects {
            Ok(effects) => {
                if !skip_incoming_event {
                    self.events
                        .insert(events_len_before, Event::Incoming(packet));
                }
                self.last_incoming = Instant::now();
                Ok(effects)
            }
            Err(err) => {
                if queue_incoming_event_on_error {
                    self.events
                        .insert(events_len_before, Event::Incoming(packet));
                }
                Err(err)
            }
        }
    }

    fn is_duplicate_incoming_qos2_publish(&self, packet: &Incoming) -> bool {
        matches!(
            packet,
            Incoming::Publish(publish)
                if publish.qos == QoS::ExactlyOnce
                    && self.incoming_pubrec.contains(usize::from(publish.pkid))
        )
    }

    /// Builds the protocol-error disconnect response for the current state.
    ///
    /// # Errors
    ///
    /// Returns an error if the disconnect packet cannot be produced for the
    /// current state.
    pub fn handle_protocol_error(&mut self) -> Result<Option<Packet>, StateError> {
        // send DISCONNECT packet with REASON_CODE 0x82
        let disconnect = Disconnect::new(DisconnectReasonCode::ProtocolError);
        Ok(Some(self.outgoing_disconnect(disconnect)))
    }

    pub fn clear_collision(&mut self) {
        self.collision = None;
        self.collision_notice = None;
        self.ping.reset_collision_count();
    }

    fn handle_incoming_suback(
        &mut self,
        suback: &SubAck,
    ) -> Result<IncomingPacketEffects, StateError> {
        let Some(pending_subscribe) = self.pending_subscribe.get(&suback.pkid) else {
            error!("Unsolicited suback packet: {:?}", suback.pkid);
            return Err(StateError::Unsolicited(suback.pkid));
        };

        let expected = pending_subscribe.subscribe.filters.len();
        let actual = suback.return_codes.len();
        if actual != expected {
            return Err(StateError::ProtocolViolation(
                ProtocolViolation::SubAckReturnCodeCountMismatch {
                    pkid: suback.pkid,
                    expected,
                    actual,
                },
            ));
        }

        for reason in &suback.return_codes {
            match reason {
                SubscribeReasonCode::Success(qos) => {
                    debug!("SubAck Pkid = {:?}, QoS = {:?}", suback.pkid, qos);
                }
                _ => {
                    warn!("SubAck Pkid = {:?}, Reason = {:?}", suback.pkid, reason);
                }
            }
        }

        let pending_subscribe = self
            .pending_subscribe
            .remove(&suback.pkid)
            .expect("tracked subscribe checked before SUBACK validation");
        self.mark_control_packet_id_complete(suback.pkid);
        self.release_outbound_pkid(suback.pkid);
        let mut effects = IncomingPacketEffects::outgoing(None);
        if let Some(notice) = pending_subscribe.notice {
            effects = effects.with_notice(DeferredNotice::Subscribe(notice, suback.clone()));
        }
        Ok(effects)
    }

    fn handle_incoming_unsuback(
        &mut self,
        unsuback: &UnsubAck,
    ) -> Result<IncomingPacketEffects, StateError> {
        let Some(pending_unsubscribe) = self.pending_unsubscribe.get(&unsuback.pkid) else {
            error!("Unsolicited unsuback packet: {:?}", unsuback.pkid);
            return Err(StateError::Unsolicited(unsuback.pkid));
        };

        let expected = pending_unsubscribe.unsubscribe.filters.len();
        let actual = unsuback.reasons.len();
        if actual != expected {
            return Err(StateError::ProtocolViolation(
                ProtocolViolation::UnsubAckReasonCodeCountMismatch {
                    pkid: unsuback.pkid,
                    expected,
                    actual,
                },
            ));
        }

        for reason in &unsuback.reasons {
            if reason != &UnsubAckReason::Success {
                warn!("UnsubAck Pkid = {:?}, Reason = {:?}", unsuback.pkid, reason);
            }
        }

        let pending_unsubscribe = self
            .pending_unsubscribe
            .remove(&unsuback.pkid)
            .expect("tracked unsubscribe checked before UNSUBACK validation");
        self.mark_control_packet_id_complete(unsuback.pkid);
        self.release_outbound_pkid(unsuback.pkid);
        let mut effects = IncomingPacketEffects::outgoing(None);
        if let Some(notice) = pending_unsubscribe.notice {
            effects = effects.with_notice(DeferredNotice::Unsubscribe(notice, unsuback.clone()));
        }
        Ok(effects)
    }

    fn handle_incoming_connack(&mut self, connack: &ConnAck) -> Result<Option<Packet>, StateError> {
        if self.connack_received {
            return Err(StateError::ProtocolViolation(
                ProtocolViolation::DuplicateConnAck,
            ));
        }

        if connack.code != ConnectReturnCode::Success {
            return Err(StateError::ConnFail {
                reason: connack.code,
            });
        }

        self.auth.validate_successful_connack(connack)?;
        self.reset_connection_scoped_state();
        self.auth.complete_initial_connack(&mut self.events);

        if let Some(props) = &connack.properties
            && let Some(topic_alias_max) = props.topic_alias_max
        {
            self.topic_aliases.broker_max = topic_alias_max;
        }

        if let Some(props) = &connack.properties
            && let Some(max_inflight) = props.receive_max
        {
            self.max_outgoing_inflight = max_inflight.min(self.max_outgoing_inflight_upper_limit);
            // Shrinking depends on pending retransmission state in eventloop.
            // Grow immediately so incoming/outgoing packet-id indexed tracking stays valid.
            self.reconcile_outgoing_tracking_capacity(false);
        }
        if let Some(props) = &connack.properties
            && let Some(retain_available) = props.retain_available
        {
            self.retain_available = retain_available == 1;
        }
        self.connack_received = true;
        Ok(None)
    }

    fn handle_incoming_disconn(disconn: &Disconnect) -> Result<Option<Packet>, StateError> {
        let reason_code = disconn.reason_code;
        let reason_string = disconn
            .properties
            .as_ref()
            .and_then(|props| props.reason_string.clone());
        Err(StateError::ServerDisconnect {
            reason_code,
            reason_string,
        })
    }

    /// Results in a publish notification in all the `QoS` cases. Replys with an ack
    /// in case of `QoS1` and Replys rec in case of `QoS` while also storing the message
    fn handle_incoming_publish(
        &mut self,
        publish: &mut Publish,
    ) -> Result<Option<Packet>, StateError> {
        let qos = publish.qos;

        let topic_alias = publish
            .properties
            .as_ref()
            .and_then(|props| props.topic_alias);

        // [MQTT-3.1.2-26] The Server MUST NOT send a Topic Alias greater
        // than the Topic Alias Maximum the Client advertised in CONNECT.
        // [MQTT-3.1.2-27] If Topic Alias Maximum is absent or zero, the
        // Server MUST NOT send any Topic Aliases to the Client.
        if let Some(alias) = topic_alias
            && alias > self.topic_aliases.client_max
        {
            let disconnect = Disconnect::new(DisconnectReasonCode::TopicAliasInvalid);
            return Ok(Some(self.outgoing_disconnect(disconnect)));
        }

        if !publish.topic.is_empty() {
            if let Some(alias) = topic_alias {
                self.topic_aliases
                    .incoming
                    .insert(alias, publish.topic.clone());
            }
        } else if let Some(alias) = topic_alias
            && let Some(topic) = self.topic_aliases.incoming.get(&alias)
        {
            topic.clone_into(&mut publish.topic);
        } else if topic_alias.is_some() {
            return self.handle_protocol_error();
        }

        match qos {
            QoS::AtMostOnce => Ok(None),
            QoS::AtLeastOnce => {
                let pkid = publish.pkid;
                self.incoming_puback.insert(usize::from(pkid));

                if self.ack_mode == AckMode::Automatic {
                    let puback = PubAck::new(pkid, None);
                    return Ok(Some(self.outgoing_puback(puback)?));
                }
                Ok(None)
            }
            QoS::ExactlyOnce => {
                let pkid = publish.pkid;
                self.incoming_pub.insert(usize::from(pkid));

                if self.ack_mode == AckMode::Automatic
                    || self.incoming_pubrec.contains(usize::from(pkid))
                {
                    let pubrec = PubRec::new(pkid, None);
                    return Ok(Some(self.outgoing_pubrec_for_incoming_publish(pubrec)?));
                }
                Ok(None)
            }
        }
    }

    fn handle_incoming_puback(
        &mut self,
        puback: &PubAck,
    ) -> Result<IncomingPacketEffects, StateError> {
        let publish = self
            .outgoing_pub
            .get_mut(usize::from(puback.pkid))
            .ok_or(StateError::Unsolicited(puback.pkid))?;

        if publish.take().is_none() {
            error!("Unsolicited puback packet: {:?}", puback.pkid);
            return Err(StateError::Unsolicited(puback.pkid));
        }
        self.outgoing_pub_flush_attempted
            .set(usize::from(puback.pkid), false);
        self.mark_outgoing_packet_id_complete(puback.pkid);
        self.release_outbound_pkid(puback.pkid);

        let notice = self.outgoing_pub_notice[usize::from(puback.pkid)].take();
        self.inflight -= 1;

        if puback.reason != PubAckReason::Success
            && puback.reason != PubAckReason::NoMatchingSubscribers
        {
            warn!(
                "PubAck Pkid = {:?}, reason: {:?}",
                puback.pkid, puback.reason
            );
        }
        let mut effects =
            IncomingPacketEffects::outgoing(self.replay_collision_publish(puback.pkid));
        if let Some(tx) = notice {
            effects = effects.with_notice(DeferredNotice::Publish(
                tx,
                PublishResult::Qos1(puback.clone()),
            ));
        }

        Ok(effects)
    }

    fn handle_incoming_pubrec(
        &mut self,
        pubrec: &PubRec,
    ) -> Result<IncomingPacketEffects, StateError> {
        let publish = self
            .outgoing_pub
            .get_mut(usize::from(pubrec.pkid))
            .ok_or(StateError::Unsolicited(pubrec.pkid))?;

        if publish.take().is_none() {
            error!("Unsolicited pubrec packet: {:?}", pubrec.pkid);
            return Err(StateError::Unsolicited(pubrec.pkid));
        }
        self.outgoing_pub_flush_attempted
            .set(usize::from(pubrec.pkid), false);

        let notice = self.outgoing_pub_notice[usize::from(pubrec.pkid)].take();
        if pubrec.reason != PubRecReason::Success
            && pubrec.reason != PubRecReason::NoMatchingSubscribers
        {
            warn!(
                "PubRec Pkid = {:?}, reason: {:?}",
                pubrec.pkid, pubrec.reason
            );
            self.mark_outgoing_packet_id_complete(pubrec.pkid);
            self.release_outbound_pkid(pubrec.pkid);
            self.inflight -= 1;
            let mut effects =
                IncomingPacketEffects::outgoing(self.replay_collision_publish(pubrec.pkid));
            if let Some(tx) = notice {
                effects = effects.with_notice(DeferredNotice::Publish(
                    tx,
                    PublishResult::Qos2PubRecRejected(pubrec.clone()),
                ));
            }
            return Ok(effects);
        }

        // NOTE: Inflight - 1 for qos2 in comp
        self.outgoing_rel.insert(usize::from(pubrec.pkid));
        self.outgoing_rel_notice[usize::from(pubrec.pkid)] = notice;
        let event = Event::Outgoing(Outgoing::PubRel(pubrec.pkid));
        self.events.push_back(event);

        Ok(IncomingPacketEffects::outgoing(Some(Packet::PubRel(
            PubRel::new(pubrec.pkid, None),
        ))))
    }

    fn handle_incoming_pubrel(&mut self, pubrel: &PubRel) -> Result<Option<Packet>, StateError> {
        if !self.incoming_pub.contains(usize::from(pubrel.pkid)) {
            warn!(
                "Untracked pubrel packet: {:?}. Sending pubcomp with packet identifier not found",
                pubrel.pkid
            );
            let event = Event::Outgoing(Outgoing::PubComp(pubrel.pkid));
            let mut pubcomp = PubComp::new(pubrel.pkid, None);
            pubcomp.reason = PubCompReason::PacketIdentifierNotFound;
            self.events.push_back(event);
            return Ok(Some(Packet::PubComp(pubcomp)));
        }
        if !self.incoming_pubrec.contains(usize::from(pubrel.pkid)) {
            error!(
                "Pubrel packet received before pubrec was sent: {:?}",
                pubrel.pkid
            );
            return Err(StateError::Unsolicited(pubrel.pkid));
        }
        self.incoming_pub.set(usize::from(pubrel.pkid), false);
        self.incoming_pubrec.set(usize::from(pubrel.pkid), false);

        if pubrel.reason != PubRelReason::Success {
            warn!(
                "PubRel Pkid = {:?}, reason: {:?}",
                pubrel.pkid, pubrel.reason
            );
            return Ok(None);
        }

        let event = Event::Outgoing(Outgoing::PubComp(pubrel.pkid));
        self.events.push_back(event);

        Ok(Some(Packet::PubComp(PubComp::new(pubrel.pkid, None))))
    }

    fn handle_incoming_pubcomp(
        &mut self,
        pubcomp: &PubComp,
    ) -> Result<IncomingPacketEffects, StateError> {
        if !self.outgoing_rel.contains(usize::from(pubcomp.pkid)) {
            error!("Unsolicited pubcomp packet: {:?}", pubcomp.pkid);
            return Err(StateError::Unsolicited(pubcomp.pkid));
        }
        self.outgoing_rel.set(usize::from(pubcomp.pkid), false);
        let notice = self.outgoing_rel_notice[usize::from(pubcomp.pkid)].take();
        self.mark_outgoing_packet_id_complete(pubcomp.pkid);
        self.release_outbound_pkid(pubcomp.pkid);
        self.inflight -= 1;

        if pubcomp.reason != PubCompReason::Success {
            warn!(
                "PubComp Pkid = {:?}, reason: {:?}",
                pubcomp.pkid, pubcomp.reason
            );
        }
        let mut effects =
            IncomingPacketEffects::outgoing(self.replay_collision_publish(pubcomp.pkid));
        if let Some(tx) = notice {
            effects = effects.with_notice(DeferredNotice::Publish(
                tx,
                PublishResult::Qos2Completed(pubcomp.clone()),
            ));
        }

        Ok(effects)
    }

    fn replay_collision_publish(&mut self, pkid: u16) -> Option<Packet> {
        self.check_collision(pkid).map(|(publish, notice)| {
            let pkid = publish.pkid;
            let replay_publish = self.publish_for_replay_tracking(&publish);
            self.reserve_outbound_pkid(pkid)
                .expect("collision replay packet identifier should have been released");
            self.outgoing_pub[usize::from(pkid)] = Some(replay_publish);
            self.outgoing_pub_notice[usize::from(pkid)] = notice;
            self.outgoing_pub_flush_attempted
                .set(usize::from(pkid), false);
            self.inflight += 1;
            self.record_outgoing_topic_alias(&publish);

            let event = Event::Outgoing(Outgoing::Publish(pkid));
            self.events.push_back(event);
            self.ping.reset_collision_count();

            Packet::Publish(publish)
        })
    }

    const fn handle_incoming_pingresp(&mut self) -> Option<Packet> {
        self.ping.finish_waiting_for_resp();
        None
    }

    fn handle_incoming_auth(&mut self, auth: &Auth) -> Result<Option<Packet>, StateError> {
        let effect = match self.auth.incoming_auth(auth, &mut self.events) {
            Ok(effect) => effect,
            Err(err @ StateError::Deserialization(mqttbytes::Error::ProtocolError)) => {
                self.fail_auth_exchange(
                    AuthNoticeError::ProtocolError,
                    AuthError::Failed("authentication protocol error".to_owned()),
                );
                return Err(err);
            }
            Err(err) => return Err(err),
        };

        match effect {
            IncomingAuthEffect::Success { kind, method } => {
                if let Some(authenticator) = self.authenticator.clone() {
                    let context = AuthContext {
                        kind,
                        method: &method,
                    };
                    let auth_result = match authenticator.lock() {
                        Ok(mut locked) => locked.success(context, auth.properties.clone()),
                        Err(poisoned) => {
                            drop(poisoned);
                            self.fail_auth_exchange_due_to_authenticator_lock_poisoned();
                            return Err(StateError::AuthenticatorLockPoisoned);
                        }
                    };
                    if let Err(err) = auth_result {
                        return Err(self.fail_authenticator(&err));
                    }
                }
                self.auth.complete_success(kind, method, &mut self.events);
                Ok(None)
            }
            IncomingAuthEffect::Continue { kind } => {
                let authenticator = self
                    .authenticator
                    .clone()
                    .ok_or(StateError::AuthenticatorNotSet)?;
                let method = auth
                    .properties
                    .as_ref()
                    .and_then(|props| props.method.as_deref())
                    .unwrap_or_default();
                let context = AuthContext { kind, method };
                let continue_result = match authenticator.lock() {
                    Ok(mut locked) => locked.continue_auth(context, auth.properties.clone()),
                    Err(poisoned) => {
                        drop(poisoned);
                        self.fail_auth_exchange_due_to_authenticator_lock_poisoned();
                        return Err(StateError::AuthenticatorLockPoisoned);
                    }
                };
                let action = match continue_result {
                    Ok(action) => action,
                    Err(err) => return Err(self.fail_authenticator(&err)),
                };

                let out_auth_props = match action.into_continue_properties() {
                    Ok(properties) => properties,
                    Err(err) => return Err(self.fail_authenticator(&err)),
                };
                let client_auth = self.auth.outgoing_continue(out_auth_props)?;
                Ok(Some(self.outgoing_auth_packet(client_auth)))
            }
        }
    }

    fn fail_authenticator(&mut self, error: &AuthError) -> StateError {
        self.fail_auth_exchange(
            AuthNoticeError::AuthenticationFailed(error.to_string()),
            error.clone(),
        );
        StateError::AuthError(error.to_string())
    }

    fn fail_auth_exchange(&mut self, notice_error: AuthNoticeError, callback_error: AuthError) {
        if let Some((kind, method)) = self.auth.active_exchange()
            && let Some(authenticator) = self.authenticator.clone()
        {
            match authenticator.lock() {
                Ok(mut locked) => locked.failure(
                    AuthContext {
                        kind,
                        method: &method,
                    },
                    callback_error,
                ),
                Err(_) => debug!(
                    "authenticator lock poisoned while failing auth exchange: {notice_error:?}"
                ),
            }
        }
        self.auth.reset(notice_error, &mut self.events);
    }

    /// Adds next packet identifier to `QoS` 1 and 2 publish packets and returns
    /// it by wrapping the publish in a packet.
    #[cfg(test)]
    fn outgoing_publish(&mut self, publish: Publish) -> Result<Option<Packet>, StateError> {
        let (packet, flush_notice) = self.outgoing_publish_with_notice(publish, None)?;
        if let Some(tx) = flush_notice {
            tx.success(PublishResult::Qos0Flushed);
        }
        Ok(packet)
    }

    fn outgoing_publish_with_notice(
        &mut self,
        publish: Publish,
        notice: Option<PublishNoticeTx>,
    ) -> Result<(Option<Packet>, Option<PublishNoticeTx>), StateError> {
        self.outgoing_publish_with_notice_internal(publish, notice, false)
    }

    fn replay_outgoing_publish_with_notice(
        &mut self,
        publish: Publish,
        notice: Option<PublishNoticeTx>,
    ) -> Result<(Option<Packet>, Option<PublishNoticeTx>), StateError> {
        self.outgoing_publish_with_notice_internal(publish, notice, true)
    }

    fn outgoing_publish_with_notice_internal(
        &mut self,
        mut publish: Publish,
        notice: Option<PublishNoticeTx>,
        replay: bool,
    ) -> Result<(Option<Packet>, Option<PublishNoticeTx>), StateError> {
        if publish.retain && !self.retain_available {
            return Err(StateError::RetainNotSupported);
        }

        let mut notice = notice;
        let auto_topic_alias_action = self.apply_auto_topic_alias(&mut publish);
        self.validate_outgoing_topic_alias(&publish)?;

        if publish.qos != QoS::AtMostOnce {
            if publish.pkid == 0 {
                if replay {
                    return Err(StateError::InvalidState);
                }
                publish.pkid = self.next_pkid().ok_or(StateError::InvalidState)?;
            }

            let pkid = publish.pkid;
            self.validate_outgoing_pkid_bound(pkid)?;
            if self
                .outgoing_pub
                .get(usize::from(publish.pkid))
                .ok_or(StateError::Unsolicited(publish.pkid))?
                .is_some()
            {
                info!("Collision on packet id = {:?}", publish.pkid);
                if let Some(action) = auto_topic_alias_action {
                    action.restore_for_replay(&mut publish);
                }
                self.collision = Some(publish);
                self.collision_notice = notice.take();
                let event = Event::Outgoing(Outgoing::AwaitAck(pkid));
                self.events.push_back(event);
                return Ok((None, None));
            }

            if self.outgoing_rel.contains(usize::from(pkid))
                || self.outgoing_rel_replay.contains(usize::from(pkid))
                || self.pending_subscribe.contains_key(&pkid)
                || self.pending_unsubscribe.contains_key(&pkid)
            {
                return Err(StateError::InvalidState);
            }

            // if there is an existing publish at this pkid, this implies that broker hasn't acked this
            // packet yet. This error is possible only when broker isn't acking sequentially
            if replay {
                self.accept_replayed_outbound_pkid(pkid)?;
            } else {
                self.reserve_outbound_pkid(pkid)?;
            }
            let replay_publish = self.publish_for_replay_tracking(&publish);
            self.outgoing_pub[usize::from(pkid)] = Some(replay_publish);
            self.outgoing_pub_notice[usize::from(pkid)] = notice.take();
            self.outgoing_pub_flush_attempted
                .set(usize::from(pkid), false);
            self.outgoing_pub_ack.set(usize::from(pkid), false);
            self.inflight += 1;
        }

        debug!(
            "Publish. Topic = {}, Pkid = {:?}, Payload Size = {:?}",
            String::from_utf8_lossy(&publish.topic),
            publish.pkid,
            publish.payload.len()
        );

        let pkid = publish.pkid;
        if let Some(pending_auto_topic_alias) = auto_topic_alias_action
            .as_ref()
            .and_then(AutoTopicAliasAction::pending_alias)
        {
            self.record_auto_topic_alias(pending_auto_topic_alias.clone());
        } else if let Some(alias) = auto_topic_alias_action
            .as_ref()
            .and_then(AutoTopicAliasAction::existing_alias)
        {
            self.record_auto_topic_alias_use(alias);
        }
        self.record_outgoing_topic_alias(&publish);

        let event = Event::Outgoing(Outgoing::Publish(pkid));
        self.events.push_back(event);

        if publish.qos == QoS::AtMostOnce {
            Ok((Some(Packet::Publish(publish)), notice.take()))
        } else {
            Ok((Some(Packet::Publish(publish)), None))
        }
    }

    fn outgoing_pubrel_with_notice(
        &mut self,
        pubrel: PubRel,
        notice: Option<PublishNoticeTx>,
    ) -> Result<(Option<Packet>, Option<PublishNoticeTx>), StateError> {
        let pubrel = self.save_pubrel_with_notice(pubrel, notice)?;

        debug!("Pubrel. Pkid = {}", pubrel.pkid);

        let event = Event::Outgoing(Outgoing::PubRel(pubrel.pkid));
        self.events.push_back(event);

        Ok((Some(Packet::PubRel(PubRel::new(pubrel.pkid, None))), None))
    }

    fn outgoing_puback(&mut self, puback: PubAck) -> Result<Packet, StateError> {
        if !self.incoming_puback.contains(usize::from(puback.pkid)) {
            error!("Unsolicited puback request: {:?}", puback.pkid);
            return Err(StateError::Unsolicited(puback.pkid));
        }

        self.incoming_puback.set(usize::from(puback.pkid), false);
        let pkid = puback.pkid;
        let event = Event::Outgoing(Outgoing::PubAck(pkid));
        self.events.push_back(event);

        Ok(Packet::PubAck(puback))
    }

    fn outgoing_pubrec(&mut self, pubrec: PubRec) -> Result<Packet, StateError> {
        self.outgoing_pubrec_internal(pubrec, false)
    }

    fn outgoing_pubrec_for_incoming_publish(
        &mut self,
        pubrec: PubRec,
    ) -> Result<Packet, StateError> {
        self.outgoing_pubrec_internal(pubrec, true)
    }

    fn outgoing_pubrec_internal(
        &mut self,
        pubrec: PubRec,
        allow_duplicate: bool,
    ) -> Result<Packet, StateError> {
        if !self.incoming_pub.contains(usize::from(pubrec.pkid)) {
            error!("Unsolicited pubrec request: {:?}", pubrec.pkid);
            return Err(StateError::Unsolicited(pubrec.pkid));
        }
        if self.incoming_pubrec.contains(usize::from(pubrec.pkid)) {
            if allow_duplicate {
                let pkid = pubrec.pkid;
                let event = Event::Outgoing(Outgoing::PubRec(pkid));
                self.events.push_back(event);

                return Ok(Packet::PubRec(pubrec));
            }

            error!("Duplicate pubrec request: {:?}", pubrec.pkid);
            return Err(StateError::Unsolicited(pubrec.pkid));
        }

        if (pubrec.reason as u8) >= 0x80 {
            self.incoming_pub.set(usize::from(pubrec.pkid), false);
            self.incoming_pubrec.set(usize::from(pubrec.pkid), false);
        } else {
            self.incoming_pubrec.insert(usize::from(pubrec.pkid));
        }
        let pkid = pubrec.pkid;
        let event = Event::Outgoing(Outgoing::PubRec(pkid));
        self.events.push_back(event);

        Ok(Packet::PubRec(pubrec))
    }

    /// check when the last control packet/pingreq packet is received and return
    /// the status which tells if keep alive time has exceeded
    /// NOTE: status will be checked for zero keepalive times also
    fn outgoing_ping(&mut self) -> Result<Option<Packet>, StateError> {
        let elapsed_in = self.last_incoming.elapsed();
        let elapsed_out = self.last_outgoing.elapsed();

        if self.collision.is_some() && self.ping.increment_collision_count() >= 2 {
            return Err(StateError::CollisionTimeout);
        }

        // raise error if last ping didn't receive ack
        if self.ping.await_pingresp() {
            return Err(StateError::AwaitPingResp);
        }

        self.ping.begin_waiting_for_resp();

        debug!(
            "Pingreq, last incoming packet before {elapsed_in:?}, last outgoing request before {elapsed_out:?}",
        );

        let event = Event::Outgoing(Outgoing::PingReq);
        self.events.push_back(event);

        Ok(Some(Packet::PingReq))
    }

    fn outgoing_subscribe(
        &mut self,
        mut subscription: Subscribe,
        notice: Option<SubscribeNoticeTx>,
    ) -> Result<Option<Packet>, StateError> {
        if subscription.filters.is_empty() {
            return Err(StateError::EmptySubscription);
        }

        let pkid = self.next_control_pkid()?;
        subscription.pkid = pkid;
        Ok(Some(self.save_outgoing_subscribe(subscription, notice)))
    }

    fn replay_outgoing_subscribe(
        &mut self,
        subscription: Subscribe,
        notice: Option<SubscribeNoticeTx>,
    ) -> Result<Option<Packet>, StateError> {
        if subscription.filters.is_empty() {
            return Err(StateError::EmptySubscription);
        }

        self.accept_replayed_outbound_pkid(subscription.pkid)?;
        Ok(Some(self.save_outgoing_subscribe(subscription, notice)))
    }

    fn save_outgoing_subscribe(
        &mut self,
        subscription: Subscribe,
        notice: Option<SubscribeNoticeTx>,
    ) -> Packet {
        debug!(
            "Subscribe. Topics = {:?}, Pkid = {:?}",
            subscription.filters, subscription.pkid
        );

        let pkid = subscription.pkid;
        let event = Event::Outgoing(Outgoing::Subscribe(pkid));
        self.events.push_back(event);
        self.pending_subscribe.insert(
            subscription.pkid,
            PendingSubscribe {
                subscribe: subscription.clone(),
                notice,
            },
        );

        Packet::Subscribe(subscription)
    }

    fn outgoing_unsubscribe(
        &mut self,
        mut unsub: Unsubscribe,
        notice: Option<UnsubscribeNoticeTx>,
    ) -> Result<Packet, StateError> {
        let pkid = self.next_control_pkid()?;
        unsub.pkid = pkid;
        Ok(self.save_outgoing_unsubscribe(unsub, notice))
    }

    fn replay_outgoing_unsubscribe(
        &mut self,
        unsub: Unsubscribe,
        notice: Option<UnsubscribeNoticeTx>,
    ) -> Result<Packet, StateError> {
        self.accept_replayed_outbound_pkid(unsub.pkid)?;
        Ok(self.save_outgoing_unsubscribe(unsub, notice))
    }

    fn save_outgoing_unsubscribe(
        &mut self,
        unsub: Unsubscribe,
        notice: Option<UnsubscribeNoticeTx>,
    ) -> Packet {
        debug!(
            "Unsubscribe. Topics = {:?}, Pkid = {:?}",
            unsub.filters, unsub.pkid
        );

        let pkid = unsub.pkid;
        let event = Event::Outgoing(Outgoing::Unsubscribe(pkid));
        self.events.push_back(event);
        self.pending_unsubscribe.insert(
            unsub.pkid,
            PendingUnsubscribe {
                unsubscribe: unsub.clone(),
                notice,
            },
        );

        Packet::Unsubscribe(unsub)
    }

    fn outgoing_disconnect(&mut self, disconnect: Disconnect) -> Packet {
        self.fail_auth_exchange_due_to_client_disconnect();
        let reason = disconnect.reason_code;
        debug!("Disconnect with {reason:?}");
        let event = Event::Outgoing(Outgoing::Disconnect);
        self.events.push_back(event);

        Packet::Disconnect(disconnect)
    }

    pub(crate) fn discard_last_outgoing_disconnect_event(&mut self) {
        if matches!(
            self.events.back(),
            Some(Event::Outgoing(Outgoing::Disconnect))
        ) {
            self.events.pop_back();
        }
    }

    fn outgoing_auth(
        &mut self,
        mut auth: Auth,
        mut notice: Option<crate::notice::AuthNoticeTx>,
    ) -> Result<Packet, StateError> {
        let method = match self.auth.reauth_method() {
            Ok(method) => method.to_owned(),
            Err(err) => {
                if let Some(notice) = notice.take() {
                    notice.error(err.clone());
                }
                return Err(StateError::AuthError(err.to_string()));
            }
        };

        if let Some(authenticator) = self.authenticator.clone() {
            let context = AuthContext {
                kind: AuthExchangeKind::Reauthentication,
                method: &method,
            };
            let start_result = match authenticator.lock() {
                Ok(mut locked) => locked.start(context),
                Err(_poisoned) => {
                    if let Some(notice) = notice.take() {
                        notice.error(AuthNoticeError::AuthenticationFailed(
                            StateError::AuthenticatorLockPoisoned.to_string(),
                        ));
                    }
                    return Err(StateError::AuthenticatorLockPoisoned);
                }
            };
            match start_result {
                Ok(Some(properties)) if auth.properties.is_none() => {
                    auth.properties = Some(properties);
                }
                Ok(_) => {}
                Err(err) => {
                    if let Some(notice) = notice.take() {
                        notice.error(AuthNoticeError::AuthenticationFailed(err.to_string()));
                    }
                    return Err(StateError::AuthError(err.to_string()));
                }
            }
        }
        let auth = self
            .auth
            .begin_reauth(auth.properties, notice, &mut self.events)?;
        Ok(self.outgoing_auth_packet(auth))
    }

    fn outgoing_auth_packet(&mut self, auth: Auth) -> Packet {
        let props = auth
            .properties
            .as_ref()
            .expect("AUTH packets created by state always contain properties");
        debug!(
            "Auth packet sent. Auth Method: {:?}. Auth Data: {:?}",
            props.method, props.data
        );
        let event = Event::Outgoing(Outgoing::Auth);
        self.events.push_back(event);
        Packet::Auth(auth)
    }

    fn check_collision(&mut self, pkid: u16) -> Option<(Publish, Option<PublishNoticeTx>)> {
        if let Some(publish) = &self.collision
            && publish.pkid == pkid
        {
            return self
                .collision
                .take()
                .map(|publish| (publish, self.collision_notice.take()));
        }

        None
    }

    fn save_pubrel_with_notice(
        &mut self,
        pubrel: PubRel,
        notice: Option<PublishNoticeTx>,
    ) -> Result<PubRel, StateError> {
        self.validate_outgoing_pkid_bound(pubrel.pkid)?;
        let pkid_index = usize::from(pubrel.pkid);
        let replaying_pubrel = self.outgoing_rel_replay.contains(pkid_index);
        if !self.outgoing_rel.contains(pkid_index) && !replaying_pubrel {
            error!("Unsolicited pubrel request: {:?}", pubrel.pkid);
            return Err(StateError::Unsolicited(pubrel.pkid));
        }

        if replaying_pubrel {
            self.outgoing_rel_replay.set(pkid_index, false);
        }
        self.outgoing_rel.insert(pkid_index);
        self.outgoing_rel_notice[pkid_index] = notice;
        self.inflight += 1;
        Ok(pubrel)
    }

    fn mark_outgoing_packet_id_complete(&mut self, pkid: u16) {
        self.outgoing_pub_ack.set(usize::from(pkid), true);
        self.advance_last_puback_frontier();
    }

    fn mark_control_packet_id_complete(&mut self, pkid: u16) {
        if self.control_packet_id_needs_frontier_completion(pkid) {
            self.ensure_outgoing_tracking_capacity(usize::from(pkid) + 1);
            self.mark_outgoing_packet_id_complete(pkid);
        }
    }

    const fn control_packet_id_needs_frontier_completion(&self, pkid: u16) -> bool {
        pkid != 0
            && pkid <= self.max_outgoing_inflight
            && pkid != self.last_puback
            && (self.last_puback >= self.max_outgoing_inflight || pkid > self.last_puback)
    }

    fn advance_last_puback_frontier(&mut self) {
        let mut next = self.next_puback_boundary_pkid(self.last_puback);
        while next != 0 && self.outgoing_pub_ack.contains(usize::from(next)) {
            self.outgoing_pub_ack.set(usize::from(next), false);
            self.last_puback = next;
            next = self.next_puback_boundary_pkid(self.last_puback);
        }
    }

    const fn next_puback_boundary_pkid(&self, pkid: u16) -> u16 {
        if self.max_outgoing_inflight == 0 {
            return 0;
        }

        if pkid >= self.max_outgoing_inflight {
            1
        } else {
            pkid + 1
        }
    }

    fn reserve_outbound_pkid(&mut self, pkid: u16) -> Result<(), StateError> {
        if pkid == 0 || self.packet_identifier_in_use(pkid) {
            return Err(StateError::InvalidState);
        }

        self.outbound_pkid_in_use.insert(usize::from(pkid));
        self.outbound_pkid_count = self
            .outbound_pkid_count
            .checked_add(1)
            .ok_or(StateError::InvalidState)?;
        Ok(())
    }

    fn accept_replayed_outbound_pkid(&self, pkid: u16) -> Result<(), StateError> {
        if pkid == 0 || !self.packet_identifier_in_use(pkid) {
            return Err(StateError::InvalidState);
        }

        Ok(())
    }

    fn release_outbound_pkid(&mut self, pkid: u16) {
        if pkid == 0 || !self.packet_identifier_in_use(pkid) {
            return;
        }

        self.outbound_pkid_in_use.set(usize::from(pkid), false);
        self.outbound_pkid_count = self
            .outbound_pkid_count
            .checked_sub(1)
            .expect("reserved packet identifier count should not underflow");
    }

    pub(crate) fn discard_replayed_request(&mut self, request: &Request) {
        match request {
            Request::Publish(publish) if publish.qos != QoS::AtMostOnce => {
                self.release_outbound_pkid(publish.pkid);
            }
            Request::PubRel(pubrel) => {
                self.outgoing_rel_replay
                    .set(usize::from(pubrel.pkid), false);
                self.release_outbound_pkid(pubrel.pkid);
            }
            Request::Subscribe(subscribe) => {
                self.release_outbound_pkid(subscribe.pkid);
            }
            Request::Unsubscribe(unsubscribe) => {
                self.release_outbound_pkid(unsubscribe.pkid);
            }
            _ => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn rebuild_outbound_pkid_index(&mut self) {
        self.outbound_pkid_in_use.clear();

        for (pkid, publish) in self.outgoing_pub.iter().enumerate() {
            if pkid != 0 && publish.is_some() {
                self.outbound_pkid_in_use.insert(pkid);
            }
        }

        self.outbound_pkid_in_use.union_with(&self.outgoing_rel);
        self.outbound_pkid_in_use
            .union_with(&self.outgoing_rel_replay);

        for &pkid in self.pending_subscribe.keys() {
            self.outbound_pkid_in_use.insert(usize::from(pkid));
        }

        for &pkid in self.pending_unsubscribe.keys() {
            self.outbound_pkid_in_use.insert(usize::from(pkid));
        }

        self.outbound_pkid_in_use.set(0, false);
        self.outbound_pkid_count = u16::try_from(self.outbound_pkid_in_use.ones().count())
            .expect("non-zero MQTT packet identifier count fits in u16");
    }

    fn next_free_outbound_pkid_after(&self, last_pkid: u16) -> Option<u16> {
        if !self.control_packet_identifier_available() {
            return None;
        }

        let start = if last_pkid == u16::MAX {
            1
        } else {
            usize::from(last_pkid) + 1
        };

        self.find_free_outbound_pkid_in_range(start, Self::full_packet_identifier_len())
            .or_else(|| self.find_free_outbound_pkid_in_range(1, start))
    }

    fn find_free_outbound_pkid_in_range(&self, start: usize, end: usize) -> Option<u16> {
        if start >= end {
            return None;
        }

        let bits_per_block = usize::BITS as usize;
        let blocks = self.outbound_pkid_in_use.as_slice();
        let mut block_index = start / bits_per_block;
        let end_block = (end - 1) / bits_per_block;

        while block_index <= end_block {
            let block_start = block_index * bits_per_block;
            let from = start.saturating_sub(block_start).min(bits_per_block);
            let to = (end - block_start).min(bits_per_block);
            let width = to.saturating_sub(from);
            if width == 0 {
                block_index += 1;
                continue;
            }

            let mask = if width == bits_per_block {
                usize::MAX
            } else {
                ((1usize << width) - 1) << from
            };
            let occupied = blocks.get(block_index).copied().unwrap_or(0);
            let free = !occupied & mask;
            if free != 0 {
                let pkid = block_start + free.trailing_zeros() as usize;
                return u16::try_from(pkid).ok();
            }

            block_index += 1;
        }

        None
    }

    /// <http://stackoverflow.com/questions/11115364/mqtt-messageid-practical-implementation>
    /// Packet ids are incremented till maximum set inflight messages and reset to 1 after that.
    ///
    fn next_publish_pkid(&self) -> Option<u16> {
        let mut pkid = self.next_publish_pkid_after(self.last_pkid);
        for _ in 0..usize::from(self.max_outgoing_inflight) {
            if !self.packet_identifier_in_use(pkid) {
                return Some(pkid);
            }
            pkid = self.next_publish_pkid_after(pkid);
        }

        None
    }

    fn next_pkid(&mut self) -> Option<u16> {
        let next_pkid = self.next_publish_pkid()?;

        // When next packet id is at the edge of inflight queue,
        // set await flag. This instructs eventloop to stop
        // processing requests until all the inflight publishes
        // are acked
        if next_pkid == self.max_outgoing_inflight {
            self.last_pkid = 0;
            return Some(next_pkid);
        }

        self.last_pkid = next_pkid;
        Some(next_pkid)
    }

    fn next_control_pkid(&mut self) -> Result<u16, StateError> {
        let pkid = self
            .next_free_outbound_pkid_after(self.last_pkid)
            .ok_or(StateError::InvalidState)?;
        self.reserve_outbound_pkid(pkid)?;
        self.last_pkid = pkid;
        Ok(pkid)
    }

    fn publish_topic_alias(publish: &Publish) -> Option<u16> {
        publish
            .properties
            .as_ref()
            .and_then(|props| props.topic_alias)
    }

    fn set_publish_topic_alias(publish: &mut Publish, alias: u16) {
        publish
            .properties
            .get_or_insert_with(PublishProperties::default)
            .topic_alias = Some(alias);
    }

    fn apply_auto_topic_alias(&self, publish: &mut Publish) -> Option<AutoTopicAliasAction> {
        if self.topic_aliases.auto_policy == TopicAliasPolicy::Disabled
            || self.topic_aliases.broker_max == 0
            || publish.topic.is_empty()
            || Self::publish_topic_alias(publish).is_some()
        {
            return None;
        }

        if let Some(alias) = self
            .topic_aliases
            .auto_outgoing
            .get(&publish.topic)
            .copied()
        {
            let original_topic = publish.topic.clone();
            Self::set_publish_topic_alias(publish, alias);
            publish.topic = Bytes::new();
            return Some(AutoTopicAliasAction::Existing {
                original_topic,
                alias,
            });
        }

        let (alias, previous_topic) = self.next_auto_topic_alias_assignment()?;

        let pending = PendingAutoTopicAlias {
            topic: publish.topic.clone(),
            alias,
            previous_topic,
        };
        Self::set_publish_topic_alias(publish, alias);
        Some(AutoTopicAliasAction::New(pending))
    }

    fn next_auto_topic_alias_assignment(&self) -> Option<(u16, Option<Bytes>)> {
        if let Some(alias) = self.next_available_auto_topic_alias() {
            return Some((alias, None));
        }

        if self.topic_aliases.auto_policy != TopicAliasPolicy::Lru {
            return None;
        }

        self.least_recent_auto_topic_alias()
    }

    fn next_available_auto_topic_alias(&self) -> Option<u16> {
        let next_auto_topic_alias = self.topic_aliases.next_auto?;
        (next_auto_topic_alias..=self.topic_aliases.broker_max)
            .find(|&alias| !self.topic_aliases.outgoing.contains_key(&alias))
    }

    fn record_auto_topic_alias(&mut self, pending: PendingAutoTopicAlias) {
        if let Some(previous_topic) = pending.previous_topic {
            self.topic_aliases.auto_outgoing.remove(&previous_topic);
        }
        self.topic_aliases
            .auto_outgoing
            .insert(pending.topic.clone(), pending.alias);
        self.topic_aliases
            .outgoing
            .insert(pending.alias, pending.topic.clone());
        self.record_auto_topic_alias_use(pending.alias);
        self.advance_next_auto_topic_alias();
    }

    fn record_auto_topic_alias_use(&mut self, alias: u16) {
        if self.topic_aliases.auto_policy != TopicAliasPolicy::Lru {
            return;
        }

        self.topic_aliases.auto_lru.retain(|entry| *entry != alias);
        self.topic_aliases.auto_lru.push_back(alias);
    }

    fn least_recent_auto_topic_alias(&self) -> Option<(u16, Option<Bytes>)> {
        for &alias in &self.topic_aliases.auto_lru {
            if alias == 0 || alias > self.topic_aliases.broker_max {
                continue;
            }

            let Some(topic) = self.topic_aliases.outgoing.get(&alias) else {
                continue;
            };

            if self
                .topic_aliases
                .auto_outgoing
                .get(topic)
                .is_some_and(|mapped_alias| *mapped_alias == alias)
            {
                return Some((alias, Some(topic.clone())));
            }
        }

        None
    }

    fn advance_next_auto_topic_alias(&mut self) {
        let Some(mut alias) = self.topic_aliases.next_auto else {
            return;
        };

        while alias <= self.topic_aliases.broker_max
            && self.topic_aliases.outgoing.contains_key(&alias)
        {
            let Some(next_alias) = alias.checked_add(1) else {
                self.topic_aliases.next_auto = None;
                return;
            };
            alias = next_alias;
        }

        self.topic_aliases.next_auto = (alias <= self.topic_aliases.broker_max).then_some(alias);
    }

    const fn strip_publish_topic_alias(publish: &mut Publish) {
        if let Some(props) = &mut publish.properties {
            props.topic_alias = None;
        }
    }

    fn publish_for_replay_tracking(&self, publish: &Publish) -> Publish {
        let mut replay_publish = publish.clone();
        if replay_publish.topic.is_empty()
            && let Some(alias) = Self::publish_topic_alias(&replay_publish)
            && let Some(topic) = self.topic_aliases.outgoing.get(&alias)
        {
            topic.clone_into(&mut replay_publish.topic);
        }

        replay_publish
    }

    fn validate_outgoing_topic_alias(&self, publish: &Publish) -> Result<(), StateError> {
        if let Some(alias) = Self::publish_topic_alias(publish)
            && (alias == 0 || alias > self.topic_aliases.broker_max)
        {
            // We MUST NOT send a Topic Alias of 0 or one greater than the
            // broker's Topic Alias Maximum.
            return Err(StateError::InvalidAlias {
                alias,
                max: self.topic_aliases.broker_max,
            });
        }

        Ok(())
    }

    fn record_outgoing_topic_alias(&mut self, publish: &Publish) {
        if !publish.topic.is_empty()
            && let Some(alias) = Self::publish_topic_alias(publish)
        {
            if let Some(previous_topic) = self
                .topic_aliases
                .outgoing
                .insert(alias, publish.topic.clone())
                && previous_topic != publish.topic
            {
                self.topic_aliases.auto_outgoing.remove(&previous_topic);
                self.topic_aliases.auto_lru.retain(|entry| *entry != alias);
            }
            self.topic_aliases
                .auto_outgoing
                .retain(|topic, mapped_alias| *mapped_alias != alias || topic == &publish.topic);
        }
    }

    pub(crate) fn persisted_session<'a>(
        &self,
        options: &MqttOptions,
        replay_requests: impl IntoIterator<Item = &'a Request>,
    ) -> PersistedSession {
        let mut replay = self.persisted_replay_requests();
        replay.extend(
            replay_requests
                .into_iter()
                .filter_map(Self::persisted_request_from_request),
        );

        PersistedSession {
            format_version: 1,
            client_id: options.client_id(),
            clean_start: options.clean_start(),
            session_expiry_interval: options.session_expiry_interval(),
            outgoing_inflight_upper_limit: options.get_outgoing_inflight_upper_limit(),
            ack_mode: persisted_ack_mode(options.ack_mode()),
            last_pkid: self.last_pkid,
            last_puback: self.last_puback,
            replay,
            incoming_qos2: self
                .incoming_pubrec
                .ones()
                .filter_map(|pkid| {
                    u16::try_from(pkid)
                        .ok()
                        .map(|pkid| PersistedIncomingQos2 { pkid })
                })
                .collect(),
        }
    }

    fn persisted_replay_requests(&self) -> Vec<PersistedRequest> {
        let mut replay = Vec::with_capacity(self.clean_pending_capacity());
        let (first_half, second_half) = self
            .outgoing_pub
            .split_at(usize::from(self.last_puback) + 1);

        for publish in second_half.iter().chain(first_half.iter()).flatten() {
            let mut publish = publish.clone();
            if publish.qos != QoS::AtMostOnce
                && self
                    .outgoing_pub_flush_attempted
                    .contains(usize::from(publish.pkid))
            {
                publish.dup = true;
            }
            if let Some(persisted) = persisted_publish(&publish) {
                replay.push(PersistedRequest::Publish(persisted));
            }
        }

        replay.extend(
            self.outgoing_rel
                .ones()
                .filter_map(|pkid| u16::try_from(pkid).ok())
                .map(|pkid| PersistedRequest::PubRel(PersistedPubRel { pkid })),
        );

        replay.extend(
            self.pending_subscribe.values().map(|pending| {
                PersistedRequest::Subscribe(persisted_subscribe(&pending.subscribe))
            }),
        );
        replay.extend(self.pending_unsubscribe.values().map(|pending| {
            PersistedRequest::Unsubscribe(persisted_unsubscribe(&pending.unsubscribe))
        }));

        replay
    }

    fn persisted_request_from_request(request: &Request) -> Option<PersistedRequest> {
        match request {
            Request::Publish(publish) => persisted_publish(publish).map(PersistedRequest::Publish),
            Request::PubRel(pubrel) => Some(PersistedRequest::PubRel(PersistedPubRel {
                pkid: pubrel.pkid,
            })),
            Request::Subscribe(subscribe) if subscribe.pkid != 0 => {
                Some(PersistedRequest::Subscribe(persisted_subscribe(subscribe)))
            }
            Request::Unsubscribe(unsubscribe) if unsubscribe.pkid != 0 => Some(
                PersistedRequest::Unsubscribe(persisted_unsubscribe(unsubscribe)),
            ),
            _ => None,
        }
    }

    pub(crate) fn restore_persisted_session(
        &mut self,
        options: &MqttOptions,
        session: &PersistedSession,
    ) -> Result<Vec<Request>, SessionRestoreError> {
        validate_persisted_session_signature(options, session)?;
        self.validate_persisted_session_packet_ids(session)?;

        self.reset_session_state();
        self.last_pkid = session.last_pkid;
        self.last_puback = session.last_puback;

        for incoming in &session.incoming_qos2 {
            self.incoming_pub.insert(usize::from(incoming.pkid));
            self.incoming_pubrec.insert(usize::from(incoming.pkid));
        }

        for request in &session.replay {
            let pkid = match request {
                PersistedRequest::Publish(publish) => {
                    self.ensure_outgoing_tracking_capacity(usize::from(publish.pkid) + 1);
                    publish.pkid
                }
                PersistedRequest::PubRel(pubrel) => {
                    self.ensure_outgoing_tracking_capacity(usize::from(pubrel.pkid) + 1);
                    self.outgoing_rel_replay.insert(usize::from(pubrel.pkid));
                    pubrel.pkid
                }
                PersistedRequest::Subscribe(subscribe) => subscribe.pkid,
                PersistedRequest::Unsubscribe(unsubscribe) => unsubscribe.pkid,
            };
            self.outbound_pkid_in_use.insert(usize::from(pkid));
        }
        self.outbound_pkid_in_use.set(0, false);
        let outbound_pkid_count = self.outbound_pkid_in_use.ones().count();
        self.outbound_pkid_count = u16::try_from(outbound_pkid_count).map_err(|_| {
            SessionRestoreError::OutgoingPacketIdentifierCountOutOfRange {
                count: outbound_pkid_count,
            }
        })?;

        session
            .replay
            .iter()
            .map(request_from_persisted_request)
            .collect()
    }

    fn validate_persisted_session_packet_ids(
        &self,
        session: &PersistedSession,
    ) -> Result<(), SessionRestoreError> {
        validate_last_puback(session.last_puback, self.max_outgoing_inflight)?;

        for incoming in &session.incoming_qos2 {
            validate_pkid(incoming.pkid)?;
        }

        session.replay.iter().try_for_each(|request| {
            validate_persisted_replay_request_pkid(request, self.max_outgoing_inflight)
        })?;

        Ok(())
    }
}

fn validate_persisted_session_signature(
    options: &MqttOptions,
    session: &PersistedSession,
) -> Result<(), SessionRestoreError> {
    if session.format_version != 1 {
        return Err(SessionRestoreError::UnsupportedFormatVersion {
            actual: session.format_version,
        });
    }

    let configured_client_id = options.client_id();
    if session.client_id != configured_client_id {
        return Err(SessionRestoreError::ClientIdMismatch {
            persisted: session.client_id.clone(),
            configured: configured_client_id,
        });
    }

    if session.clean_start != options.clean_start() {
        return Err(SessionRestoreError::CleanStartMismatch {
            persisted: session.clean_start,
            configured: options.clean_start(),
        });
    }

    if session.outgoing_inflight_upper_limit != options.get_outgoing_inflight_upper_limit() {
        return Err(SessionRestoreError::OutgoingInflightUpperLimitMismatch {
            persisted: session.outgoing_inflight_upper_limit,
            configured: options.get_outgoing_inflight_upper_limit(),
        });
    }

    if session.ack_mode != persisted_ack_mode(options.ack_mode()) {
        return Err(SessionRestoreError::AckModeMismatch);
    }

    Ok(())
}

const fn validate_pkid(pkid: u16) -> Result<(), SessionRestoreError> {
    if pkid == 0 {
        return Err(SessionRestoreError::InvalidPacketIdentifier(pkid));
    }

    Ok(())
}

fn validate_outgoing_pkid(
    pkid: u16,
    max_outgoing_inflight: u16,
) -> Result<(), SessionRestoreError> {
    validate_pkid(pkid)?;

    if pkid > max_outgoing_inflight {
        return Err(SessionRestoreError::OutgoingPacketIdentifierOutOfRange {
            pkid,
            max_outgoing_inflight,
        });
    }

    Ok(())
}

const fn validate_last_puback(
    last_puback: u16,
    max_outgoing_inflight: u16,
) -> Result<(), SessionRestoreError> {
    if last_puback > max_outgoing_inflight {
        return Err(SessionRestoreError::LastPubAckOutOfRange {
            last_puback,
            max_outgoing_inflight,
        });
    }

    Ok(())
}

fn validate_persisted_replay_request_pkid(
    request: &PersistedRequest,
    max_outgoing_inflight: u16,
) -> Result<(), SessionRestoreError> {
    match request {
        PersistedRequest::Publish(publish) => {
            validate_outgoing_pkid(publish.pkid, max_outgoing_inflight)
        }
        PersistedRequest::PubRel(pubrel) => {
            validate_outgoing_pkid(pubrel.pkid, max_outgoing_inflight)
        }
        PersistedRequest::Subscribe(subscribe) => validate_pkid(subscribe.pkid),
        PersistedRequest::Unsubscribe(unsubscribe) => validate_pkid(unsubscribe.pkid),
    }
}

const fn persisted_ack_mode(ack_mode: AckMode) -> PersistedAckMode {
    match ack_mode {
        AckMode::Automatic => PersistedAckMode::Automatic,
        AckMode::Manual => PersistedAckMode::Manual,
    }
}

const fn persisted_qos(qos: QoS) -> PersistedQoS {
    match qos {
        QoS::AtMostOnce => PersistedQoS::AtMostOnce,
        QoS::AtLeastOnce => PersistedQoS::AtLeastOnce,
        QoS::ExactlyOnce => PersistedQoS::ExactlyOnce,
    }
}

const fn qos_from_persisted(qos: PersistedQoS) -> QoS {
    match qos {
        PersistedQoS::AtMostOnce => QoS::AtMostOnce,
        PersistedQoS::AtLeastOnce => QoS::AtLeastOnce,
        PersistedQoS::ExactlyOnce => QoS::ExactlyOnce,
    }
}

fn persisted_publish(publish: &Publish) -> Option<PersistedPublish> {
    if publish.qos == QoS::AtMostOnce || publish.pkid == 0 || publish.topic.is_empty() {
        return None;
    }

    Some(PersistedPublish {
        dup: publish.dup,
        qos: persisted_qos(publish.qos),
        retain: publish.retain,
        topic: publish.topic.to_vec(),
        pkid: publish.pkid,
        payload: publish.payload.to_vec(),
        properties: publish
            .properties
            .as_ref()
            .map(persisted_publish_properties),
    })
}

fn persisted_publish_properties(properties: &PublishProperties) -> PersistedPublishProperties {
    PersistedPublishProperties {
        payload_format_indicator: properties.payload_format_indicator,
        message_expiry_interval: properties.message_expiry_interval,
        response_topic: properties.response_topic.clone(),
        correlation_data: properties
            .correlation_data
            .as_ref()
            .map(|data| data.to_vec()),
        user_properties: properties.user_properties.clone(),
        subscription_identifiers: properties.subscription_identifiers.clone(),
        content_type: properties.content_type.clone(),
    }
}

fn persisted_subscribe(subscribe: &Subscribe) -> PersistedSubscribe {
    PersistedSubscribe {
        pkid: subscribe.pkid,
        filters: subscribe.filters.iter().map(persisted_filter).collect(),
        properties: subscribe
            .properties
            .as_ref()
            .map(|properties| PersistedSubscribeProperties {
                id: properties.id,
                user_properties: properties.user_properties.clone(),
            }),
    }
}

fn persisted_filter(filter: &SubscribeFilter) -> PersistedFilter {
    PersistedFilter {
        path: filter.path.clone(),
        qos: persisted_qos(filter.qos),
        nolocal: filter.nolocal,
        preserve_retain: filter.preserve_retain,
        retain_forward_rule: match filter.retain_forward_rule {
            RetainForwardRule::OnEverySubscribe => PersistedRetainForwardRule::OnEverySubscribe,
            RetainForwardRule::OnNewSubscribe => PersistedRetainForwardRule::OnNewSubscribe,
            RetainForwardRule::Never => PersistedRetainForwardRule::Never,
        },
    }
}

fn persisted_unsubscribe(unsubscribe: &Unsubscribe) -> PersistedUnsubscribe {
    PersistedUnsubscribe {
        pkid: unsubscribe.pkid,
        filters: unsubscribe.filters.clone(),
        properties: unsubscribe.properties.as_ref().map(|properties| {
            PersistedUnsubscribeProperties {
                user_properties: properties.user_properties.clone(),
            }
        }),
    }
}

fn request_from_persisted_request(
    request: &PersistedRequest,
) -> Result<Request, SessionRestoreError> {
    match request {
        PersistedRequest::Publish(publish) => {
            validate_pkid(publish.pkid)?;
            Ok(Request::Publish(Publish {
                dup: publish.dup,
                qos: qos_from_persisted(publish.qos),
                retain: publish.retain,
                topic: Bytes::from(publish.topic.clone()),
                pkid: publish.pkid,
                payload: Bytes::from(publish.payload.clone()),
                properties: publish
                    .properties
                    .as_ref()
                    .map(publish_properties_from_persisted),
            }))
        }
        PersistedRequest::PubRel(pubrel) => {
            validate_pkid(pubrel.pkid)?;
            Ok(Request::PubRel(PubRel::new(pubrel.pkid, None)))
        }
        PersistedRequest::Subscribe(subscribe) => {
            validate_pkid(subscribe.pkid)?;
            Ok(Request::Subscribe(Subscribe {
                pkid: subscribe.pkid,
                filters: subscribe
                    .filters
                    .iter()
                    .map(filter_from_persisted)
                    .collect(),
                properties: subscribe
                    .properties
                    .as_ref()
                    .map(subscribe_properties_from_persisted),
            }))
        }
        PersistedRequest::Unsubscribe(unsubscribe) => {
            validate_pkid(unsubscribe.pkid)?;
            Ok(Request::Unsubscribe(Unsubscribe {
                pkid: unsubscribe.pkid,
                filters: unsubscribe.filters.clone(),
                properties: unsubscribe
                    .properties
                    .as_ref()
                    .map(unsubscribe_properties_from_persisted),
            }))
        }
    }
}

fn publish_properties_from_persisted(properties: &PersistedPublishProperties) -> PublishProperties {
    PublishProperties {
        payload_format_indicator: properties.payload_format_indicator,
        message_expiry_interval: properties.message_expiry_interval,
        topic_alias: None,
        response_topic: properties.response_topic.clone(),
        correlation_data: properties
            .correlation_data
            .as_ref()
            .map(|data| Bytes::from(data.clone())),
        user_properties: properties.user_properties.clone(),
        subscription_identifiers: properties.subscription_identifiers.clone(),
        content_type: properties.content_type.clone(),
    }
}

fn filter_from_persisted(filter: &PersistedFilter) -> SubscribeFilter {
    SubscribeFilter {
        path: filter.path.clone(),
        qos: qos_from_persisted(filter.qos),
        nolocal: filter.nolocal,
        preserve_retain: filter.preserve_retain,
        retain_forward_rule: match filter.retain_forward_rule {
            PersistedRetainForwardRule::OnEverySubscribe => RetainForwardRule::OnEverySubscribe,
            PersistedRetainForwardRule::OnNewSubscribe => RetainForwardRule::OnNewSubscribe,
            PersistedRetainForwardRule::Never => RetainForwardRule::Never,
        },
    }
}

fn subscribe_properties_from_persisted(
    properties: &PersistedSubscribeProperties,
) -> SubscribeProperties {
    SubscribeProperties {
        id: properties.id,
        user_properties: properties.user_properties.clone(),
    }
}

fn unsubscribe_properties_from_persisted(
    properties: &PersistedUnsubscribeProperties,
) -> UnsubscribeProperties {
    UnsubscribeProperties {
        user_properties: properties.user_properties.clone(),
    }
}

impl Clone for MqttState {
    fn clone(&self) -> Self {
        Self {
            ping: self.ping,
            last_incoming: self.last_incoming,
            last_outgoing: self.last_outgoing,
            last_pkid: self.last_pkid,
            last_puback: self.last_puback,
            inflight: self.inflight,
            outgoing_pub: self.outgoing_pub.clone(),
            outgoing_pub_notice: Self::new_notice_slots_with_len(self.outgoing_pub.len()),
            outgoing_pub_flush_attempted: self.outgoing_pub_flush_attempted.clone(),
            outgoing_pub_ack: self.outgoing_pub_ack.clone(),
            outgoing_rel: self.outgoing_rel.clone(),
            outgoing_rel_replay: self.outgoing_rel_replay.clone(),
            outgoing_rel_notice: Self::new_notice_slots_with_len(self.outgoing_rel_notice.len()),
            incoming_puback: self.incoming_puback.clone(),
            incoming_pub: self.incoming_pub.clone(),
            incoming_pubrec: self.incoming_pubrec.clone(),
            outbound_pkid_in_use: self.outbound_pkid_in_use.clone(),
            outbound_pkid_count: self.outbound_pkid_count,
            collision: self.collision.clone(),
            collision_notice: None,
            pending_subscribe: self
                .pending_subscribe
                .iter()
                .map(|(&pkid, pending)| {
                    (
                        pkid,
                        PendingSubscribe {
                            subscribe: pending.subscribe.clone(),
                            notice: None,
                        },
                    )
                })
                .collect(),
            pending_unsubscribe: self
                .pending_unsubscribe
                .iter()
                .map(|(&pkid, pending)| {
                    (
                        pkid,
                        PendingUnsubscribe {
                            unsubscribe: pending.unsubscribe.clone(),
                            notice: None,
                        },
                    )
                })
                .collect(),
            events: self.events.clone(),
            ack_mode: self.ack_mode,
            topic_aliases: self.topic_aliases.clone(),
            connack_received: self.connack_received,
            retain_available: self.retain_available,
            max_outgoing_inflight: self.max_outgoing_inflight,
            max_outgoing_inflight_upper_limit: self.max_outgoing_inflight_upper_limit,
            authenticator: self.authenticator.clone(),
            auth: self.auth.clone(),
        }
    }
}

#[cfg(test)]
mod test {
    use super::mqttbytes::v5::*;
    use super::mqttbytes::*;
    use super::{Event, Incoming, Outgoing, Request};
    use super::{MqttState, PendingSubscribe, PendingUnsubscribe, ProtocolViolation, StateError};
    use crate::notice::{
        AuthNoticeError, AuthNoticeTx, PublishNotice, PublishNoticeError, PublishNoticeTx,
        PublishResult, SubscribeNoticeError, SubscribeNoticeTx, TrackedNoticeTx,
        UnsubscribeNoticeError, UnsubscribeNoticeTx,
    };
    use crate::{
        AckMode, MqttOptions, NoticeFailureReason, PersistedAckMode, PersistedPubRel,
        PersistedRequest, PersistedSession, SessionMode, SessionRestoreError, TopicAliasPolicy,
    };
    use bytes::Bytes;
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};
    use std::thread;

    const AUTH_METHOD: &str = "test-method";

    fn build_outgoing_publish(qos: QoS) -> Publish {
        let topic = "hello/world".to_owned();
        let payload = vec![1, 2, 3];

        let mut publish = Publish::new(topic, QoS::AtLeastOnce, payload, None);
        publish.qos = qos;
        publish
    }

    fn publish_properties_with_alias(alias: u16) -> PublishProperties {
        PublishProperties {
            topic_alias: Some(alias),
            ..Default::default()
        }
    }

    fn build_outgoing_publish_with_alias(topic: &str, qos: QoS, alias: u16) -> Publish {
        Publish::new(
            topic,
            qos,
            vec![1, 2, 3],
            Some(publish_properties_with_alias(alias)),
        )
    }

    fn build_incoming_publish(qos: QoS, pkid: u16) -> Publish {
        let topic = "hello/world".to_owned();
        let payload = vec![1, 2, 3];

        let mut publish = Publish::new(topic, QoS::AtLeastOnce, payload, None);
        publish.pkid = pkid;
        publish.qos = qos;
        publish
    }

    fn build_mqttstate() -> MqttState {
        MqttState::builder(u16::MAX).build()
    }

    fn persistent_options() -> MqttOptions {
        MqttOptions::builder("client", "localhost")
            .session_mode(SessionMode::Persistent)
            .build()
    }

    fn build_lru_auto_alias_mqttstate(max_inflight: u16, broker_topic_alias_max: u16) -> MqttState {
        let mut mqtt = MqttState::builder(max_inflight)
            .topic_alias_policy(TopicAliasPolicy::Lru)
            .build();
        mqtt.set_broker_topic_alias_max(broker_topic_alias_max);
        mqtt
    }

    fn assert_publish(packet: Packet, topic: &'static [u8], alias: Option<u16>) {
        match packet {
            Packet::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(topic));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    alias
                );
            }
            packet => panic!("expected publish, got {packet:?}"),
        }
    }

    fn build_auth_mqttstate(authentication_method: Option<&str>) -> MqttState {
        MqttState::builder(10)
            .authentication_method(authentication_method.map(str::to_owned))
            .build()
    }

    fn poison_mutex<T: Send + 'static>(mutex: &Arc<Mutex<T>>) {
        let mutex = Arc::clone(mutex);
        assert!(
            thread::spawn(move || {
                let _guard = mutex
                    .lock()
                    .expect("mutex should not be poisoned before this helper poisons it");
                panic!("poison mutex");
            })
            .join()
            .is_err(),
            "poisoning thread should panic while holding the mutex"
        );
    }

    fn auth_properties(authentication_method: Option<&str>) -> AuthProperties {
        AuthProperties {
            method: authentication_method.map(str::to_owned),
            data: Some(Bytes::from_static(b"auth-data")),
            reason: None,
            user_properties: vec![],
        }
    }

    #[derive(Debug)]
    struct StaticAuthManager {
        response: Result<Option<AuthProperties>, String>,
    }

    impl crate::Authenticator for StaticAuthManager {
        fn start(
            &mut self,
            _context: crate::AuthContext<'_>,
        ) -> Result<Option<AuthProperties>, crate::AuthError> {
            Ok(None)
        }

        fn continue_auth(
            &mut self,
            _context: crate::AuthContext<'_>,
            _auth_prop: Option<AuthProperties>,
        ) -> Result<crate::AuthAction, crate::AuthError> {
            self.response
                .clone()
                .map(|props| props.map_or(crate::AuthAction::Complete, crate::AuthAction::Send))
                .map_err(crate::AuthError::from)
        }

        fn success(
            &mut self,
            _context: crate::AuthContext<'_>,
            _incoming: Option<AuthProperties>,
        ) -> Result<(), crate::AuthError> {
            Ok(())
        }

        fn failure(&mut self, _context: crate::AuthContext<'_>, _error: crate::AuthError) {}
    }

    #[derive(Debug)]
    struct StartAuthManager {
        response: Option<AuthProperties>,
    }

    impl crate::Authenticator for StartAuthManager {
        fn start(
            &mut self,
            _context: crate::AuthContext<'_>,
        ) -> Result<Option<AuthProperties>, crate::AuthError> {
            Ok(self.response.clone())
        }

        fn continue_auth(
            &mut self,
            _context: crate::AuthContext<'_>,
            _auth_prop: Option<AuthProperties>,
        ) -> Result<crate::AuthAction, crate::AuthError> {
            Ok(crate::AuthAction::Complete)
        }

        fn success(
            &mut self,
            _context: crate::AuthContext<'_>,
            _incoming: Option<AuthProperties>,
        ) -> Result<(), crate::AuthError> {
            Ok(())
        }

        fn failure(&mut self, _context: crate::AuthContext<'_>, _error: crate::AuthError) {}
    }

    #[derive(Debug)]
    struct FailingStartAuthManager;

    impl crate::Authenticator for FailingStartAuthManager {
        fn start(
            &mut self,
            _context: crate::AuthContext<'_>,
        ) -> Result<Option<AuthProperties>, crate::AuthError> {
            Err(crate::AuthError::from("start failed"))
        }

        fn continue_auth(
            &mut self,
            _context: crate::AuthContext<'_>,
            _auth_prop: Option<AuthProperties>,
        ) -> Result<crate::AuthAction, crate::AuthError> {
            Ok(crate::AuthAction::Complete)
        }

        fn success(
            &mut self,
            _context: crate::AuthContext<'_>,
            _incoming: Option<AuthProperties>,
        ) -> Result<(), crate::AuthError> {
            Ok(())
        }

        fn failure(&mut self, _context: crate::AuthContext<'_>, _error: crate::AuthError) {}
    }

    fn queue_publish_with_notice(mqtt: &mut MqttState, publish: Publish) -> PublishNotice {
        let (tx, notice) = PublishNoticeTx::new();
        let (packet, flush_notice) = mqtt
            .outgoing_publish_with_notice(publish, Some(tx))
            .unwrap();
        assert!(packet.is_some());
        assert!(flush_notice.is_none());
        notice
    }

    fn replayed_publish_from_pending(pending: Vec<(Request, Option<TrackedNoticeTx>)>) -> Publish {
        pending
            .into_iter()
            .find_map(|(request, _)| match request {
                Request::Publish(publish) => Some(publish),
                _ => None,
            })
            .expect("expected pending publish replay")
    }

    #[test]
    fn new_state_preallocates_event_queue_for_read_batch_bursts() {
        let mqtt = MqttState::builder(10).build();
        assert!(mqtt.events.capacity() >= MqttState::initial_events_capacity());
    }

    #[test]
    fn handle_outgoing_packet_rejects_incoming_only_request_variants() {
        let mut mqtt = build_mqttstate();

        let err = mqtt.handle_outgoing_packet(Request::PingResp).unwrap_err();

        assert!(matches!(err, StateError::InvalidState));
    }

    #[test]
    fn clean_marks_unacked_qos1_publish_dup_for_reconnect_replay() {
        let mut mqtt = build_mqttstate();
        queue_publish_with_notice(&mut mqtt, build_outgoing_publish(QoS::AtLeastOnce));
        mqtt.mark_outgoing_publishes_flush_attempted();

        let replay = replayed_publish_from_pending(mqtt.clean_with_notices());

        assert!(replay.dup);
        assert_eq!(replay.qos, QoS::AtLeastOnce);
    }

    #[test]
    fn clean_preserves_incomplete_incoming_qos2_state_for_session_resume() {
        let mut mqtt = build_mqttstate();
        let mut publish = build_incoming_publish(QoS::ExactlyOnce, 1);

        match mqtt.handle_incoming_publish(&mut publish).unwrap().unwrap() {
            Packet::PubRec(pubrec) => assert_eq!(pubrec.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }

        let pending = mqtt.clean();

        assert!(pending.is_empty());
        assert!(mqtt.incoming_pub.contains(1));
        assert!(mqtt.incoming_pubrec.contains(1));

        let packet = mqtt
            .handle_incoming_pubrel(&PubRel::new(1, None))
            .unwrap()
            .unwrap();
        match packet {
            Packet::PubComp(pubcomp) => assert_eq!(pubcomp.pkid, 1),
            packet => panic!("Invalid network request after session resume: {packet:?}"),
        }
        assert!(!mqtt.incoming_pub.contains(1));
        assert!(!mqtt.incoming_pubrec.contains(1));
    }

    #[test]
    fn reset_session_state_discards_incomplete_incoming_qos2_state() {
        let mut mqtt = build_mqttstate();
        let mut publish = build_incoming_publish(QoS::ExactlyOnce, 1);

        mqtt.handle_incoming_publish(&mut publish).unwrap();
        assert!(mqtt.incoming_pub.contains(1));
        assert!(mqtt.incoming_pubrec.contains(1));

        mqtt.reset_session_state();

        assert!(!mqtt.incoming_pub.contains(1));
        assert!(!mqtt.incoming_pubrec.contains(1));
        let packet = mqtt
            .handle_incoming_pubrel(&PubRel::new(1, None))
            .unwrap()
            .unwrap();
        match packet {
            Packet::PubComp(pubcomp) => {
                assert_eq!(pubcomp.pkid, 1);
                assert_eq!(pubcomp.reason, PubCompReason::PacketIdentifierNotFound);
            }
            packet => panic!("Invalid recovery network request after session reset: {packet:?}"),
        }
    }

    #[test]
    fn persisted_session_restores_qos1_publish_replay() {
        let options = persistent_options();
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        mqtt.mark_outgoing_publishes_flush_attempted();

        let session = mqtt.persisted_session(&options, std::iter::empty());
        let mut restored = build_mqttstate();
        let replay = restored
            .restore_persisted_session(&options, &session)
            .expect("persisted session should restore");

        assert_eq!(replay.len(), 1);
        assert!(restored.packet_identifier_in_use(1));
        match &replay[0] {
            Request::Publish(publish) => {
                assert!(restored.can_send_replayed_publish(publish));
                assert_eq!(publish.pkid, 1);
                assert!(publish.dup);
                assert_eq!(publish.qos, QoS::AtLeastOnce);
                assert_eq!(publish.topic.as_ref(), b"hello/world");
            }
            request => panic!("expected restored publish replay, got {request:?}"),
        }
    }

    #[test]
    fn persisted_session_allows_broker_overridden_session_expiry_interval() {
        let mut options = persistent_options();
        options.set_session_expiry_interval(Some(120));
        let mut session = build_mqttstate().persisted_session(&options, std::iter::empty());
        session.session_expiry_interval = Some(60);
        let mut restored = build_mqttstate();

        let replay = restored
            .restore_persisted_session(&options, &session)
            .expect("server-overridden session expiry should not block pre-connect restore");

        assert!(replay.is_empty());
    }

    #[test]
    fn persisted_session_restores_qos2_pubrel_replay() {
        let options = persistent_options();
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::ExactlyOnce))
            .unwrap();
        mqtt.handle_incoming_pubrec(&PubRec::new(1, None)).unwrap();

        let session = mqtt.persisted_session(&options, std::iter::empty());
        let mut restored = build_mqttstate();
        let mut replay = restored
            .restore_persisted_session(&options, &session)
            .expect("persisted session should restore");

        assert_eq!(replay.len(), 1);
        let packet = restored
            .handle_outgoing_packet(replay.remove(0))
            .expect("restored pubrel replay should be accepted")
            .expect("restored pubrel replay should produce a packet");
        match packet {
            Packet::PubRel(pubrel) => assert_eq!(pubrel.pkid, 1),
            packet => panic!("expected restored PUBREL replay, got {packet:?}"),
        }
    }

    #[test]
    fn persisted_session_replays_control_packets_with_original_identifiers() {
        let options = persistent_options();
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_subscribe(
            Subscribe::new(SubscribeFilter::new("a/b", QoS::AtMostOnce), None),
            None,
        )
        .unwrap();
        mqtt.outgoing_unsubscribe(Unsubscribe::new("c/d", None), None)
            .unwrap();

        let session = mqtt.persisted_session(&options, std::iter::empty());
        let mut restored = build_mqttstate();
        let replay = restored
            .restore_persisted_session(&options, &session)
            .expect("persisted session should restore");

        assert_eq!(replay.len(), 2);
        assert!(restored.packet_identifier_in_use(1));
        assert!(restored.packet_identifier_in_use(2));
        for request in replay {
            let packet = restored
                .handle_replayed_outgoing_packet_with_notice(request, None)
                .expect("restored control replay should be accepted")
                .0
                .expect("restored control replay should produce a packet");
            match packet {
                Packet::Subscribe(subscribe) => assert_eq!(subscribe.pkid, 1),
                Packet::Unsubscribe(unsubscribe) => assert_eq!(unsubscribe.pkid, 2),
                packet => panic!("expected restored control replay, got {packet:?}"),
            }
        }

        assert!(restored.pending_subscribe.contains_key(&1));
        assert!(restored.pending_unsubscribe.contains_key(&2));
    }

    #[test]
    fn persisted_session_omits_unsent_control_requests_without_packet_identifiers() {
        let options = persistent_options();
        let mqtt = build_mqttstate();
        let replay_requests = [
            Request::Subscribe(Subscribe::new(
                SubscribeFilter::new("a/b", QoS::AtMostOnce),
                None,
            )),
            Request::Unsubscribe(Unsubscribe::new("c/d", None)),
        ];

        let session = mqtt.persisted_session(&options, replay_requests.iter());

        assert!(session.replay.is_empty());
        let mut restored = build_mqttstate();
        let replay = restored
            .restore_persisted_session(&options, &session)
            .expect("checkpoint without unsent controls should restore");
        assert!(replay.is_empty());
    }

    #[test]
    fn persisted_session_allows_control_packet_identifiers_above_publish_inflight_limit() {
        let options = MqttOptions::builder("client", "localhost")
            .session_mode(SessionMode::Persistent)
            .outgoing_inflight_upper_limit(1)
            .build();
        let mut subscribe = Subscribe::new(SubscribeFilter::new("a/b", QoS::AtMostOnce), None);
        subscribe.pkid = 2;
        let mut unsubscribe = Unsubscribe::new("c/d", None);
        unsubscribe.pkid = 3;
        let session = PersistedSession {
            format_version: 1,
            client_id: "client".to_owned(),
            clean_start: false,
            session_expiry_interval: options.session_expiry_interval(),
            outgoing_inflight_upper_limit: Some(1),
            ack_mode: PersistedAckMode::Automatic,
            last_pkid: 3,
            last_puback: 0,
            replay: vec![
                PersistedRequest::Subscribe(super::persisted_subscribe(&subscribe)),
                PersistedRequest::Unsubscribe(super::persisted_unsubscribe(&unsubscribe)),
            ],
            incoming_qos2: vec![],
        };
        let mut restored = MqttState::builder(1).build();

        let replay = restored
            .restore_persisted_session(&options, &session)
            .expect("control ids are not constrained by publish inflight limit");

        assert_eq!(replay.len(), 2);
        assert!(matches!(&replay[0], Request::Subscribe(subscribe) if subscribe.pkid == 2));
        assert!(matches!(&replay[1], Request::Unsubscribe(unsubscribe) if unsubscribe.pkid == 3));
    }

    #[test]
    fn persisted_session_rejects_pubrel_replay_above_outgoing_inflight_limit() {
        let options = MqttOptions::builder("client", "localhost")
            .session_mode(SessionMode::Persistent)
            .outgoing_inflight_upper_limit(1)
            .build();
        let session = PersistedSession {
            format_version: 1,
            client_id: "client".to_owned(),
            clean_start: false,
            session_expiry_interval: options.session_expiry_interval(),
            outgoing_inflight_upper_limit: Some(1),
            ack_mode: PersistedAckMode::Automatic,
            last_pkid: 0,
            last_puback: 0,
            replay: vec![PersistedRequest::PubRel(PersistedPubRel { pkid: 2 })],
            incoming_qos2: vec![],
        };
        let mut restored = MqttState::builder(1).build();
        restored.last_pkid = 9;

        let err = restored
            .restore_persisted_session(&options, &session)
            .unwrap_err();

        assert_eq!(
            err,
            SessionRestoreError::OutgoingPacketIdentifierOutOfRange {
                pkid: 2,
                max_outgoing_inflight: 1,
            }
        );
        assert_eq!(restored.last_pkid, 9);
    }

    #[test]
    fn persisted_session_restores_pubrel_replay_when_replay_bitset_is_short() {
        let options = MqttOptions::builder("client", "localhost")
            .session_mode(SessionMode::Persistent)
            .outgoing_inflight_upper_limit(10)
            .build();
        let session = PersistedSession {
            format_version: 1,
            client_id: "client".to_owned(),
            clean_start: false,
            session_expiry_interval: options.session_expiry_interval(),
            outgoing_inflight_upper_limit: Some(10),
            ack_mode: PersistedAckMode::Automatic,
            last_pkid: 0,
            last_puback: 0,
            replay: vec![PersistedRequest::PubRel(PersistedPubRel { pkid: 10 })],
            incoming_qos2: vec![],
        };
        let mut restored = MqttState::builder(10).build();
        restored.outgoing_rel_replay = fixedbitset::FixedBitSet::with_capacity(2);

        let replay = restored
            .restore_persisted_session(&options, &session)
            .expect("persisted session should restore");

        assert!(restored.outgoing_rel_replay.contains(10));
        assert!(matches!(
            &replay[0],
            Request::PubRel(pubrel) if pubrel.pkid == 10
        ));
    }

    #[test]
    fn persisted_session_rejects_last_puback_above_outgoing_inflight_limit() {
        let options = MqttOptions::builder("client", "localhost")
            .session_mode(SessionMode::Persistent)
            .outgoing_inflight_upper_limit(10)
            .build();
        let session = PersistedSession {
            format_version: 1,
            client_id: "client".to_owned(),
            clean_start: false,
            session_expiry_interval: options.session_expiry_interval(),
            outgoing_inflight_upper_limit: Some(10),
            ack_mode: PersistedAckMode::Automatic,
            last_pkid: 0,
            last_puback: 100,
            replay: vec![],
            incoming_qos2: vec![],
        };
        let mut restored = MqttState::builder(10).build();
        restored.last_puback = 4;

        let err = restored
            .restore_persisted_session(&options, &session)
            .unwrap_err();

        assert_eq!(
            err,
            SessionRestoreError::LastPubAckOutOfRange {
                last_puback: 100,
                max_outgoing_inflight: 10,
            }
        );
        assert_eq!(restored.last_puback, 4);
    }

    #[test]
    fn persisted_session_restores_incoming_qos2_state() {
        let options = persistent_options();
        let mut mqtt = build_mqttstate();
        let mut publish = build_incoming_publish(QoS::ExactlyOnce, 7);
        mqtt.handle_incoming_publish(&mut publish).unwrap();

        let session = mqtt.persisted_session(&options, std::iter::empty());
        let mut restored = build_mqttstate();
        let replay = restored
            .restore_persisted_session(&options, &session)
            .expect("persisted session should restore");

        assert!(replay.is_empty());
        let packet = restored
            .handle_incoming_pubrel(&PubRel::new(7, None))
            .unwrap()
            .expect("restored incoming qos2 state should produce pubcomp");
        match packet {
            Packet::PubComp(pubcomp) => assert_eq!(pubcomp.pkid, 7),
            packet => panic!("expected restored PUBCOMP response, got {packet:?}"),
        }
    }

    #[test]
    fn clean_does_not_mark_unflushed_qos1_publish_dup_for_reconnect_replay() {
        let mut mqtt = build_mqttstate();
        queue_publish_with_notice(&mut mqtt, build_outgoing_publish(QoS::AtLeastOnce));

        let replay = replayed_publish_from_pending(mqtt.clean_with_notices());

        assert!(!replay.dup);
        assert_eq!(replay.qos, QoS::AtLeastOnce);
    }

    #[test]
    fn clean_replays_untracked_subscribe_and_unsubscribe_without_notices() {
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_subscribe(
            Subscribe::new(SubscribeFilter::new("a/b", QoS::AtMostOnce), None),
            None,
        )
        .unwrap();
        mqtt.outgoing_unsubscribe(Unsubscribe::new("c/d", None), None)
            .unwrap();

        let pending = mqtt.clean_with_notices();

        assert_eq!(mqtt.pending_subscribe_len(), 0);
        assert_eq!(mqtt.pending_unsubscribe_len(), 0);
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().all(|(_, notice)| notice.is_none()));
        assert!(pending.iter().any(|(request, _)| matches!(
            request,
            Request::Subscribe(subscribe)
                if subscribe.pkid == 1 && subscribe.filters.len() == 1
        )));
        assert!(pending.iter().any(|(request, _)| matches!(
            request,
            Request::Unsubscribe(unsubscribe)
                if unsubscribe.pkid == 2
                    && unsubscribe.filters.len() == 1
                    && unsubscribe.filters[0] == "c/d"
        )));
    }

    #[test]
    fn clone_preserves_pending_control_protocol_state_without_notices() {
        let mut mqtt = build_mqttstate();
        let (sub_tx, _) = SubscribeNoticeTx::new();
        let (unsub_tx, _) = UnsubscribeNoticeTx::new();
        mqtt.outgoing_subscribe(
            Subscribe::new(SubscribeFilter::new("a/b", QoS::AtMostOnce), None),
            Some(sub_tx),
        )
        .unwrap();
        mqtt.outgoing_unsubscribe(Unsubscribe::new("c/d", None), Some(unsub_tx))
            .unwrap();

        let mut cloned = mqtt.clone();
        assert_eq!(cloned.pending_subscribe_len(), 1);
        assert_eq!(cloned.pending_unsubscribe_len(), 1);
        assert!(
            cloned
                .pending_subscribe
                .get(&1)
                .is_some_and(|pending| pending.notice.is_none())
        );
        assert!(
            cloned
                .pending_unsubscribe
                .get(&2)
                .is_some_and(|pending| pending.notice.is_none())
        );

        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.pkid = 1;
        assert!(matches!(
            cloned.outgoing_publish(publish),
            Err(StateError::InvalidState)
        ));

        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.pkid = 2;
        assert!(matches!(
            cloned.outgoing_publish(publish),
            Err(StateError::InvalidState)
        ));

        let suback = SubAck {
            pkid: 1,
            return_codes: vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
            properties: None,
        };
        assert!(
            cloned
                .handle_incoming_packet(Incoming::SubAck(suback))
                .unwrap()
                .is_none()
        );
        let unsuback = UnsubAck {
            pkid: 2,
            reasons: vec![UnsubAckReason::Success],
            properties: None,
        };
        assert!(
            cloned
                .handle_incoming_packet(Incoming::UnsubAck(unsuback))
                .unwrap()
                .is_none()
        );

        assert!(cloned.pending_control_requests_is_empty());
    }

    #[test]
    fn clean_pending_capacity_counts_publish_rel_and_tracked_requests() {
        let mut mqtt = MqttState::builder(10).build();
        mqtt.outgoing_pub[1] = Some(build_outgoing_publish(QoS::AtLeastOnce));
        mqtt.outgoing_pub[2] = Some(build_outgoing_publish(QoS::ExactlyOnce));
        mqtt.outgoing_rel.insert(3);
        mqtt.outgoing_rel.insert(4);

        let filter = SubscribeFilter::new("a/b", QoS::AtMostOnce);
        let (sub_notice, _) = SubscribeNoticeTx::new();
        mqtt.pending_subscribe.insert(
            5,
            PendingSubscribe {
                subscribe: Subscribe::new(filter, None),
                notice: Some(sub_notice),
            },
        );

        let (unsub_notice, _) = UnsubscribeNoticeTx::new();
        mqtt.pending_unsubscribe.insert(
            6,
            PendingUnsubscribe {
                unsubscribe: Unsubscribe::new("a/b", None),
                notice: Some(unsub_notice),
            },
        );

        assert_eq!(mqtt.clean_pending_capacity(), 6);
    }

    #[test]
    fn outbound_diagnostics_reports_initial_state() {
        let mqtt = MqttState::builder(10).build();

        let diagnostics = mqtt.outbound_diagnostics();

        assert_eq!(diagnostics.inflight, 0);
        assert_eq!(diagnostics.max_inflight, 10);
        assert!(!diagnostics.publish_window_full);
        assert_eq!(diagnostics.packet_identifiers_in_use, 0);
        assert_eq!(diagnostics.pending_subscribe, 0);
        assert_eq!(diagnostics.pending_unsubscribe, 0);
        assert_eq!(diagnostics.outgoing_publish, 0);
        assert!(diagnostics.outbound_drained);
    }

    #[test]
    fn outbound_diagnostics_reports_active_protocol_state() {
        let mut mqtt = MqttState::builder(2).build();
        mqtt.outgoing_pub[1] = Some(build_outgoing_publish(QoS::AtLeastOnce));
        let (publish_notice, _) = PublishNoticeTx::new();
        mqtt.outgoing_pub_notice[1] = Some(publish_notice);
        mqtt.outgoing_pub_flush_attempted.insert(1);
        mqtt.outgoing_pub_ack.insert(1);
        mqtt.outgoing_rel.insert(2);
        mqtt.outgoing_rel_replay.insert(2);
        let (pubrel_notice, _) = PublishNoticeTx::new();
        mqtt.outgoing_rel_notice[2] = Some(pubrel_notice);
        mqtt.inflight = 2;
        mqtt.collision = Some(build_outgoing_publish(QoS::AtLeastOnce));
        let (collision_notice, _) = PublishNoticeTx::new();
        mqtt.collision_notice = Some(collision_notice);
        mqtt.incoming_puback.insert(3);
        mqtt.incoming_pub.insert(4);
        mqtt.incoming_pubrec.insert(4);

        let filter = SubscribeFilter::new("a/b", QoS::AtMostOnce);
        let (sub_notice, _) = SubscribeNoticeTx::new();
        mqtt.pending_subscribe.insert(
            5,
            PendingSubscribe {
                subscribe: Subscribe::new(filter, None),
                notice: Some(sub_notice),
            },
        );
        let (unsub_notice, _) = UnsubscribeNoticeTx::new();
        mqtt.pending_unsubscribe.insert(
            6,
            PendingUnsubscribe {
                unsubscribe: Unsubscribe::new("a/b", None),
                notice: Some(unsub_notice),
            },
        );
        mqtt.rebuild_outbound_pkid_index();

        let diagnostics = mqtt.outbound_diagnostics();

        assert_eq!(diagnostics.inflight, 2);
        assert_eq!(diagnostics.max_inflight, 2);
        assert!(diagnostics.publish_window_full);
        assert_eq!(diagnostics.packet_identifiers_in_use, 4);
        assert!(diagnostics.collision);
        assert!(diagnostics.collision_notice);
        assert_eq!(diagnostics.pending_subscribe, 1);
        assert_eq!(diagnostics.pending_unsubscribe, 1);
        assert_eq!(diagnostics.outgoing_publish, 1);
        assert_eq!(diagnostics.outgoing_publish_notices, 1);
        assert_eq!(diagnostics.outgoing_pub_flush_attempted, 1);
        assert_eq!(diagnostics.outgoing_puback_waiting, 1);
        assert_eq!(diagnostics.outgoing_pubrel, 1);
        assert_eq!(diagnostics.outgoing_pubrel_replay, 1);
        assert_eq!(diagnostics.outgoing_pubrel_notices, 1);
        assert_eq!(diagnostics.incoming_puback, 1);
        assert_eq!(diagnostics.incoming_pub, 1);
        assert_eq!(diagnostics.incoming_pubrec, 1);
        assert!(!diagnostics.outbound_drained);
        assert!(mqtt.outbound_drain_diagnostics().contains("inflight=2"));
    }

    #[test]
    fn tracked_request_len_helpers_report_counts() {
        let mut mqtt = MqttState::builder(10).build();
        let filter = SubscribeFilter::new("a/b", QoS::AtMostOnce);
        let (sub_notice, _) = SubscribeNoticeTx::new();
        mqtt.pending_subscribe.insert(
            5,
            PendingSubscribe {
                subscribe: Subscribe::new(filter, None),
                notice: Some(sub_notice),
            },
        );
        let (unsub_notice, _) = UnsubscribeNoticeTx::new();
        mqtt.pending_unsubscribe.insert(
            6,
            PendingUnsubscribe {
                unsubscribe: Unsubscribe::new("a/b", None),
                notice: Some(unsub_notice),
            },
        );

        assert_eq!(mqtt.tracked_subscribe_len(), 1);
        assert_eq!(mqtt.tracked_unsubscribe_len(), 1);
        assert!(!mqtt.tracked_requests_is_empty());

        mqtt.drain_tracked_requests_as_failed(NoticeFailureReason::SessionReset);
        assert!(mqtt.tracked_requests_is_empty());
    }

    #[test]
    fn tracked_request_len_helpers_ignore_untracked_pending_requests() {
        let mut mqtt = MqttState::builder(10).build();
        let (sub_notice, _) = SubscribeNoticeTx::new();
        mqtt.pending_subscribe.insert(
            5,
            PendingSubscribe {
                subscribe: Subscribe::new(SubscribeFilter::new("tracked", QoS::AtMostOnce), None),
                notice: Some(sub_notice),
            },
        );
        mqtt.pending_subscribe.insert(
            6,
            PendingSubscribe {
                subscribe: Subscribe::new(SubscribeFilter::new("untracked", QoS::AtMostOnce), None),
                notice: None,
            },
        );
        mqtt.pending_unsubscribe.insert(
            7,
            PendingUnsubscribe {
                unsubscribe: Unsubscribe::new("untracked", None),
                notice: None,
            },
        );

        assert_eq!(mqtt.pending_subscribe_len(), 2);
        assert_eq!(mqtt.pending_unsubscribe_len(), 1);
        assert_eq!(mqtt.tracked_subscribe_len(), 1);
        assert_eq!(mqtt.tracked_unsubscribe_len(), 0);
        assert!(!mqtt.tracked_requests_is_empty());
    }

    #[test]
    fn drain_tracked_requests_as_failed_reports_session_reset_and_returns_count() {
        let mut mqtt = MqttState::builder(10).build();
        let filter = SubscribeFilter::new("a/b", QoS::AtMostOnce);
        let (sub_notice_tx, sub_notice) = SubscribeNoticeTx::new();
        mqtt.pending_subscribe.insert(
            5,
            PendingSubscribe {
                subscribe: Subscribe::new(filter, None),
                notice: Some(sub_notice_tx),
            },
        );
        let (unsub_notice_tx, unsub_notice) = UnsubscribeNoticeTx::new();
        mqtt.pending_unsubscribe.insert(
            6,
            PendingUnsubscribe {
                unsubscribe: Unsubscribe::new("a/b", None),
                notice: Some(unsub_notice_tx),
            },
        );

        let drained = mqtt.drain_tracked_requests_as_failed(NoticeFailureReason::SessionReset);

        assert_eq!(drained, 2);
        assert!(mqtt.tracked_requests_is_empty());
        assert_eq!(
            sub_notice.wait().unwrap_err(),
            SubscribeNoticeError::SessionReset
        );
        assert_eq!(
            unsub_notice.wait().unwrap_err(),
            UnsubscribeNoticeError::SessionReset
        );
    }

    #[test]
    fn drain_tracked_requests_as_failed_counts_untracked_requests_it_clears() {
        let mut mqtt = MqttState::builder(10).build();
        mqtt.pending_subscribe.insert(
            5,
            PendingSubscribe {
                subscribe: Subscribe::new(SubscribeFilter::new("a/b", QoS::AtMostOnce), None),
                notice: None,
            },
        );
        mqtt.pending_unsubscribe.insert(
            6,
            PendingUnsubscribe {
                unsubscribe: Unsubscribe::new("a/b", None),
                notice: None,
            },
        );

        let drained = mqtt.drain_tracked_requests_as_failed(NoticeFailureReason::SessionReset);

        assert_eq!(drained, 2);
        assert!(mqtt.pending_control_requests_is_empty());
        assert!(mqtt.tracked_requests_is_empty());
    }

    #[test]
    fn drain_tracked_requests_as_failed_is_noop_when_empty() {
        let mut mqtt = MqttState::builder(10).build();
        let drained = mqtt.drain_tracked_requests_as_failed(NoticeFailureReason::SessionReset);

        assert_eq!(drained, 0);
        assert!(mqtt.tracked_requests_is_empty());
    }

    #[test]
    fn tracked_puback_returns_ack_and_preserves_incoming_event() {
        let mut mqtt = build_mqttstate();
        let notice = queue_publish_with_notice(&mut mqtt, build_outgoing_publish(QoS::AtLeastOnce));
        mqtt.events.clear();

        let mut puback = PubAck::new(1, None);
        puback.reason = PubAckReason::NoMatchingSubscribers;
        puback.properties = Some(PubAckProperties {
            reason_string: Some("accepted without subscribers".to_owned()),
            user_properties: vec![("k".to_owned(), "v".to_owned())],
        });
        assert!(
            mqtt.handle_incoming_packet(Incoming::PubAck(puback.clone()))
                .unwrap()
                .is_none()
        );

        assert_eq!(notice.wait(), Ok(PublishResult::Qos1(puback.clone())));
        assert_eq!(
            mqtt.events.pop_front(),
            Some(Event::Incoming(Packet::PubAck(puback)))
        );
    }

    #[test]
    fn effect_path_defers_tracked_puback_notice() {
        let mut mqtt = build_mqttstate();
        let notice = queue_publish_with_notice(&mut mqtt, build_outgoing_publish(QoS::AtLeastOnce));
        mqtt.events.clear();

        let puback = PubAck::new(1, None);
        let effects = mqtt
            .handle_incoming_packet_with_effects(Incoming::PubAck(puback.clone()))
            .unwrap();

        assert!(effects.outgoing.is_none());
        assert_eq!(effects.notices.len(), 1);
        assert_eq!(
            mqtt.events.pop_front(),
            Some(Event::Incoming(Packet::PubAck(puback.clone())))
        );

        assert!(effects.complete_notices().is_none());
        assert_eq!(notice.wait(), Ok(PublishResult::Qos1(puback)));
    }

    #[test]
    fn tracked_suback_returns_ack_with_properties_and_preserves_incoming_event() {
        let mut mqtt = build_mqttstate();
        let (tx, notice) = SubscribeNoticeTx::new();
        mqtt.outgoing_subscribe(
            Subscribe::new(SubscribeFilter::new("a/b", QoS::AtMostOnce), None),
            Some(tx),
        )
        .unwrap();
        mqtt.events.clear();

        let suback = SubAck {
            pkid: 1,
            return_codes: vec![SubscribeReasonCode::Unspecified],
            properties: Some(SubAckProperties {
                reason_string: Some("denied".to_owned()),
                user_properties: vec![("scope".to_owned(), "missing".to_owned())],
            }),
        };
        assert!(
            mqtt.handle_incoming_packet(Incoming::SubAck(suback.clone()))
                .unwrap()
                .is_none()
        );

        assert_eq!(notice.wait(), Ok(suback.clone()));
        assert_eq!(
            mqtt.events.pop_front(),
            Some(Event::Incoming(Packet::SubAck(suback)))
        );
    }

    #[test]
    fn effect_path_defers_tracked_suback_notice() {
        let mut mqtt = build_mqttstate();
        let (tx, notice) = SubscribeNoticeTx::new();
        mqtt.outgoing_subscribe(
            Subscribe::new(SubscribeFilter::new("a/b", QoS::AtMostOnce), None),
            Some(tx),
        )
        .unwrap();
        mqtt.events.clear();

        let suback = SubAck {
            pkid: 1,
            return_codes: vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
            properties: None,
        };
        let effects = mqtt
            .handle_incoming_packet_with_effects(Incoming::SubAck(suback.clone()))
            .unwrap();

        assert!(effects.outgoing.is_none());
        assert_eq!(effects.notices.len(), 1);
        assert_eq!(
            mqtt.events.pop_front(),
            Some(Event::Incoming(Packet::SubAck(suback.clone())))
        );

        assert!(effects.complete_notices().is_none());
        assert_eq!(notice.wait(), Ok(suback));
    }

    #[test]
    fn untracked_suback_returns_ack_with_properties_and_preserves_incoming_event() {
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_subscribe(
            Subscribe::new(SubscribeFilter::new("a/b", QoS::AtMostOnce), None),
            None,
        )
        .unwrap();
        mqtt.events.clear();

        let suback = SubAck {
            pkid: 1,
            return_codes: vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
            properties: Some(SubAckProperties {
                reason_string: Some("ok".to_owned()),
                user_properties: vec![("scope".to_owned(), "present".to_owned())],
            }),
        };
        assert!(
            mqtt.handle_incoming_packet(Incoming::SubAck(suback.clone()))
                .unwrap()
                .is_none()
        );

        assert_eq!(mqtt.tracked_subscribe_len(), 0);
        assert_eq!(
            mqtt.events.pop_front(),
            Some(Event::Incoming(Packet::SubAck(suback)))
        );
    }

    #[test]
    fn tracked_unsuback_returns_ack_with_properties_and_preserves_incoming_event() {
        let mut mqtt = build_mqttstate();
        let (tx, notice) = UnsubscribeNoticeTx::new();
        mqtt.outgoing_unsubscribe(Unsubscribe::new("a/b", None), Some(tx))
            .unwrap();
        mqtt.events.clear();

        let unsuback = UnsubAck {
            pkid: 1,
            reasons: vec![UnsubAckReason::UnspecifiedError],
            properties: Some(UnsubAckProperties {
                reason_string: Some("failed".to_owned()),
                user_properties: vec![("detail".to_owned(), "x".to_owned())],
            }),
        };
        assert!(
            mqtt.handle_incoming_packet(Incoming::UnsubAck(unsuback.clone()))
                .unwrap()
                .is_none()
        );

        assert_eq!(notice.wait(), Ok(unsuback.clone()));
        assert_eq!(
            mqtt.events.pop_front(),
            Some(Event::Incoming(Packet::UnsubAck(unsuback)))
        );
    }

    #[test]
    fn effect_path_defers_tracked_unsuback_notice() {
        let mut mqtt = build_mqttstate();
        let (tx, notice) = UnsubscribeNoticeTx::new();
        mqtt.outgoing_unsubscribe(Unsubscribe::new("a/b", None), Some(tx))
            .unwrap();
        mqtt.events.clear();

        let unsuback = UnsubAck {
            pkid: 1,
            reasons: vec![UnsubAckReason::Success],
            properties: None,
        };
        let effects = mqtt
            .handle_incoming_packet_with_effects(Incoming::UnsubAck(unsuback.clone()))
            .unwrap();

        assert!(effects.outgoing.is_none());
        assert_eq!(effects.notices.len(), 1);
        assert_eq!(
            mqtt.events.pop_front(),
            Some(Event::Incoming(Packet::UnsubAck(unsuback.clone())))
        );

        assert!(effects.complete_notices().is_none());
        assert_eq!(notice.wait(), Ok(unsuback));
    }

    #[test]
    fn untracked_unsuback_returns_ack_with_properties_and_preserves_incoming_event() {
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_unsubscribe(Unsubscribe::new("a/b", None), None)
            .unwrap();
        mqtt.events.clear();

        let unsuback = UnsubAck {
            pkid: 1,
            reasons: vec![UnsubAckReason::Success],
            properties: Some(UnsubAckProperties {
                reason_string: Some("ok".to_owned()),
                user_properties: vec![("detail".to_owned(), "x".to_owned())],
            }),
        };
        assert!(
            mqtt.handle_incoming_packet(Incoming::UnsubAck(unsuback.clone()))
                .unwrap()
                .is_none()
        );

        assert_eq!(mqtt.tracked_unsubscribe_len(), 0);
        assert_eq!(
            mqtt.events.pop_front(),
            Some(Event::Incoming(Packet::UnsubAck(unsuback)))
        );
    }

    #[test]
    fn control_acks_above_publish_tracking_capacity_complete_without_publish_frontier() {
        let mut mqtt = MqttState::builder(10).build();
        let pkid = 11;

        mqtt.pending_subscribe.insert(
            pkid,
            PendingSubscribe {
                subscribe: Subscribe::new(SubscribeFilter::new("a/b", QoS::AtMostOnce), None),
                notice: None,
            },
        );
        mqtt.pending_unsubscribe.insert(
            pkid + 1,
            PendingUnsubscribe {
                unsubscribe: Unsubscribe::new("c/d", None),
                notice: None,
            },
        );

        assert!(mqtt.outgoing_pub_ack.len() <= usize::from(pkid));
        mqtt.handle_incoming_suback(&SubAck {
            pkid,
            return_codes: vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
            properties: None,
        })
        .unwrap();
        mqtt.handle_incoming_unsuback(&UnsubAck {
            pkid: pkid + 1,
            reasons: vec![UnsubAckReason::Success],
            properties: None,
        })
        .unwrap();

        assert!(mqtt.outbound_requests_drained());
        assert!(mqtt.outgoing_pub_ack.ones().next().is_none());
        assert_eq!(mqtt.last_puback, 0);
    }

    #[test]
    fn incoming_suback_with_untracked_pkid_is_unsolicited() {
        let mut mqtt = build_mqttstate();

        let suback = SubAck {
            pkid: 5,
            return_codes: vec![SubscribeReasonCode::Unspecified],
            properties: None,
        };
        let got = mqtt
            .handle_incoming_packet(Incoming::SubAck(suback))
            .unwrap_err();

        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 5),
            e => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn incoming_suback_with_too_few_return_codes_is_protocol_violation() {
        let mut mqtt = build_mqttstate();

        let subscribe = Subscribe::new_many(
            [
                SubscribeFilter::new("a/b", QoS::AtMostOnce),
                SubscribeFilter::new("c/d", QoS::AtLeastOnce),
            ],
            None,
        );
        mqtt.outgoing_subscribe(subscribe, None).unwrap();
        mqtt.events.clear();

        let suback = SubAck {
            pkid: 1,
            return_codes: vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
            properties: None,
        };
        let got = mqtt
            .handle_incoming_packet(Incoming::SubAck(suback))
            .unwrap_err();

        assert!(matches!(
            got,
            StateError::ProtocolViolation(ProtocolViolation::SubAckReturnCodeCountMismatch {
                pkid: 1,
                expected: 2,
                actual: 1
            })
        ));
        assert_eq!(mqtt.pending_subscribe_len(), 1);
        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn incoming_suback_with_too_many_return_codes_is_protocol_violation() {
        let mut mqtt = build_mqttstate();

        let subscribe = Subscribe::new(SubscribeFilter::new("a/b", QoS::AtMostOnce), None);
        mqtt.outgoing_subscribe(subscribe, None).unwrap();
        mqtt.events.clear();

        let suback = SubAck {
            pkid: 1,
            return_codes: vec![
                SubscribeReasonCode::Success(QoS::AtMostOnce),
                SubscribeReasonCode::Success(QoS::AtLeastOnce),
            ],
            properties: None,
        };
        let got = mqtt
            .handle_incoming_packet(Incoming::SubAck(suback))
            .unwrap_err();

        assert!(matches!(
            got,
            StateError::ProtocolViolation(ProtocolViolation::SubAckReturnCodeCountMismatch {
                pkid: 1,
                expected: 1,
                actual: 2
            })
        ));
        assert_eq!(mqtt.pending_subscribe_len(), 1);
        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn incoming_unsuback_with_untracked_pkid_is_unsolicited() {
        let mut mqtt = build_mqttstate();

        let unsuback = UnsubAck {
            pkid: 7,
            reasons: vec![UnsubAckReason::Success],
            properties: None,
        };
        let got = mqtt
            .handle_incoming_packet(Incoming::UnsubAck(unsuback))
            .unwrap_err();

        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 7),
            e => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn incoming_unsuback_with_too_few_reason_codes_is_protocol_violation() {
        let mut mqtt = build_mqttstate();

        let mut unsubscribe = Unsubscribe::new("a/b", None);
        unsubscribe.filters.push("c/d".to_owned());
        mqtt.outgoing_unsubscribe(unsubscribe, None).unwrap();
        mqtt.events.clear();

        let unsuback = UnsubAck {
            pkid: 1,
            reasons: vec![UnsubAckReason::Success],
            properties: None,
        };
        let got = mqtt
            .handle_incoming_packet(Incoming::UnsubAck(unsuback))
            .unwrap_err();

        assert!(matches!(
            got,
            StateError::ProtocolViolation(ProtocolViolation::UnsubAckReasonCodeCountMismatch {
                pkid: 1,
                expected: 2,
                actual: 1
            })
        ));
        assert_eq!(mqtt.pending_unsubscribe_len(), 1);
        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn incoming_unsuback_with_too_many_reason_codes_is_protocol_violation() {
        let mut mqtt = build_mqttstate();

        let unsubscribe = Unsubscribe::new("a/b", None);
        mqtt.outgoing_unsubscribe(unsubscribe, None).unwrap();
        mqtt.events.clear();

        let unsuback = UnsubAck {
            pkid: 1,
            reasons: vec![
                UnsubAckReason::Success,
                UnsubAckReason::NoSubscriptionExisted,
            ],
            properties: None,
        };
        let got = mqtt
            .handle_incoming_packet(Incoming::UnsubAck(unsuback))
            .unwrap_err();

        assert!(matches!(
            got,
            StateError::ProtocolViolation(ProtocolViolation::UnsubAckReasonCodeCountMismatch {
                pkid: 1,
                expected: 1,
                actual: 2
            })
        ));
        assert_eq!(mqtt.pending_unsubscribe_len(), 1);
        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn incoming_pubrec_with_untracked_pkid_is_unsolicited() {
        let mut mqtt = build_mqttstate();

        let got = mqtt
            .handle_incoming_pubrec(&PubRec::new(5, None))
            .unwrap_err();

        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 5),
            e => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn incoming_pubrel_with_untracked_pkid_sends_pubcomp_with_packet_identifier_not_found() {
        let mut mqtt = build_mqttstate();

        let packet = mqtt
            .handle_incoming_pubrel(&PubRel::new(7, None))
            .unwrap()
            .unwrap();

        match packet {
            Packet::PubComp(pubcomp) => {
                assert_eq!(pubcomp.pkid, 7);
                assert_eq!(pubcomp.reason, PubCompReason::PacketIdentifierNotFound);
            }
            packet => panic!("Invalid network request: {packet:?}"),
        }
    }

    #[test]
    fn incoming_pubcomp_with_untracked_pkid_is_unsolicited() {
        let mut mqtt = build_mqttstate();

        let got = mqtt
            .handle_incoming_pubcomp(&PubComp::new(11, None))
            .unwrap_err();

        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 11),
            e => panic!("Unexpected error: {e}"),
        }
    }

    fn build_connack_with_receive_max(receive_max: u16) -> ConnAck {
        build_connack(Some(receive_max), None)
    }

    fn build_connack(receive_max: Option<u16>, retain_available: Option<u8>) -> ConnAck {
        ConnAck {
            session_present: false,
            code: ConnectReturnCode::Success,
            properties: Some(ConnAckProperties {
                session_expiry_interval: None,
                receive_max,
                max_qos: None,
                retain_available,
                max_packet_size: None,
                assigned_client_identifier: None,
                topic_alias_max: None,
                reason_string: None,
                user_properties: vec![],
                wildcard_subscription_available: None,
                subscription_identifiers_available: None,
                shared_subscription_available: None,
                server_keep_alive: None,
                response_information: None,
                server_reference: None,
                authentication_method: None,
                authentication_data: None,
            }),
        }
    }

    #[test]
    fn retained_publish_is_rejected_when_broker_does_not_support_retained_messages() {
        let mut mqtt = build_mqttstate();
        mqtt.handle_incoming_packet(Incoming::ConnAck(build_connack(None, Some(0))))
            .unwrap();
        mqtt.events.clear();
        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.retain = true;

        let err = mqtt.outgoing_publish(publish).unwrap_err();

        assert!(matches!(err, StateError::RetainNotSupported));
        assert_eq!(mqtt.last_pkid, 0);
        assert_eq!(mqtt.inflight, 0);
        assert!(mqtt.outgoing_pub.iter().all(Option::is_none));
        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn non_retained_publish_is_allowed_when_broker_does_not_support_retained_messages() {
        let mut mqtt = build_mqttstate();
        mqtt.handle_incoming_packet(Incoming::ConnAck(build_connack(None, Some(0))))
            .unwrap();
        mqtt.events.clear();
        let publish = build_outgoing_publish(QoS::AtMostOnce);

        let packet = mqtt.outgoing_publish(publish).unwrap();

        assert!(matches!(
            packet,
            Some(Packet::Publish(Publish { retain: false, .. }))
        ));
    }

    #[test]
    fn retained_publish_is_allowed_when_retain_available_is_absent_or_one() {
        for retain_available in [None, Some(1)] {
            let mut mqtt = build_mqttstate();
            mqtt.handle_incoming_packet(Incoming::ConnAck(build_connack(None, retain_available)))
                .unwrap();
            mqtt.events.clear();
            let mut publish = build_outgoing_publish(QoS::AtMostOnce);
            publish.retain = true;

            let packet = mqtt.outgoing_publish(publish).unwrap();

            assert!(matches!(
                packet,
                Some(Packet::Publish(Publish { retain: true, .. }))
            ));
        }
    }

    #[test]
    fn retain_available_resets_between_network_connections() {
        let mut mqtt = build_mqttstate();
        mqtt.handle_incoming_packet(Incoming::ConnAck(build_connack(None, Some(0))))
            .unwrap();
        assert!(!mqtt.retain_available);

        mqtt.reset_connection_scoped_state();

        assert!(mqtt.retain_available);
    }

    #[test]
    fn retained_publish_replay_is_rejected_by_new_broker_capability() {
        let mut mqtt = build_mqttstate();
        mqtt.handle_incoming_packet(Incoming::ConnAck(build_connack(None, None)))
            .unwrap();
        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.retain = true;
        mqtt.outgoing_publish(publish).unwrap();

        let mut pending = mqtt.clean();
        mqtt.handle_incoming_packet(Incoming::ConnAck(build_connack(None, Some(0))))
            .unwrap();
        mqtt.events.clear();
        let replay = pending.pop().expect("retained publish should be pending");

        let err = mqtt.handle_outgoing_packet(replay).unwrap_err();

        assert!(matches!(err, StateError::RetainNotSupported));
        assert_eq!(mqtt.inflight, 0);
        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn connack_receive_max_can_grow_tracking_capacity_after_previous_shrink() {
        let mut mqtt = MqttState::builder(10).build();
        mqtt.handle_incoming_packet(Incoming::ConnAck(build_connack_with_receive_max(4)))
            .unwrap();
        mqtt.reconcile_outgoing_tracking_capacity(true);
        assert_eq!(mqtt.outgoing_pub.len(), 5);

        mqtt.reset_connection_scoped_state();
        mqtt.handle_incoming_packet(Incoming::ConnAck(build_connack_with_receive_max(9)))
            .unwrap();
        assert_eq!(mqtt.outgoing_pub.len(), 10);
        assert_eq!(mqtt.outgoing_pub_notice.len(), 10);
        assert_eq!(mqtt.outgoing_rel_notice.len(), 10);
        assert_eq!(mqtt.outgoing_pub_ack.len(), 10);
        assert_eq!(mqtt.outgoing_rel.len(), 10);
    }

    #[test]
    fn connack_receive_max_shrinks_when_tracking_is_empty_and_pending_is_empty() {
        let mut mqtt = MqttState::builder(10).build();
        mqtt.last_pkid = 9;
        mqtt.last_puback = 8;

        mqtt.handle_incoming_packet(Incoming::ConnAck(build_connack_with_receive_max(3)))
            .unwrap();
        assert_eq!(mqtt.outgoing_pub.len(), 11);

        mqtt.reconcile_outgoing_tracking_capacity(true);
        assert_eq!(mqtt.outgoing_pub.len(), 4);
        assert_eq!(mqtt.outgoing_pub_notice.len(), 4);
        assert_eq!(mqtt.outgoing_rel_notice.len(), 4);
        assert_eq!(mqtt.outgoing_pub_ack.len(), 4);
        assert_eq!(mqtt.outgoing_rel.len(), 4);
        assert_eq!(mqtt.last_pkid, 0);
        assert_eq!(mqtt.last_puback, 0);
    }

    #[test]
    fn connack_resets_connection_scoped_alias_state_when_topic_alias_maximum_is_omitted() {
        let mut mqtt = MqttState::builder(10).build();
        mqtt.set_broker_topic_alias_max(10);
        mqtt.outgoing_publish(build_outgoing_publish_with_alias(
            "hello/replay",
            QoS::AtMostOnce,
            2,
        ))
        .unwrap();

        mqtt.handle_incoming_packet(Incoming::ConnAck(build_connack_with_receive_max(5)))
            .unwrap();

        assert_eq!(mqtt.broker_topic_alias_max(), 0);
        let mut replay = build_outgoing_publish_with_alias("", QoS::AtLeastOnce, 2);
        let mut replay_topic_aliases = mqtt.replay_topic_aliases();
        assert_eq!(
            MqttState::prepare_publish_for_replay_with_aliases(
                &mut replay,
                &mut replay_topic_aliases
            )
            .unwrap_err(),
            PublishNoticeError::TopicAliasReplayUnavailable(2)
        );
    }

    #[test]
    fn connack_receive_max_does_not_shrink_when_tracking_is_non_empty() {
        let mut mqtt = MqttState::builder(10).build();
        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.pkid = 8;
        mqtt.outgoing_pub[8] = Some(publish);
        mqtt.inflight = 1;

        mqtt.handle_incoming_packet(Incoming::ConnAck(build_connack_with_receive_max(3)))
            .unwrap();
        mqtt.reconcile_outgoing_tracking_capacity(true);

        assert_eq!(mqtt.outgoing_pub.len(), 11);
        assert_eq!(mqtt.outgoing_rel_notice.len(), 11);
    }

    #[test]
    fn duplicate_connack_after_connection_establishment_is_protocol_error() {
        let mut mqtt = MqttState::builder(10).build();

        mqtt.handle_incoming_packet(Incoming::ConnAck(build_connack_with_receive_max(4)))
            .unwrap();
        mqtt.events.clear();

        let err = mqtt
            .handle_incoming_packet(Incoming::ConnAck(build_connack_with_receive_max(4)))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::ProtocolViolation(ProtocolViolation::DuplicateConnAck)
        ));
        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn refused_connack_is_terminal_error_and_preserves_incoming_event() {
        let mut mqtt = MqttState::builder(10).build();
        let connack = ConnAck {
            session_present: false,
            code: ConnectReturnCode::BadUserNamePassword,
            properties: None,
        };

        let err = mqtt
            .handle_incoming_packet(Incoming::ConnAck(connack.clone()))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::ConnFail {
                reason: ConnectReturnCode::BadUserNamePassword
            }
        ));
        assert_eq!(
            mqtt.events.pop_front(),
            Some(Event::Incoming(Incoming::ConnAck(connack)))
        );
    }

    #[test]
    fn inbound_connect_after_connection_establishment_is_protocol_error() {
        let mut mqtt = build_mqttstate();
        let connect = Connect {
            keep_alive: 30,
            client_id: "client".to_owned(),
            clean_start: true,
            properties: None,
        };

        let err = mqtt
            .handle_incoming_packet(Incoming::Connect(connect, None, ConnectAuth::None))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::ProtocolViolation(ProtocolViolation::UnexpectedIncomingPacket(
                PacketType::Connect
            ))
        ));
        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn inbound_pingreq_after_connection_establishment_is_protocol_error() {
        let mut mqtt = build_mqttstate();

        let err = mqtt.handle_incoming_packet(Incoming::PingReq).unwrap_err();

        assert!(matches!(
            err,
            StateError::ProtocolViolation(ProtocolViolation::UnexpectedIncomingPacket(
                PacketType::PingReq
            ))
        ));
        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn inbound_subscribe_after_connection_establishment_is_protocol_error() {
        let mut mqtt = build_mqttstate();
        let mut subscribe =
            Subscribe::new(SubscribeFilter::new("topic/one", QoS::AtLeastOnce), None);
        subscribe.pkid = 1;

        let err = mqtt
            .handle_incoming_packet(Incoming::Subscribe(subscribe))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::ProtocolViolation(ProtocolViolation::UnexpectedIncomingPacket(
                PacketType::Subscribe
            ))
        ));
        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn inbound_unsubscribe_after_connection_establishment_is_protocol_error() {
        let mut mqtt = build_mqttstate();
        let mut unsubscribe = Unsubscribe::new("topic/one", None);
        unsubscribe.pkid = 1;

        let err = mqtt
            .handle_incoming_packet(Incoming::Unsubscribe(unsubscribe))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::ProtocolViolation(ProtocolViolation::UnexpectedIncomingPacket(
                PacketType::Unsubscribe
            ))
        ));
        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn clone_preserves_current_tracking_queue_lengths() {
        let mut mqtt = MqttState::builder(10).build();
        mqtt.handle_incoming_packet(Incoming::ConnAck(build_connack_with_receive_max(3)))
            .unwrap();
        mqtt.reconcile_outgoing_tracking_capacity(true);

        let cloned = mqtt.clone();
        assert_eq!(cloned.outgoing_pub.len(), 4);
        assert_eq!(cloned.outgoing_pub_notice.len(), 4);
        assert_eq!(cloned.outgoing_rel_notice.len(), 4);
    }

    #[test]
    fn next_pkid_increments_as_expected() {
        let mut mqtt = build_mqttstate();

        for i in 1..=100 {
            let pkid = mqtt.next_pkid().unwrap();

            // loops between 0-99. % 100 == 0 implies border
            let expected = i % 100;
            if expected == 0 {
                break;
            }

            assert_eq!(expected, pkid);
        }
    }

    #[test]
    fn can_send_publish_searches_free_pkid_after_control_ids_pass_inflight_limit() {
        let mut mqtt = MqttState::builder(4).build();
        let mut active_publish = build_outgoing_publish(QoS::AtLeastOnce);
        active_publish.pkid = 1;
        mqtt.outgoing_pub[1] = Some(active_publish);
        mqtt.inflight = 1;
        mqtt.last_pkid = 5;
        mqtt.rebuild_outbound_pkid_index();

        assert!(mqtt.can_send_publish(&build_outgoing_publish(QoS::AtLeastOnce)));

        let packet = mqtt
            .outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap()
            .unwrap();
        match packet {
            Packet::Publish(publish) => assert_eq!(publish.pkid, 2),
            packet => panic!("Unexpected packet: {packet:?}"),
        }
    }

    #[test]
    fn next_pkid_returns_none_when_all_publish_packet_identifiers_are_in_use() {
        let mut mqtt = MqttState::builder(4).build();

        for pkid in 1..=4 {
            let filter = SubscribeFilter::new("a/b", QoS::AtMostOnce);
            let (notice, _) = SubscribeNoticeTx::new();
            mqtt.pending_subscribe.insert(
                pkid,
                PendingSubscribe {
                    subscribe: Subscribe::new(filter, None),
                    notice: Some(notice),
                },
            );
        }
        mqtt.rebuild_outbound_pkid_index();

        assert!(!mqtt.can_send_publish(&build_outgoing_publish(QoS::AtLeastOnce)));
        assert!(mqtt.next_pkid().is_none());
        assert!(matches!(
            mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce)),
            Err(StateError::InvalidState)
        ));
    }

    #[test]
    fn outgoing_publish_rejects_packet_identifier_used_by_non_publish_flow() {
        let mut mqtt = MqttState::builder(10).build();
        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.pkid = 3;

        let filter = SubscribeFilter::new("a/b", QoS::AtMostOnce);
        let (notice, _) = SubscribeNoticeTx::new();
        mqtt.pending_subscribe.insert(
            3,
            PendingSubscribe {
                subscribe: Subscribe::new(filter, None),
                notice: Some(notice),
            },
        );

        assert!(matches!(
            mqtt.outgoing_publish(publish),
            Err(StateError::InvalidState)
        ));
        assert!(mqtt.outgoing_pub[3].is_none());

        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.pkid = 4;
        let (notice, _) = UnsubscribeNoticeTx::new();
        mqtt.pending_unsubscribe.insert(
            4,
            PendingUnsubscribe {
                unsubscribe: Unsubscribe::new("a/b", None),
                notice: Some(notice),
            },
        );

        assert!(matches!(
            mqtt.outgoing_publish(publish),
            Err(StateError::InvalidState)
        ));
        assert!(mqtt.outgoing_pub[4].is_none());

        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.pkid = 5;
        mqtt.outgoing_rel.insert(5);

        assert!(matches!(
            mqtt.outgoing_publish(publish),
            Err(StateError::InvalidState)
        ));
        assert!(mqtt.outgoing_pub[5].is_none());
    }

    #[test]
    fn outgoing_publish_rejects_packet_identifier_used_by_untracked_control_flow() {
        let mut mqtt = MqttState::builder(10).build();
        mqtt.outgoing_subscribe(
            Subscribe::new(SubscribeFilter::new("a/b", QoS::AtMostOnce), None),
            None,
        )
        .unwrap();

        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.pkid = 1;
        assert!(matches!(
            mqtt.outgoing_publish(publish),
            Err(StateError::InvalidState)
        ));

        mqtt.outgoing_unsubscribe(Unsubscribe::new("c/d", None), None)
            .unwrap();

        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.pkid = 2;
        assert!(matches!(
            mqtt.outgoing_publish(publish),
            Err(StateError::InvalidState)
        ));
    }

    #[test]
    fn outgoing_publish_should_set_pkid_and_add_publish_to_queue() {
        let mut mqtt = build_mqttstate();

        // QoS0 Publish
        let publish = build_outgoing_publish(QoS::AtMostOnce);

        // QoS 0 publish shouldn't be saved in queue
        mqtt.outgoing_publish(publish).unwrap();
        assert_eq!(mqtt.last_pkid, 0);
        assert_eq!(mqtt.inflight, 0);

        // QoS1 Publish
        let publish = build_outgoing_publish(QoS::AtLeastOnce);

        // Packet id should be set and publish should be saved in queue
        mqtt.outgoing_publish(publish.clone()).unwrap();
        assert_eq!(mqtt.last_pkid, 1);
        assert_eq!(mqtt.inflight, 1);

        // Packet id should be incremented and publish should be saved in queue
        mqtt.outgoing_publish(publish).unwrap();
        assert_eq!(mqtt.last_pkid, 2);
        assert_eq!(mqtt.inflight, 2);

        // QoS1 Publish
        let publish = build_outgoing_publish(QoS::ExactlyOnce);

        // Packet id should be set and publish should be saved in queue
        mqtt.outgoing_publish(publish.clone()).unwrap();
        assert_eq!(mqtt.last_pkid, 3);
        assert_eq!(mqtt.inflight, 3);

        // Packet id should be incremented and publish should be saved in queue
        mqtt.outgoing_publish(publish).unwrap();
        assert_eq!(mqtt.last_pkid, 4);
        assert_eq!(mqtt.inflight, 4);
    }

    #[test]
    fn outgoing_publish_with_max_inflight_is_ok() {
        let mut mqtt = MqttState::builder(2).build();

        // QoS2 publish
        let publish = build_outgoing_publish(QoS::ExactlyOnce);

        mqtt.outgoing_publish(publish.clone()).unwrap();
        assert_eq!(mqtt.last_pkid, 1);
        assert_eq!(mqtt.inflight, 1);

        // Packet id should be set back down to 0, since we hit the limit
        mqtt.outgoing_publish(publish.clone()).unwrap();
        assert_eq!(mqtt.last_pkid, 0);
        assert_eq!(mqtt.inflight, 2);

        assert!(matches!(
            mqtt.outgoing_publish(publish.clone()),
            Err(StateError::InvalidState)
        ));
        assert_eq!(mqtt.last_pkid, 0);
        assert_eq!(mqtt.inflight, 2);
        assert!(mqtt.collision.is_none());

        mqtt.handle_incoming_puback(&PubAck::new(1, None)).unwrap();
        mqtt.handle_incoming_puback(&PubAck::new(2, None)).unwrap();
        assert_eq!(mqtt.inflight, 0);

        // Now there should be space in the outgoing queue
        mqtt.outgoing_publish(publish).unwrap();
        assert_eq!(mqtt.last_pkid, 1);
        assert_eq!(mqtt.inflight, 1);
    }

    #[test]
    fn clean_is_calculating_pending_correctly() {
        fn build_publish_with_pkid(pkid: u16) -> Publish {
            let mut publish = Publish::new("test".to_owned(), QoS::AtLeastOnce, vec![], None);
            publish.pkid = pkid;
            publish
        }

        fn build_outgoing_pub() -> Vec<Option<Publish>> {
            vec![
                None,
                Some(build_publish_with_pkid(1)),
                Some(build_publish_with_pkid(2)),
                Some(build_publish_with_pkid(3)),
                None,
                None,
                Some(build_publish_with_pkid(6)),
            ]
        }

        let mut mqtt = build_mqttstate();
        mqtt.outgoing_pub = build_outgoing_pub();
        mqtt.last_puback = 3;
        let requests = mqtt.clean();
        let expected = vec![6, 1, 2, 3];
        for (req, pkid) in requests.iter().zip(expected) {
            if let Request::Publish(publish) = req {
                assert_eq!(publish.pkid, pkid);
            } else {
                unreachable!();
            }
        }

        mqtt.outgoing_pub = build_outgoing_pub();
        mqtt.last_puback = 0;
        let requests = mqtt.clean();
        let expected = vec![1, 2, 3, 6];
        for (req, pkid) in requests.iter().zip(expected) {
            if let Request::Publish(publish) = req {
                assert_eq!(publish.pkid, pkid);
            } else {
                unreachable!();
            }
        }

        mqtt.outgoing_pub = build_outgoing_pub();
        mqtt.last_puback = 6;
        let requests = mqtt.clean();
        let expected = vec![1, 2, 3, 6];
        for (req, pkid) in requests.iter().zip(expected) {
            if let Request::Publish(publish) = req {
                assert_eq!(publish.pkid, pkid);
            } else {
                unreachable!();
            }
        }
    }

    #[test]
    fn clean_recovers_collision_stashed_publish() {
        let mut mqtt = build_mqttstate();

        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.pkid = 1;
        mqtt.collision = Some(publish);

        let requests = mqtt.clean();

        assert!(mqtt.collision.is_none());
        match requests.as_slice() {
            [Request::Publish(publish)] => assert_eq!(publish.pkid, 1),
            requests => panic!("unexpected pending requests: {requests:?}"),
        }
    }

    #[test]
    fn clean_replays_tracked_publish_before_collision_stashed_publish() {
        let mut mqtt = build_mqttstate();

        let mut original = build_outgoing_publish(QoS::AtLeastOnce);
        original.pkid = 1;
        original.payload = Bytes::from_static(b"original");
        mqtt.outgoing_pub[1] = Some(original);

        let mut collision = build_outgoing_publish(QoS::AtLeastOnce);
        collision.pkid = 1;
        collision.payload = Bytes::from_static(b"collision");
        mqtt.collision = Some(collision);

        let requests = mqtt.clean();

        assert!(mqtt.collision.is_none());
        match requests.as_slice() {
            [Request::Publish(original), Request::Publish(collision)] => {
                assert_eq!(original.pkid, 1);
                assert_eq!(original.payload, Bytes::from_static(b"original"));
                assert_eq!(collision.pkid, 1);
                assert_eq!(collision.payload, Bytes::from_static(b"collision"));
            }
            requests => panic!("unexpected pending requests: {requests:?}"),
        }
    }

    #[test]
    fn clean_repairs_collision_stashed_publish_topic_alias() {
        let mut mqtt = build_mqttstate();

        let mut publish = build_outgoing_publish_with_alias("collision/topic", QoS::AtLeastOnce, 1);
        publish.pkid = 1;
        mqtt.collision = Some(publish);

        let requests = mqtt.clean();

        assert!(mqtt.collision.is_none());
        match requests.as_slice() {
            [Request::Publish(publish)] => {
                assert_eq!(publish.pkid, 1);
                assert_eq!(publish.topic, Bytes::from_static(b"collision/topic"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|properties| properties.topic_alias),
                    None
                );
            }
            requests => panic!("unexpected pending requests: {requests:?}"),
        }
    }

    #[test]
    fn incoming_publish_should_be_added_to_queue_correctly() {
        let mut mqtt = build_mqttstate();

        // QoS0, 1, 2 Publishes
        let mut publish1 = build_incoming_publish(QoS::AtMostOnce, 1);
        let mut publish2 = build_incoming_publish(QoS::AtLeastOnce, 2);
        let mut publish3 = build_incoming_publish(QoS::ExactlyOnce, 3);

        mqtt.handle_incoming_publish(&mut publish1).unwrap();
        mqtt.handle_incoming_publish(&mut publish2).unwrap();
        mqtt.handle_incoming_publish(&mut publish3).unwrap();

        // qos1 and qos2 publishes are tracked until their acknowledgement flow completes
        assert!(!mqtt.incoming_puback.contains(1));
        assert!(!mqtt.incoming_puback.contains(2));
        assert!(mqtt.incoming_pub.contains(3));
    }

    #[test]
    fn incoming_publish_should_be_acked() {
        let mut mqtt = build_mqttstate();

        // QoS0, 1, 2 Publishes
        let mut publish1 = build_incoming_publish(QoS::AtMostOnce, 1);
        let mut publish2 = build_incoming_publish(QoS::AtLeastOnce, 2);
        let mut publish3 = build_incoming_publish(QoS::ExactlyOnce, 3);

        mqtt.handle_incoming_publish(&mut publish1).unwrap();
        mqtt.handle_incoming_publish(&mut publish2).unwrap();
        mqtt.handle_incoming_publish(&mut publish3).unwrap();

        if let Event::Outgoing(Outgoing::PubAck(pkid)) = mqtt.events[0] {
            assert_eq!(pkid, 2);
        } else {
            panic!("missing puback");
        }

        if let Event::Outgoing(Outgoing::PubRec(pkid)) = mqtt.events[1] {
            assert_eq!(pkid, 3);
        } else {
            panic!("missing PubRec");
        }
    }

    #[test]
    fn incoming_publish_should_not_be_acked_with_manual_ack_mode() {
        let mut mqtt = build_mqttstate();
        mqtt.ack_mode = AckMode::Manual;

        // QoS0, 1, 2 Publishes
        let mut publish1 = build_incoming_publish(QoS::AtMostOnce, 1);
        let mut publish2 = build_incoming_publish(QoS::AtLeastOnce, 2);
        let mut publish3 = build_incoming_publish(QoS::ExactlyOnce, 3);

        mqtt.handle_incoming_publish(&mut publish1).unwrap();
        mqtt.handle_incoming_publish(&mut publish2).unwrap();
        mqtt.handle_incoming_publish(&mut publish3).unwrap();

        assert!(mqtt.incoming_puback.contains(2));
        assert!(mqtt.incoming_pub.contains(3));
        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn manual_puback_must_match_received_qos1_publish() {
        let mut mqtt = build_mqttstate();
        mqtt.ack_mode = AckMode::Manual;
        let mut publish = build_incoming_publish(QoS::AtLeastOnce, 10);

        assert!(
            mqtt.handle_incoming_publish(&mut publish)
                .unwrap()
                .is_none()
        );

        let got = mqtt
            .handle_outgoing_packet(Request::PubAck(PubAck::new(11, None)))
            .unwrap_err();
        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 11),
            e => panic!("Unexpected error: {e}"),
        }

        let packet = mqtt
            .handle_outgoing_packet(Request::PubAck(PubAck::new(10, None)))
            .unwrap()
            .unwrap();
        match packet {
            Packet::PubAck(puback) => assert_eq!(puback.pkid, 10),
            packet => panic!("Invalid network request: {packet:?}"),
        }
        assert!(!mqtt.incoming_puback.contains(10));
    }

    #[test]
    fn manual_pubrec_must_match_received_qos2_publish() {
        let mut mqtt = build_mqttstate();
        mqtt.ack_mode = AckMode::Manual;
        let mut publish = build_incoming_publish(QoS::ExactlyOnce, 10);

        assert!(
            mqtt.handle_incoming_publish(&mut publish)
                .unwrap()
                .is_none()
        );

        let got = mqtt
            .handle_outgoing_packet(Request::PubRec(PubRec::new(11, None)))
            .unwrap_err();
        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 11),
            e => panic!("Unexpected error: {e}"),
        }

        let packet = mqtt
            .handle_outgoing_packet(Request::PubRec(PubRec::new(10, None)))
            .unwrap()
            .unwrap();
        match packet {
            Packet::PubRec(pubrec) => assert_eq!(pubrec.pkid, 10),
            packet => panic!("Invalid network request: {packet:?}"),
        }
        assert!(mqtt.incoming_pub.contains(10));
    }

    #[test]
    fn manual_ack_rejects_duplicate_ack() {
        let mut mqtt = build_mqttstate();
        mqtt.ack_mode = AckMode::Manual;
        let mut qos1 = build_incoming_publish(QoS::AtLeastOnce, 10);
        let mut qos2 = build_incoming_publish(QoS::ExactlyOnce, 11);

        mqtt.handle_incoming_publish(&mut qos1).unwrap();
        mqtt.handle_incoming_publish(&mut qos2).unwrap();

        mqtt.handle_outgoing_packet(Request::PubAck(PubAck::new(10, None)))
            .unwrap();
        assert!(matches!(
            mqtt.handle_outgoing_packet(Request::PubAck(PubAck::new(10, None))),
            Err(StateError::Unsolicited(10))
        ));

        mqtt.handle_outgoing_packet(Request::PubRec(PubRec::new(11, None)))
            .unwrap();
        assert!(matches!(
            mqtt.handle_outgoing_packet(Request::PubRec(PubRec::new(11, None))),
            Err(StateError::Unsolicited(11))
        ));
    }

    #[test]
    fn manual_ack_rejects_qos_mismatch() {
        let mut mqtt = build_mqttstate();
        mqtt.ack_mode = AckMode::Manual;
        let mut qos1 = build_incoming_publish(QoS::AtLeastOnce, 10);
        let mut qos2 = build_incoming_publish(QoS::ExactlyOnce, 11);

        mqtt.handle_incoming_publish(&mut qos1).unwrap();
        mqtt.handle_incoming_publish(&mut qos2).unwrap();

        assert!(matches!(
            mqtt.handle_outgoing_packet(Request::PubRec(PubRec::new(10, None))),
            Err(StateError::Unsolicited(10))
        ));
        assert!(matches!(
            mqtt.handle_outgoing_packet(Request::PubAck(PubAck::new(11, None))),
            Err(StateError::Unsolicited(11))
        ));
    }

    #[test]
    fn outgoing_pubrel_without_original_publish_is_unsolicited() {
        let mut mqtt = build_mqttstate();

        let got = mqtt
            .handle_outgoing_packet(Request::PubRel(PubRel::new(7, None)))
            .unwrap_err();

        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 7),
            e => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn unknown_incoming_topic_alias_returns_protocol_error_disconnect() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .client_topic_alias_max(10)
            .build();
        let mut publish = build_incoming_publish(QoS::AtMostOnce, 0);
        publish.topic = Bytes::new();
        publish.properties = Some(publish_properties_with_alias(1));

        let packet = mqtt.handle_incoming_publish(&mut publish).unwrap().unwrap();

        assert!(matches!(
            packet,
            Packet::Disconnect(disconnect)
                if disconnect.reason_code == DisconnectReasonCode::ProtocolError
        ));
        assert!(publish.topic.is_empty());
    }

    #[test]
    fn handle_incoming_packet_does_not_surface_unknown_topic_alias_publish() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .client_topic_alias_max(10)
            .build();
        let mut publish = build_incoming_publish(QoS::AtMostOnce, 0);
        publish.topic = Bytes::new();
        publish.properties = Some(publish_properties_with_alias(1));

        let packet = mqtt
            .handle_incoming_packet(Incoming::Publish(publish))
            .unwrap()
            .unwrap();

        assert!(matches!(
            packet,
            Packet::Disconnect(disconnect)
                if disconnect.reason_code == DisconnectReasonCode::ProtocolError
        ));
        assert!(
            !mqtt
                .events
                .iter()
                .any(|event| matches!(event, Event::Incoming(Incoming::Publish(_))))
        );
        assert_eq!(
            mqtt.events,
            VecDeque::from([Event::Outgoing(Outgoing::Disconnect)])
        );
    }

    /// [MQTT-3.1.2-27] If the client's Topic Alias Maximum is 0 (default),
    /// any incoming topic alias from the server is a protocol error.
    #[test]
    fn incoming_topic_alias_rejected_when_client_topic_alias_max_is_zero() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .client_topic_alias_max(0)
            .build();
        let mut publish = build_incoming_publish(QoS::AtMostOnce, 0);
        publish.properties = Some(publish_properties_with_alias(1));

        let packet = mqtt.handle_incoming_publish(&mut publish).unwrap().unwrap();

        assert!(matches!(
            packet,
            Packet::Disconnect(disconnect)
                if disconnect.reason_code == DisconnectReasonCode::TopicAliasInvalid
        ));
    }

    /// [MQTT-3.1.2-27] When client_topic_alias_max is 0, the incoming
    /// PUBLISH with a topic alias should not be surfaced to the user.
    #[test]
    fn handle_incoming_packet_does_not_surface_publish_with_alias_exceeding_client_max() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .client_topic_alias_max(0)
            .build();
        let mut publish = build_incoming_publish(QoS::AtMostOnce, 0);
        publish.properties = Some(publish_properties_with_alias(1));

        let packet = mqtt
            .handle_incoming_packet(Incoming::Publish(publish))
            .unwrap()
            .unwrap();

        assert!(matches!(
            packet,
            Packet::Disconnect(disconnect)
                if disconnect.reason_code == DisconnectReasonCode::TopicAliasInvalid
        ));
        assert!(
            !mqtt
                .events
                .iter()
                .any(|event| matches!(event, Event::Incoming(Incoming::Publish(_))))
        );
        assert_eq!(
            mqtt.events,
            VecDeque::from([Event::Outgoing(Outgoing::Disconnect)])
        );
    }

    /// [MQTT-3.1.2-26] An incoming topic alias greater than the client's
    /// Topic Alias Maximum is a protocol error.
    #[test]
    fn incoming_topic_alias_greater_than_client_max_returns_topic_alias_invalid_disconnect() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .client_topic_alias_max(5)
            .build();
        let mut publish = build_incoming_publish(QoS::AtMostOnce, 0);
        publish.properties = Some(publish_properties_with_alias(6));

        let packet = mqtt.handle_incoming_publish(&mut publish).unwrap().unwrap();

        assert!(matches!(
            packet,
            Packet::Disconnect(disconnect)
                if disconnect.reason_code == DisconnectReasonCode::TopicAliasInvalid
        ));
    }

    /// [MQTT-3.1.2-26] An incoming topic alias equal to the client's
    /// Topic Alias Maximum is valid and should be accepted.
    #[test]
    fn incoming_topic_alias_equal_to_client_max_is_accepted() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .client_topic_alias_max(5)
            .build();
        let mut publish = build_incoming_publish(QoS::AtMostOnce, 0);
        publish.properties = Some(publish_properties_with_alias(5));

        let result = mqtt.handle_incoming_publish(&mut publish).unwrap();

        assert!(result.is_none());
        assert_eq!(
            mqtt.topic_aliases.incoming.get(&5),
            Some(&Bytes::from_static(b"hello/world"))
        );
    }

    /// [MQTT-3.1.2-26] An incoming topic alias less than the client's
    /// Topic Alias Maximum is valid and should be accepted.
    #[test]
    fn incoming_topic_alias_less_than_client_max_is_accepted() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .client_topic_alias_max(10)
            .build();
        let mut publish = build_incoming_publish(QoS::AtMostOnce, 0);
        publish.properties = Some(publish_properties_with_alias(3));

        let result = mqtt.handle_incoming_publish(&mut publish).unwrap();

        assert!(result.is_none());
        assert_eq!(
            mqtt.topic_aliases.incoming.get(&3),
            Some(&Bytes::from_static(b"hello/world"))
        );
    }

    /// [MQTT-3.3.2-9] An alias-only PUBLISH using a known alias in the
    /// client's accepted range should resolve and be delivered.
    #[test]
    fn incoming_alias_only_publish_with_known_alias_is_accepted() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .client_topic_alias_max(10)
            .build();
        let mut first = build_incoming_publish(QoS::AtMostOnce, 0);
        first.properties = Some(publish_properties_with_alias(3));
        mqtt.handle_incoming_publish(&mut first).unwrap();

        let mut second = build_incoming_publish(QoS::AtMostOnce, 0);
        second.topic = Bytes::new();
        second.properties = Some(publish_properties_with_alias(3));

        let result = mqtt.handle_incoming_publish(&mut second).unwrap();

        assert!(result.is_none());
        assert_eq!(second.topic, Bytes::from_static(b"hello/world"));
    }

    /// Verify that set_client_topic_alias_max updates the validation limit.
    #[test]
    fn set_client_topic_alias_max_updates_incoming_validation() {
        let mut mqtt = MqttState::builder(u16::MAX).build();
        // Default client_topic_alias_max is 0, so any alias should be rejected.
        let mut publish = build_incoming_publish(QoS::AtMostOnce, 0);
        publish.properties = Some(publish_properties_with_alias(1));

        let packet = mqtt.handle_incoming_publish(&mut publish).unwrap().unwrap();
        assert!(matches!(
            packet,
            Packet::Disconnect(disconnect)
                if disconnect.reason_code == DisconnectReasonCode::TopicAliasInvalid
        ));

        // After updating the limit, the same alias should be accepted.
        mqtt.set_client_topic_alias_max(Some(5));
        let mut publish = build_incoming_publish(QoS::AtMostOnce, 0);
        publish.properties = Some(publish_properties_with_alias(1));

        let result = mqtt.handle_incoming_publish(&mut publish).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn outgoing_publish_with_alias_greater_than_broker_maximum_returns_error() {
        let mut mqtt = build_mqttstate();
        mqtt.set_broker_topic_alias_max(5);
        let publish = build_outgoing_publish_with_alias("hello/world", QoS::AtMostOnce, 6);

        let err = mqtt.outgoing_publish(publish).unwrap_err();

        assert!(matches!(err, StateError::InvalidAlias { alias: 6, max: 5 }));
    }

    #[test]
    fn outgoing_publish_with_alias_zero_returns_error() {
        let mut mqtt = build_mqttstate();
        mqtt.set_broker_topic_alias_max(5);
        let publish = build_outgoing_publish_with_alias("hello/world", QoS::AtMostOnce, 0);

        let err = mqtt.outgoing_publish(publish).unwrap_err();

        assert!(matches!(err, StateError::InvalidAlias { alias: 0, max: 5 }));
    }

    #[test]
    fn outgoing_reauth_without_properties_synthesizes_connect_authentication_method() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
        let auth = Auth::new(AuthReasonCode::ReAuthenticate, None);

        let packet = mqtt
            .handle_outgoing_packet(Request::Auth(auth))
            .unwrap()
            .unwrap();

        let Packet::Auth(auth) = packet else {
            panic!("expected AUTH packet");
        };
        let properties = auth.properties.unwrap();
        assert_eq!(properties.method.as_deref(), Some(AUTH_METHOD));
        assert_eq!(auth.code, AuthReasonCode::ReAuthenticate);
    }

    #[test]
    fn outgoing_reauth_without_connect_authentication_method_fails() {
        let mut mqtt = build_auth_mqttstate(None);
        let auth = Auth::new(AuthReasonCode::ReAuthenticate, None);

        let err = mqtt
            .handle_outgoing_packet(Request::Auth(auth))
            .unwrap_err();

        assert!(matches!(err, StateError::AuthError(_)));
    }

    #[test]
    fn public_state_authentication_method_setter_enables_outgoing_reauth() {
        let mut mqtt = MqttState::builder(10).build();
        mqtt.set_authentication_method(Some(AUTH_METHOD.to_owned()));
        let auth = Auth::new(
            AuthReasonCode::ReAuthenticate,
            Some(auth_properties(Some(AUTH_METHOD))),
        );

        let packet = mqtt
            .handle_outgoing_packet(Request::Auth(auth))
            .unwrap()
            .unwrap();

        let Packet::Auth(auth) = packet else {
            panic!("expected AUTH packet");
        };
        assert_eq!(
            auth.properties.unwrap().method.as_deref(),
            Some(AUTH_METHOD)
        );
    }

    #[test]
    fn outgoing_reauth_fills_missing_authentication_method() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
        let auth = Auth::new(AuthReasonCode::ReAuthenticate, Some(auth_properties(None)));

        let packet = mqtt
            .handle_outgoing_packet(Request::Auth(auth))
            .unwrap()
            .unwrap();

        let Packet::Auth(auth) = packet else {
            panic!("expected AUTH packet");
        };
        assert_eq!(
            auth.properties.unwrap().method.as_deref(),
            Some(AUTH_METHOD)
        );
    }

    #[test]
    fn outgoing_reauth_rejects_mismatched_authentication_method() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
        let auth = Auth::new(
            AuthReasonCode::ReAuthenticate,
            Some(auth_properties(Some("other-method"))),
        );

        let err = mqtt
            .handle_outgoing_packet(Request::Auth(auth))
            .unwrap_err();

        assert!(matches!(err, StateError::AuthError(_)));
    }

    #[test]
    fn tracked_reauth_missing_method_notice_fails_with_specific_error() {
        let mut mqtt = build_auth_mqttstate(None);
        let (notice_tx, notice) = AuthNoticeTx::new();

        let err = mqtt
            .handle_outgoing_packet_with_notice(
                Request::Auth(Auth::new(AuthReasonCode::ReAuthenticate, None)),
                Some(crate::notice::TrackedNoticeTx::Auth(notice_tx)),
            )
            .unwrap_err();

        assert!(matches!(err, StateError::AuthError(_)));
        assert_eq!(
            notice.wait().unwrap_err(),
            AuthNoticeError::MissingAuthenticationMethod
        );
    }

    #[test]
    fn tracked_reauth_mismatched_method_notice_fails_with_auth_error() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
        let (notice_tx, notice) = AuthNoticeTx::new();

        let err = mqtt
            .handle_outgoing_packet_with_notice(
                Request::Auth(Auth::new(
                    AuthReasonCode::ReAuthenticate,
                    Some(auth_properties(Some("other-method"))),
                )),
                Some(crate::notice::TrackedNoticeTx::Auth(notice_tx)),
            )
            .unwrap_err();

        assert!(matches!(err, StateError::AuthError(_)));
        assert!(matches!(
            notice.wait().unwrap_err(),
            AuthNoticeError::AuthenticationFailed(_)
        ));
    }

    #[test]
    fn tracked_reauth_start_failure_notice_fails_with_auth_error() {
        let authenticator = Arc::new(Mutex::new(FailingStartAuthManager));
        let mut mqtt = MqttState::builder(10)
            .authentication_method(Some(AUTH_METHOD.to_owned()))
            .authenticator(authenticator)
            .build();
        let (notice_tx, notice) = AuthNoticeTx::new();

        let err = mqtt
            .handle_outgoing_packet_with_notice(
                Request::Auth(Auth::new(AuthReasonCode::ReAuthenticate, None)),
                Some(crate::notice::TrackedNoticeTx::Auth(notice_tx)),
            )
            .unwrap_err();

        assert!(matches!(err, StateError::AuthError(_)));
        assert!(matches!(
            notice.wait().unwrap_err(),
            AuthNoticeError::AuthenticationFailed(_)
        ));
    }

    #[test]
    fn tracked_reauth_poisoned_authenticator_lock_fails_notice_with_auth_error() {
        let authenticator = Arc::new(Mutex::new(StartAuthManager { response: None }));
        poison_mutex(&authenticator);
        let mut mqtt = MqttState::builder(10)
            .authentication_method(Some(AUTH_METHOD.to_owned()))
            .authenticator(authenticator)
            .build();
        let (notice_tx, notice) = AuthNoticeTx::new();

        let err = mqtt.handle_outgoing_packet_with_notice(
            Request::Auth(Auth::new(AuthReasonCode::ReAuthenticate, None)),
            Some(crate::notice::TrackedNoticeTx::Auth(notice_tx)),
        );

        assert!(matches!(err, Err(StateError::AuthenticatorLockPoisoned)));
        assert_eq!(
            notice.wait().unwrap_err(),
            AuthNoticeError::AuthenticationFailed("Authenticator lock poisoned".to_owned())
        );
        assert!(!mqtt.events.iter().any(|event| {
            matches!(
                event,
                Event::Auth(crate::AuthEvent::Started {
                    kind: crate::AuthExchangeKind::Reauthentication,
                    ..
                })
            )
        }));
    }

    #[test]
    fn initial_auth_start_returns_normalized_auth_properties() {
        let authenticator = Arc::new(Mutex::new(StartAuthManager {
            response: Some(auth_properties(None)),
        }));
        let mut mqtt = MqttState::builder(10)
            .authentication_method(Some(AUTH_METHOD.to_owned()))
            .authenticator(authenticator)
            .build();

        let properties = mqtt
            .begin_authentication_connect(Some(AUTH_METHOD.to_owned()))
            .unwrap()
            .unwrap();

        assert_eq!(properties.method.as_deref(), Some(AUTH_METHOD));
        assert_eq!(properties.data, Some(Bytes::from_static(b"auth-data")));
    }

    #[test]
    fn initial_auth_poisoned_authenticator_lock_fails_exchange_and_resets_state() {
        let authenticator = Arc::new(Mutex::new(StartAuthManager { response: None }));
        poison_mutex(&authenticator);
        let mut mqtt = MqttState::builder(10)
            .authentication_method(Some(AUTH_METHOD.to_owned()))
            .authenticator(authenticator)
            .build();

        let err = mqtt.begin_authentication_connect(Some(AUTH_METHOD.to_owned()));

        assert!(matches!(err, Err(StateError::AuthenticatorLockPoisoned)));
        assert!(mqtt.auth.active_exchange().is_none());
        assert!(mqtt.events.iter().any(|event| {
            matches!(
                event,
                Event::Auth(crate::AuthEvent::Failed {
                    kind: crate::AuthExchangeKind::InitialConnect,
                    reason: crate::AuthFailureReason::AuthenticationFailed(message),
                    ..
                }) if message == "Authenticator lock poisoned"
            )
        }));
    }

    #[test]
    fn outgoing_reauth_rejects_overlapping_attempt() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
        let auth = Auth::new(AuthReasonCode::ReAuthenticate, None);

        mqtt.handle_outgoing_packet(Request::Auth(auth.clone()))
            .unwrap();
        let err = mqtt
            .handle_outgoing_packet(Request::Auth(auth))
            .unwrap_err();

        assert!(matches!(err, StateError::AuthError(_)));
    }

    #[test]
    fn tracked_reauth_notice_completes_on_matching_auth_success() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
        let (notice_tx, notice) = AuthNoticeTx::new();
        let auth = Auth::new(AuthReasonCode::ReAuthenticate, None);

        mqtt.handle_outgoing_packet_with_notice(
            Request::Auth(auth),
            Some(crate::notice::TrackedNoticeTx::Auth(notice_tx)),
        )
        .unwrap();
        mqtt.handle_incoming_packet(Incoming::Auth(Auth::new(
            AuthReasonCode::Success,
            Some(auth_properties(Some(AUTH_METHOD))),
        )))
        .unwrap();

        assert_eq!(notice.wait().unwrap(), crate::AuthOutcome::Success);
    }

    #[test]
    fn disconnect_now_fails_active_tracked_reauth_notice() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
        let (notice_tx, notice) = AuthNoticeTx::new();

        mqtt.handle_outgoing_packet_with_notice(
            Request::Auth(Auth::new(AuthReasonCode::ReAuthenticate, None)),
            Some(crate::notice::TrackedNoticeTx::Auth(notice_tx)),
        )
        .unwrap();
        mqtt.handle_outgoing_packet(Request::DisconnectNow(Disconnect::new(
            DisconnectReasonCode::NormalDisconnection,
        )))
        .unwrap();

        assert_eq!(
            notice.wait().unwrap_err(),
            AuthNoticeError::ConnectionClosed
        );
        assert!(mqtt.events.iter().any(|event| {
            matches!(
                event,
                Event::Auth(crate::AuthEvent::Failed {
                    kind: crate::AuthExchangeKind::Reauthentication,
                    reason: crate::AuthFailureReason::ConnectionClosed,
                    ..
                })
            )
        }));
    }

    #[test]
    fn tracked_overlapping_reauth_notice_fails() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
        mqtt.handle_outgoing_packet(Request::Auth(Auth::new(
            AuthReasonCode::ReAuthenticate,
            None,
        )))
        .unwrap();
        let (notice_tx, notice) = AuthNoticeTx::new();

        let err = mqtt
            .handle_outgoing_packet_with_notice(
                Request::Auth(Auth::new(AuthReasonCode::ReAuthenticate, None)),
                Some(crate::notice::TrackedNoticeTx::Auth(notice_tx)),
            )
            .unwrap_err();

        assert!(matches!(err, StateError::AuthError(_)));
        assert_eq!(
            notice.wait().unwrap_err(),
            AuthNoticeError::OverlappingReauth
        );
    }

    #[test]
    fn incoming_auth_success_without_active_exchange_is_protocol_error() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
        let auth = Auth::new(
            AuthReasonCode::Success,
            Some(auth_properties(Some(AUTH_METHOD))),
        );

        let err = mqtt
            .handle_incoming_packet(Incoming::Auth(auth))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::Deserialization(Error::ProtocolError)
        ));
    }

    #[test]
    fn incoming_auth_success_accepts_matching_authentication_method() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
        mqtt.begin_authentication_connect(Some(AUTH_METHOD.to_owned()))
            .unwrap();
        let auth = Auth::new(
            AuthReasonCode::Success,
            Some(auth_properties(Some(AUTH_METHOD))),
        );

        let packet = mqtt.handle_incoming_packet(Incoming::Auth(auth)).unwrap();

        assert!(packet.is_none());
    }

    #[test]
    fn initial_auth_survives_fresh_session_pending_notice_cleanup() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
        mqtt.begin_authentication_connect(Some(AUTH_METHOD.to_owned()))
            .unwrap();
        mqtt.fail_pending_notices();

        let mut connack = build_connack_with_receive_max(10);
        connack.properties.as_mut().unwrap().authentication_method = Some(AUTH_METHOD.to_owned());
        mqtt.handle_incoming_packet(Incoming::ConnAck(connack))
            .unwrap();

        assert!(mqtt.events.iter().any(|event| {
            matches!(
                event,
                Event::Auth(crate::AuthEvent::Succeeded {
                    kind: crate::AuthExchangeKind::InitialConnect,
                    ..
                })
            )
        }));
        assert!(!mqtt.events.iter().any(|event| {
            matches!(
                event,
                Event::Auth(crate::AuthEvent::Failed {
                    kind: crate::AuthExchangeKind::InitialConnect,
                    ..
                })
            )
        }));
    }

    #[test]
    fn incoming_auth_success_rejects_missing_authentication_method() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
        mqtt.begin_authentication_connect(Some(AUTH_METHOD.to_owned()))
            .unwrap();
        let auth = Auth::new(AuthReasonCode::Success, None);

        let err = mqtt
            .handle_incoming_packet(Incoming::Auth(auth))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::Deserialization(Error::ProtocolError)
        ));
    }

    #[test]
    fn incoming_auth_success_rejects_mismatched_authentication_method() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
        mqtt.begin_authentication_connect(Some(AUTH_METHOD.to_owned()))
            .unwrap();
        let auth = Auth::new(
            AuthReasonCode::Success,
            Some(auth_properties(Some("other-method"))),
        );

        let err = mqtt
            .handle_incoming_packet(Incoming::Auth(auth))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::Deserialization(Error::ProtocolError)
        ));
    }

    #[test]
    fn incoming_auth_success_without_connect_authentication_method_is_protocol_error() {
        let mut mqtt = build_auth_mqttstate(None);
        let auth = Auth::new(
            AuthReasonCode::Success,
            Some(auth_properties(Some(AUTH_METHOD))),
        );

        let err = mqtt
            .handle_incoming_packet(Incoming::Auth(auth))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::Deserialization(Error::ProtocolError)
        ));
    }

    #[test]
    fn incoming_auth_continue_synthesizes_method_when_auth_manager_omits_it() {
        let auth_manager = Arc::new(Mutex::new(StaticAuthManager { response: Ok(None) }));
        let mut mqtt = MqttState::builder(10)
            .authentication_method(Some(AUTH_METHOD.to_owned()))
            .auth_manager(auth_manager)
            .build();
        mqtt.begin_authentication_connect(Some(AUTH_METHOD.to_owned()))
            .unwrap();
        let auth = Auth::new(
            AuthReasonCode::Continue,
            Some(auth_properties(Some(AUTH_METHOD))),
        );

        let packet = mqtt
            .handle_incoming_packet(Incoming::Auth(auth))
            .unwrap()
            .unwrap();

        let Packet::Auth(auth) = packet else {
            panic!("expected AUTH packet");
        };
        assert_eq!(auth.code, AuthReasonCode::Continue);
        assert_eq!(
            auth.properties.unwrap().method.as_deref(),
            Some(AUTH_METHOD)
        );
    }

    #[test]
    fn incoming_auth_continue_without_connect_authentication_method_is_protocol_error() {
        let auth_manager = Arc::new(Mutex::new(StaticAuthManager { response: Ok(None) }));
        let mut mqtt = MqttState::builder(10).auth_manager(auth_manager).build();
        let auth = Auth::new(
            AuthReasonCode::Continue,
            Some(auth_properties(Some(AUTH_METHOD))),
        );

        let err = mqtt
            .handle_incoming_packet(Incoming::Auth(auth))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::Deserialization(Error::ProtocolError)
        ));
    }

    #[test]
    fn incoming_auth_continue_rejects_mismatched_server_method() {
        let auth_manager = Arc::new(Mutex::new(StaticAuthManager { response: Ok(None) }));
        let mut mqtt = MqttState::builder(10)
            .authentication_method(Some(AUTH_METHOD.to_owned()))
            .auth_manager(auth_manager)
            .build();
        mqtt.begin_authentication_connect(Some(AUTH_METHOD.to_owned()))
            .unwrap();
        let auth = Auth::new(
            AuthReasonCode::Continue,
            Some(auth_properties(Some("other-method"))),
        );

        let err = mqtt
            .handle_incoming_packet(Incoming::Auth(auth))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::Deserialization(Error::ProtocolError)
        ));
    }

    #[test]
    fn incoming_auth_reauthenticate_is_protocol_error() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
        let auth = Auth::new(
            AuthReasonCode::ReAuthenticate,
            Some(auth_properties(Some(AUTH_METHOD))),
        );

        let err = mqtt
            .handle_incoming_packet(Incoming::Auth(auth))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::Deserialization(Error::ProtocolError)
        ));
    }

    #[test]
    fn connection_scoped_alias_state_resets_incoming_aliases_and_broker_maximum() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .client_topic_alias_max(10)
            .build();
        mqtt.set_broker_topic_alias_max(10);
        let mut aliased = build_incoming_publish(QoS::AtMostOnce, 0);
        aliased.properties = Some(publish_properties_with_alias(1));
        mqtt.handle_incoming_publish(&mut aliased).unwrap();

        let mut alias_only = build_incoming_publish(QoS::AtMostOnce, 0);
        alias_only.topic = Bytes::new();
        alias_only.properties = Some(publish_properties_with_alias(1));
        mqtt.handle_incoming_publish(&mut alias_only).unwrap();
        assert_eq!(alias_only.topic, Bytes::from_static(b"hello/world"));

        mqtt.reset_connection_scoped_state();

        assert_eq!(mqtt.broker_topic_alias_max(), 0);
        let mut stale_alias = build_incoming_publish(QoS::AtMostOnce, 0);
        stale_alias.topic = Bytes::new();
        stale_alias.properties = Some(publish_properties_with_alias(1));
        let packet = mqtt
            .handle_incoming_publish(&mut stale_alias)
            .unwrap()
            .unwrap();
        assert!(matches!(packet, Packet::Disconnect(_)));
    }

    #[test]
    fn replay_publish_with_known_outgoing_alias_restores_topic() {
        let mut mqtt = build_mqttstate();
        mqtt.set_broker_topic_alias_max(10);
        mqtt.outgoing_publish(build_outgoing_publish_with_alias(
            "hello/replay",
            QoS::AtMostOnce,
            2,
        ))
        .unwrap();

        let mut replay = build_outgoing_publish_with_alias("", QoS::AtLeastOnce, 2);
        let mut replay_topic_aliases = mqtt.replay_topic_aliases();

        MqttState::prepare_publish_for_replay_with_aliases(&mut replay, &mut replay_topic_aliases)
            .unwrap();

        assert_eq!(replay.topic, Bytes::from_static(b"hello/replay"));
        assert_eq!(
            replay
                .properties
                .as_ref()
                .and_then(|props| props.topic_alias),
            None
        );
    }

    #[test]
    fn replay_publish_with_concrete_topic_strips_stale_alias() {
        let mut replay = build_outgoing_publish_with_alias("hello/replay", QoS::AtLeastOnce, 2);
        let mut replay_topic_aliases = HashMap::new();

        MqttState::prepare_publish_for_replay_with_aliases(&mut replay, &mut replay_topic_aliases)
            .unwrap();

        assert_eq!(replay.topic, Bytes::from_static(b"hello/replay"));
        assert_eq!(
            replay
                .properties
                .as_ref()
                .and_then(|props| props.topic_alias),
            None
        );
        assert_eq!(
            replay_topic_aliases.get(&2),
            Some(&Bytes::from_static(b"hello/replay"))
        );
    }

    #[test]
    fn replay_publish_with_stripped_alias_is_valid_when_next_broker_allows_no_aliases() {
        let mut replay = build_outgoing_publish_with_alias("hello/replay", QoS::AtLeastOnce, 2);
        let mut replay_topic_aliases = HashMap::new();
        MqttState::prepare_publish_for_replay_with_aliases(&mut replay, &mut replay_topic_aliases)
            .unwrap();

        let mut next_connection = build_mqttstate();
        next_connection.set_broker_topic_alias_max(0);

        next_connection
            .handle_outgoing_packet(Request::Publish(replay))
            .unwrap();
    }

    #[test]
    fn replay_publish_with_unknown_outgoing_alias_fails() {
        let mqtt = build_mqttstate();
        let mut replay = build_outgoing_publish_with_alias("", QoS::AtLeastOnce, 3);
        let mut replay_topic_aliases = mqtt.replay_topic_aliases();

        let err = MqttState::prepare_publish_for_replay_with_aliases(
            &mut replay,
            &mut replay_topic_aliases,
        )
        .unwrap_err();

        assert_eq!(err, PublishNoticeError::TopicAliasReplayUnavailable(3));
        assert!(replay.topic.is_empty());
    }

    #[test]
    fn auto_topic_aliases_are_disabled_by_default() {
        let mut mqtt = build_mqttstate();
        mqtt.set_broker_topic_alias_max(10);

        let packet = mqtt
            .outgoing_publish(build_outgoing_publish(QoS::AtMostOnce))
            .unwrap()
            .unwrap();

        match packet {
            Packet::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(b"hello/world"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    None
                );
            }
            packet => panic!("expected publish, got {packet:?}"),
        }
    }

    #[test]
    fn auto_topic_aliases_send_topic_and_alias_before_alias_only_publish() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .topic_alias_policy(TopicAliasPolicy::Monotonic)
            .build();
        mqtt.set_broker_topic_alias_max(10);

        let first = mqtt
            .outgoing_publish(build_outgoing_publish(QoS::AtMostOnce))
            .unwrap()
            .unwrap();
        let second = mqtt
            .outgoing_publish(build_outgoing_publish(QoS::AtMostOnce))
            .unwrap()
            .unwrap();

        match first {
            Packet::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(b"hello/world"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    Some(1)
                );
            }
            packet => panic!("expected publish, got {packet:?}"),
        }
        match second {
            Packet::Publish(publish) => {
                assert!(publish.topic.is_empty());
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    Some(1)
                );
            }
            packet => panic!("expected publish, got {packet:?}"),
        }
    }

    #[test]
    fn auto_topic_aliases_do_nothing_when_broker_allows_no_aliases() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .topic_alias_policy(TopicAliasPolicy::Monotonic)
            .build();

        let packet = mqtt
            .outgoing_publish(build_outgoing_publish(QoS::AtMostOnce))
            .unwrap()
            .unwrap();

        match packet {
            Packet::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(b"hello/world"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    None
                );
            }
            packet => panic!("expected publish, got {packet:?}"),
        }
    }

    #[test]
    fn auto_topic_aliases_stop_allocating_when_capacity_is_exhausted() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .topic_alias_policy(TopicAliasPolicy::Monotonic)
            .build();
        mqtt.set_broker_topic_alias_max(1);
        mqtt.outgoing_publish(Publish::new("topic/one", QoS::AtMostOnce, vec![], None))
            .unwrap();

        let packet = mqtt
            .outgoing_publish(Publish::new("topic/two", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();

        match packet {
            Packet::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(b"topic/two"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    None
                );
            }
            packet => panic!("expected publish, got {packet:?}"),
        }
    }

    #[test]
    fn auto_topic_aliases_preserve_manual_aliases_and_skip_used_aliases() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .topic_alias_policy(TopicAliasPolicy::Monotonic)
            .build();
        mqtt.set_broker_topic_alias_max(2);
        mqtt.outgoing_publish(build_outgoing_publish_with_alias(
            "manual/topic",
            QoS::AtMostOnce,
            1,
        ))
        .unwrap();

        let packet = mqtt
            .outgoing_publish(Publish::new("auto/topic", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();

        match packet {
            Packet::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(b"auto/topic"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    Some(2)
                );
            }
            packet => panic!("expected publish, got {packet:?}"),
        }
    }

    #[test]
    fn manual_rebind_clears_stale_auto_topic_alias_mapping() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .topic_alias_policy(TopicAliasPolicy::Monotonic)
            .build();
        mqtt.set_broker_topic_alias_max(2);
        mqtt.outgoing_publish(Publish::new("auto/topic", QoS::AtMostOnce, vec![], None))
            .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish_with_alias(
            "manual/topic",
            QoS::AtMostOnce,
            1,
        ))
        .unwrap();

        let packet = mqtt
            .outgoing_publish(Publish::new("auto/topic", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();

        match packet {
            Packet::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(b"auto/topic"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    Some(2)
                );
            }
            packet => panic!("expected publish, got {packet:?}"),
        }
    }

    #[test]
    fn auto_topic_alias_qos_replay_uses_full_topic_after_clean() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .topic_alias_policy(TopicAliasPolicy::Monotonic)
            .build();
        mqtt.set_broker_topic_alias_max(10);
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtMostOnce))
            .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();

        let requests = mqtt.clean();

        assert_eq!(requests.len(), 1);
        match &requests[0] {
            Request::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(b"hello/world"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    None
                );
            }
            request => panic!("expected replay publish, got {request:?}"),
        }
    }

    #[test]
    fn auto_topic_alias_collision_does_not_register_unsent_alias() {
        let mut mqtt = MqttState::builder(2)
            .topic_alias_policy(TopicAliasPolicy::Monotonic)
            .build();
        mqtt.set_broker_topic_alias_max(10);
        mqtt.outgoing_publish(Publish::new(
            "inflight/topic",
            QoS::AtLeastOnce,
            vec![],
            None,
        ))
        .unwrap();

        let mut collided = Publish::new("collided/topic", QoS::AtLeastOnce, vec![], None);
        collided.pkid = 1;
        let (packet, flush_notice) = mqtt.outgoing_publish_with_notice(collided, None).unwrap();
        assert!(packet.is_none());
        assert!(flush_notice.is_none());
        assert!(mqtt.collision.is_some());

        let packet = mqtt
            .outgoing_publish(Publish::new(
                "collided/topic",
                QoS::AtMostOnce,
                vec![],
                None,
            ))
            .unwrap()
            .unwrap();

        match packet {
            Packet::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(b"collided/topic"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    Some(2)
                );
            }
            packet => panic!("expected publish, got {packet:?}"),
        }
    }

    #[test]
    fn auto_topic_alias_collision_replay_does_not_send_uncommitted_alias() {
        let mut mqtt = MqttState::builder(2)
            .topic_alias_policy(TopicAliasPolicy::Monotonic)
            .build();
        mqtt.set_broker_topic_alias_max(10);
        let first_notice = queue_publish_with_notice(
            &mut mqtt,
            Publish::new("inflight/topic", QoS::AtLeastOnce, vec![], None),
        );

        let mut collided = Publish::new("collided/topic", QoS::AtLeastOnce, vec![], None);
        collided.pkid = 1;
        let (packet, flush_notice) = mqtt.outgoing_publish_with_notice(collided, None).unwrap();
        assert!(packet.is_none());
        assert!(flush_notice.is_none());

        let puback = PubAck::new(1, None);
        let packet = mqtt.handle_incoming_puback(&puback).unwrap().unwrap();

        match packet {
            Packet::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(b"collided/topic"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    None
                );
            }
            packet => panic!("expected publish, got {packet:?}"),
        }
        assert_eq!(first_notice.wait(), Ok(PublishResult::Qos1(puback)));
    }

    #[test]
    fn auto_topic_alias_collision_replay_restores_reused_alias_topic_after_rebind() {
        let mut mqtt = MqttState::builder(2)
            .topic_alias_policy(TopicAliasPolicy::Monotonic)
            .build();
        mqtt.set_broker_topic_alias_max(10);
        let first_notice = queue_publish_with_notice(
            &mut mqtt,
            Publish::new("aliased/topic", QoS::AtLeastOnce, vec![], None),
        );

        let mut collided = Publish::new("aliased/topic", QoS::AtLeastOnce, vec![], None);
        collided.pkid = 1;
        let (packet, flush_notice) = mqtt.outgoing_publish_with_notice(collided, None).unwrap();
        assert!(packet.is_none());
        assert!(flush_notice.is_none());

        mqtt.outgoing_publish(build_outgoing_publish_with_alias(
            "manual/rebind",
            QoS::AtMostOnce,
            1,
        ))
        .unwrap();

        let puback = PubAck::new(1, None);
        let packet = mqtt.handle_incoming_puback(&puback).unwrap().unwrap();

        match packet {
            Packet::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(b"aliased/topic"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    None
                );
            }
            packet => panic!("expected publish, got {packet:?}"),
        }
        assert_eq!(first_notice.wait(), Ok(PublishResult::Qos1(puback)));
    }

    #[test]
    fn auto_topic_aliases_exhaust_without_wrapping_at_u16_max() {
        let mut mqtt = MqttState::builder(u16::MAX)
            .topic_alias_policy(TopicAliasPolicy::Monotonic)
            .build();
        mqtt.set_broker_topic_alias_max(u16::MAX);
        mqtt.topic_aliases.next_auto = Some(u16::MAX);

        let last_packet = mqtt
            .outgoing_publish(Publish::new("last/topic", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();
        match last_packet {
            Packet::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(b"last/topic"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    Some(u16::MAX)
                );
            }
            packet => panic!("expected publish, got {packet:?}"),
        }
        assert_eq!(mqtt.topic_aliases.next_auto, None);

        let exhausted_packet = mqtt
            .outgoing_publish(Publish::new(
                "exhausted/topic",
                QoS::AtMostOnce,
                vec![],
                None,
            ))
            .unwrap()
            .unwrap();

        match exhausted_packet {
            Packet::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(b"exhausted/topic"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    None
                );
            }
            packet => panic!("expected publish, got {packet:?}"),
        }
    }

    #[test]
    fn lru_auto_topic_aliases_evict_least_recent_topic() {
        let mut mqtt = build_lru_auto_alias_mqttstate(u16::MAX, 2);

        let first = mqtt
            .outgoing_publish(Publish::new("topic/one", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();
        let second = mqtt
            .outgoing_publish(Publish::new("topic/two", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();
        let third = mqtt
            .outgoing_publish(Publish::new("topic/three", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();

        assert_publish(first, b"topic/one", Some(1));
        assert_publish(second, b"topic/two", Some(2));
        assert_publish(third, b"topic/three", Some(1));

        let packet = mqtt
            .outgoing_publish(Publish::new("topic/three", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();
        assert_publish(packet, b"", Some(1));
    }

    #[test]
    fn lru_auto_topic_aliases_refresh_existing_topic_recency() {
        let mut mqtt = build_lru_auto_alias_mqttstate(u16::MAX, 2);

        mqtt.outgoing_publish(Publish::new("topic/one", QoS::AtMostOnce, vec![], None))
            .unwrap();
        mqtt.outgoing_publish(Publish::new("topic/two", QoS::AtMostOnce, vec![], None))
            .unwrap();
        let refresh = mqtt
            .outgoing_publish(Publish::new("topic/one", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();
        let evict = mqtt
            .outgoing_publish(Publish::new("topic/three", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();

        assert_publish(refresh, b"", Some(1));
        assert_publish(evict, b"topic/three", Some(2));
    }

    #[test]
    fn lru_auto_topic_aliases_rebound_alias_sends_full_topic_then_alias_only() {
        let mut mqtt = build_lru_auto_alias_mqttstate(u16::MAX, 1);

        mqtt.outgoing_publish(Publish::new("topic/one", QoS::AtMostOnce, vec![], None))
            .unwrap();
        let rebound = mqtt
            .outgoing_publish(Publish::new("topic/two", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();
        let alias_only = mqtt
            .outgoing_publish(Publish::new("topic/two", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();

        assert_publish(rebound, b"topic/two", Some(1));
        assert_publish(alias_only, b"", Some(1));
    }

    #[test]
    fn lru_auto_topic_aliases_do_not_evict_manual_aliases() {
        let mut mqtt = build_lru_auto_alias_mqttstate(u16::MAX, 2);
        mqtt.outgoing_publish(build_outgoing_publish_with_alias(
            "manual/topic",
            QoS::AtMostOnce,
            1,
        ))
        .unwrap();

        let first_auto = mqtt
            .outgoing_publish(Publish::new("auto/one", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();
        let second_auto = mqtt
            .outgoing_publish(Publish::new("auto/two", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();

        assert_publish(first_auto, b"auto/one", Some(2));
        assert_publish(second_auto, b"auto/two", Some(2));
        assert_eq!(
            mqtt.topic_aliases.outgoing.get(&1),
            Some(&Bytes::from_static(b"manual/topic"))
        );
    }

    #[test]
    fn lru_auto_topic_aliases_reset_on_reconnect() {
        let mut mqtt = build_lru_auto_alias_mqttstate(u16::MAX, 1);
        mqtt.outgoing_publish(Publish::new("topic/one", QoS::AtMostOnce, vec![], None))
            .unwrap();

        mqtt.reset_connection_scoped_state();
        mqtt.set_broker_topic_alias_max(1);
        let packet = mqtt
            .outgoing_publish(Publish::new("topic/one", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();

        assert_publish(packet, b"topic/one", Some(1));
    }

    #[test]
    fn lru_auto_topic_alias_qos_replay_after_eviction_uses_full_topic() {
        let mut mqtt = build_lru_auto_alias_mqttstate(u16::MAX, 1);
        mqtt.outgoing_publish(Publish::new("topic/one", QoS::AtMostOnce, vec![], None))
            .unwrap();
        mqtt.outgoing_publish(Publish::new("topic/one", QoS::AtLeastOnce, vec![], None))
            .unwrap();
        mqtt.outgoing_publish(Publish::new("topic/two", QoS::AtMostOnce, vec![], None))
            .unwrap();

        let requests = mqtt.clean();

        assert_eq!(requests.len(), 1);
        match &requests[0] {
            Request::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(b"topic/one"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    None
                );
            }
            request => panic!("expected replay publish, got {request:?}"),
        }
    }

    #[test]
    fn lru_auto_topic_alias_collision_during_rebind_does_not_commit_rebind() {
        let mut mqtt = build_lru_auto_alias_mqttstate(2, 1);
        mqtt.outgoing_publish(Publish::new("topic/one", QoS::AtLeastOnce, vec![], None))
            .unwrap();

        let mut collided = Publish::new("topic/two", QoS::AtLeastOnce, vec![], None);
        collided.pkid = 1;
        let (packet, flush_notice) = mqtt.outgoing_publish_with_notice(collided, None).unwrap();
        assert!(packet.is_none());
        assert!(flush_notice.is_none());

        let packet = mqtt
            .outgoing_publish(Publish::new("topic/one", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();
        assert_publish(packet, b"", Some(1));
    }

    #[test]
    fn lru_auto_topic_alias_collision_replay_after_later_rebind_uses_original_topic() {
        let mut mqtt = build_lru_auto_alias_mqttstate(2, 1);
        let first_notice = queue_publish_with_notice(
            &mut mqtt,
            Publish::new("topic/one", QoS::AtLeastOnce, vec![], None),
        );

        let mut collided = Publish::new("topic/one", QoS::AtLeastOnce, vec![], None);
        collided.pkid = 1;
        let (packet, flush_notice) = mqtt.outgoing_publish_with_notice(collided, None).unwrap();
        assert!(packet.is_none());
        assert!(flush_notice.is_none());

        mqtt.outgoing_publish(Publish::new("topic/two", QoS::AtMostOnce, vec![], None))
            .unwrap();

        let puback = PubAck::new(1, None);
        let packet = mqtt.handle_incoming_puback(&puback).unwrap().unwrap();

        assert_publish(packet, b"topic/one", None);
        assert_eq!(first_notice.wait(), Ok(PublishResult::Qos1(puback)));
    }

    #[test]
    fn lru_auto_topic_aliases_do_not_wrap_at_u16_max() {
        let mut mqtt = build_lru_auto_alias_mqttstate(u16::MAX, u16::MAX);
        mqtt.topic_aliases.next_auto = Some(u16::MAX);

        let last_packet = mqtt
            .outgoing_publish(Publish::new("last/topic", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();
        let rebound_packet = mqtt
            .outgoing_publish(Publish::new("rebound/topic", QoS::AtMostOnce, vec![], None))
            .unwrap()
            .unwrap();

        assert_publish(last_packet, b"last/topic", Some(u16::MAX));
        assert_eq!(mqtt.topic_aliases.next_auto, None);
        assert_publish(rebound_packet, b"rebound/topic", Some(u16::MAX));
    }

    #[test]
    fn public_clean_repairs_alias_only_publish_when_mapping_is_known() {
        let mut mqtt = build_mqttstate();
        mqtt.set_broker_topic_alias_max(10);
        mqtt.outgoing_publish(build_outgoing_publish_with_alias(
            "hello/replay",
            QoS::AtMostOnce,
            2,
        ))
        .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish_with_alias("", QoS::AtLeastOnce, 2))
            .unwrap();

        let requests = mqtt.clean();

        assert_eq!(requests.len(), 1);
        match &requests[0] {
            Request::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(b"hello/replay"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    None
                );
            }
            request => panic!("expected publish replay, got {request:?}"),
        }
        assert_eq!(mqtt.broker_topic_alias_max(), 0);
    }

    #[test]
    fn public_clean_preserves_alias_only_publish_topic_from_send_time_after_rebind() {
        let mut mqtt = build_mqttstate();
        mqtt.set_broker_topic_alias_max(10);
        mqtt.outgoing_publish(build_outgoing_publish_with_alias(
            "topic/a",
            QoS::AtMostOnce,
            1,
        ))
        .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish_with_alias("", QoS::AtLeastOnce, 1))
            .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish_with_alias(
            "topic/b",
            QoS::AtMostOnce,
            1,
        ))
        .unwrap();

        let requests = mqtt.clean();

        assert_eq!(requests.len(), 1);
        match &requests[0] {
            Request::Publish(publish) => {
                assert_eq!(publish.topic, Bytes::from_static(b"topic/a"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    None
                );
            }
            request => panic!("expected publish replay, got {request:?}"),
        }
    }

    #[test]
    fn public_clean_drops_alias_only_publish_when_mapping_is_unknown() {
        let mut mqtt = build_mqttstate();
        mqtt.set_broker_topic_alias_max(10);
        mqtt.outgoing_publish(build_outgoing_publish_with_alias("", QoS::AtLeastOnce, 3))
            .unwrap();

        let requests = mqtt.clean();

        assert!(requests.is_empty());
        assert_eq!(mqtt.broker_topic_alias_max(), 0);
    }

    #[test]
    fn handle_incoming_packet_should_emit_incoming_before_derived_qos1_ack() {
        let mut mqtt = build_mqttstate();
        let publish = build_incoming_publish(QoS::AtLeastOnce, 42);

        mqtt.handle_incoming_packet(Incoming::Publish(publish.clone()))
            .unwrap();

        assert_eq!(mqtt.events.len(), 2);
        assert_eq!(mqtt.events[0], Event::Incoming(Incoming::Publish(publish)));
        assert_eq!(mqtt.events[1], Event::Outgoing(Outgoing::PubAck(42)));
    }

    #[test]
    fn handle_incoming_packet_should_emit_incoming_before_derived_qos2_ack() {
        let mut mqtt = build_mqttstate();
        let publish = build_incoming_publish(QoS::ExactlyOnce, 43);

        mqtt.handle_incoming_packet(Incoming::Publish(publish.clone()))
            .unwrap();

        assert_eq!(mqtt.events.len(), 2);
        assert_eq!(mqtt.events[0], Event::Incoming(Incoming::Publish(publish)));
        assert_eq!(mqtt.events[1], Event::Outgoing(Outgoing::PubRec(43)));
    }

    #[test]
    fn handle_incoming_packet_suppresses_duplicate_qos2_publish_after_pubrec() {
        let mut mqtt = build_mqttstate();
        let publish = build_incoming_publish(QoS::ExactlyOnce, 43);

        mqtt.handle_incoming_packet(Incoming::Publish(publish.clone()))
            .unwrap();
        mqtt.events.clear();

        let packet = mqtt
            .handle_incoming_packet(Incoming::Publish(publish))
            .unwrap()
            .unwrap();

        assert!(matches!(packet, Packet::PubRec(pubrec) if pubrec.pkid == 43));
        assert_eq!(mqtt.events.len(), 1);
        assert_eq!(mqtt.events[0], Event::Outgoing(Outgoing::PubRec(43)));
    }

    #[test]
    fn handle_incoming_packet_resends_pubrec_for_manual_duplicate_qos2_publish_after_pubrec() {
        let mut mqtt = build_mqttstate();
        mqtt.ack_mode = AckMode::Manual;
        let publish = build_incoming_publish(QoS::ExactlyOnce, 43);

        assert!(
            mqtt.handle_incoming_packet(Incoming::Publish(publish.clone()))
                .unwrap()
                .is_none()
        );
        mqtt.handle_outgoing_packet(Request::PubRec(PubRec::new(43, None)))
            .unwrap();
        mqtt.events.clear();

        let packet = mqtt
            .handle_incoming_packet(Incoming::Publish(publish))
            .unwrap()
            .unwrap();

        assert!(matches!(packet, Packet::PubRec(pubrec) if pubrec.pkid == 43));
        assert_eq!(mqtt.events.len(), 1);
        assert_eq!(mqtt.events[0], Event::Outgoing(Outgoing::PubRec(43)));
    }

    #[test]
    fn handle_incoming_packet_delivers_qos2_publish_after_pubcomp_completes_previous_flow() {
        let mut mqtt = build_mqttstate();
        let publish = build_incoming_publish(QoS::ExactlyOnce, 43);

        mqtt.handle_incoming_packet(Incoming::Publish(publish.clone()))
            .unwrap();
        mqtt.handle_incoming_packet(Incoming::PubRel(PubRel::new(43, None)))
            .unwrap();
        mqtt.events.clear();

        mqtt.handle_incoming_packet(Incoming::Publish(publish.clone()))
            .unwrap();

        assert_eq!(mqtt.events.len(), 2);
        assert_eq!(mqtt.events[0], Event::Incoming(Incoming::Publish(publish)));
        assert_eq!(mqtt.events[1], Event::Outgoing(Outgoing::PubRec(43)));
    }

    #[test]
    fn pubrec_error_does_not_suppress_next_qos2_publish_with_same_packet_identifier() {
        let mut mqtt = build_mqttstate();
        mqtt.ack_mode = AckMode::Manual;
        let publish = build_incoming_publish(QoS::ExactlyOnce, 43);

        mqtt.handle_incoming_packet(Incoming::Publish(publish.clone()))
            .unwrap();
        let mut pubrec = PubRec::new(43, None);
        pubrec.reason = PubRecReason::ImplementationSpecificError;
        mqtt.handle_outgoing_packet(Request::PubRec(pubrec))
            .unwrap();
        mqtt.events.clear();

        mqtt.handle_incoming_packet(Incoming::Publish(publish.clone()))
            .unwrap();

        assert_eq!(mqtt.events.len(), 1);
        assert_eq!(mqtt.events[0], Event::Incoming(Incoming::Publish(publish)));
        assert!(mqtt.incoming_pub.contains(43));
        assert!(!mqtt.incoming_pubrec.contains(43));
    }

    #[test]
    fn incoming_qos2_publish_should_send_rec_to_network_and_publish_to_user() {
        let mut mqtt = build_mqttstate();
        let mut publish = build_incoming_publish(QoS::ExactlyOnce, 1);

        match mqtt.handle_incoming_publish(&mut publish).unwrap().unwrap() {
            Packet::PubRec(pubrec) => assert_eq!(pubrec.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }
    }

    #[test]
    fn incoming_puback_should_remove_correct_publish_from_queue() {
        let mut mqtt = build_mqttstate();

        let publish1 = build_outgoing_publish(QoS::AtLeastOnce);
        let publish2 = build_outgoing_publish(QoS::ExactlyOnce);

        mqtt.outgoing_publish(publish1).unwrap();
        mqtt.outgoing_publish(publish2).unwrap();
        assert_eq!(mqtt.inflight, 2);

        mqtt.handle_incoming_puback(&PubAck::new(1, None)).unwrap();
        assert_eq!(mqtt.inflight, 1);

        mqtt.handle_incoming_puback(&PubAck::new(2, None)).unwrap();
        assert_eq!(mqtt.inflight, 0);

        assert!(mqtt.outgoing_pub[1].is_none());
        assert!(mqtt.outgoing_pub[2].is_none());
    }

    #[test]
    fn incoming_puback_updates_last_puback() {
        let mut mqtt = build_mqttstate();

        let publish1 = build_outgoing_publish(QoS::AtLeastOnce);
        let publish2 = build_outgoing_publish(QoS::AtLeastOnce);
        mqtt.outgoing_publish(publish1).unwrap();
        mqtt.outgoing_publish(publish2).unwrap();
        assert_eq!(mqtt.last_puback, 0);

        mqtt.handle_incoming_puback(&PubAck::new(1, None)).unwrap();
        assert_eq!(mqtt.last_puback, 1);

        mqtt.handle_incoming_puback(&PubAck::new(2, None)).unwrap();
        assert_eq!(mqtt.last_puback, 2);
    }

    #[test]
    fn incoming_puback_advances_last_puback_only_on_contiguous_boundary() {
        let mut mqtt = build_mqttstate();

        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        assert_eq!(mqtt.last_puback, 0);

        mqtt.handle_incoming_puback(&PubAck::new(2, None)).unwrap();
        assert_eq!(mqtt.last_puback, 0);

        mqtt.handle_incoming_puback(&PubAck::new(1, None)).unwrap();
        assert_eq!(mqtt.last_puback, 2);

        mqtt.handle_incoming_puback(&PubAck::new(3, None)).unwrap();
        assert_eq!(mqtt.last_puback, 3);
    }

    #[test]
    fn control_packet_id_gap_does_not_leave_completed_publish_undrained() {
        let mut mqtt = build_mqttstate();

        mqtt.outgoing_subscribe(
            Subscribe::new(SubscribeFilter::new("a/b", QoS::AtMostOnce), None),
            None,
        )
        .unwrap();
        mqtt.handle_incoming_suback(&SubAck {
            pkid: 1,
            return_codes: vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
            properties: None,
        })
        .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        mqtt.handle_incoming_puback(&PubAck::new(2, None)).unwrap();

        assert_eq!(mqtt.inflight, 0);
        assert!(mqtt.outbound_requests_drained());
        assert!(mqtt.outgoing_pub_ack.ones().next().is_none());
    }

    #[test]
    fn control_packet_id_behind_frontier_does_not_leave_stale_ack_bit() {
        let mut mqtt = build_mqttstate();
        mqtt.last_puback = 10;
        mqtt.last_pkid = 10;
        mqtt.pending_subscribe.insert(
            1,
            PendingSubscribe {
                subscribe: Subscribe::new(SubscribeFilter::new("a/b", QoS::AtMostOnce), None),
                notice: None,
            },
        );

        mqtt.handle_incoming_suback(&SubAck {
            pkid: 1,
            return_codes: vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
            properties: None,
        })
        .unwrap();

        assert!(mqtt.outbound_requests_drained());
        assert!(mqtt.outgoing_pub_ack.ones().next().is_none());
    }

    #[test]
    fn control_packet_identifier_skips_reserved_publish_identifier() {
        let mut mqtt = MqttState::builder(1).build();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();

        let packet = mqtt
            .outgoing_subscribe(
                Subscribe::new(SubscribeFilter::new("a/b", QoS::AtMostOnce), None),
                None,
            )
            .unwrap()
            .unwrap();

        match packet {
            Packet::Subscribe(subscribe) => assert_eq!(subscribe.pkid, 2),
            packet => panic!("Unexpected packet: {packet:?}"),
        }
        assert!(mqtt.packet_identifier_in_use(1));
        assert!(mqtt.packet_identifier_in_use(2));
    }

    #[test]
    fn control_packet_identifier_is_reusable_after_suback() {
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_subscribe(
            Subscribe::new(SubscribeFilter::new("a/b", QoS::AtMostOnce), None),
            None,
        )
        .unwrap();

        mqtt.handle_incoming_suback(&SubAck {
            pkid: 1,
            return_codes: vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
            properties: None,
        })
        .unwrap();

        assert!(!mqtt.packet_identifier_in_use(1));
        mqtt.reserve_outbound_pkid(1).unwrap();
        assert!(mqtt.packet_identifier_in_use(1));
    }

    #[test]
    fn qos2_publish_identifier_remains_reserved_until_pubcomp() {
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::ExactlyOnce))
            .unwrap();

        mqtt.handle_incoming_pubrec(&PubRec::new(1, None)).unwrap();
        assert!(mqtt.packet_identifier_in_use(1));

        mqtt.handle_incoming_pubcomp(&PubComp::new(1, None))
            .unwrap();
        assert!(!mqtt.packet_identifier_in_use(1));
    }

    #[test]
    fn control_packet_identifier_exhaustion_returns_invalid_state() {
        let mut mqtt = build_mqttstate();
        for pkid in 1..=u16::MAX {
            mqtt.reserve_outbound_pkid(pkid).unwrap();
        }

        assert!(!mqtt.control_packet_identifier_available());
        assert!(matches!(
            mqtt.next_control_pkid(),
            Err(StateError::InvalidState)
        ));
    }

    #[test]
    fn mixed_qos_completion_clears_outbound_drain_state() {
        let mut mqtt = build_mqttstate();

        mqtt.outgoing_publish(build_outgoing_publish(QoS::ExactlyOnce))
            .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::ExactlyOnce))
            .unwrap();

        mqtt.handle_incoming_pubrec(&PubRec::new(1, None)).unwrap();
        mqtt.handle_incoming_puback(&PubAck::new(2, None)).unwrap();
        mqtt.handle_incoming_puback(&PubAck::new(3, None)).unwrap();
        mqtt.handle_incoming_pubcomp(&PubComp::new(1, None))
            .unwrap();
        mqtt.handle_incoming_pubrec(&PubRec::new(4, None)).unwrap();
        mqtt.handle_incoming_pubcomp(&PubComp::new(4, None))
            .unwrap();

        assert_eq!(mqtt.inflight, 0);
        assert!(mqtt.outbound_requests_drained());
        assert!(mqtt.outgoing_pub_ack.ones().next().is_none());
        assert!(mqtt.outgoing_rel.ones().next().is_none());
    }

    #[test]
    fn clean_keeps_oldest_unacked_publish_first_after_out_of_order_puback() {
        let mut mqtt = build_mqttstate();

        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();

        mqtt.handle_incoming_puback(&PubAck::new(2, None)).unwrap();
        let requests = mqtt.clean();

        let pending_pkids: Vec<u16> = requests
            .iter()
            .map(|req| match req {
                Request::Publish(publish) => publish.pkid,
                req => panic!("Unexpected request while cleaning: {req:?}"),
            })
            .collect();

        assert_eq!(pending_pkids, vec![1, 3]);
    }

    #[test]
    fn incoming_puback_with_pkid_greater_than_max_inflight_should_be_handled_gracefully() {
        let mut mqtt = build_mqttstate();

        let got = mqtt
            .handle_incoming_puback(&PubAck::new(101, None))
            .unwrap_err();

        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 101),
            e => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn incoming_puback_failure_collision_replays_blocked_publish() {
        let mut mqtt = build_mqttstate();
        let first_notice =
            queue_publish_with_notice(&mut mqtt, build_outgoing_publish(QoS::AtLeastOnce));

        let (collided_tx, _collided_notice) = PublishNoticeTx::new();
        let mut collided = build_outgoing_publish(QoS::AtLeastOnce);
        collided.pkid = 1;
        let (packet, flush_notice) = mqtt
            .outgoing_publish_with_notice(collided, Some(collided_tx))
            .unwrap();
        assert!(packet.is_none());
        assert!(flush_notice.is_none());
        assert!(mqtt.collision.is_some());

        let mut puback = PubAck::new(1, None);
        puback.reason = PubAckReason::ImplementationSpecificError;

        let packet = mqtt.handle_incoming_puback(&puback).unwrap().unwrap();
        match packet {
            Packet::Publish(publish) => assert_eq!(publish.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }

        assert_eq!(first_notice.wait(), Ok(PublishResult::Qos1(puback)));
        assert_eq!(mqtt.inflight, 1);
        assert!(mqtt.collision.is_none());
    }

    #[test]
    fn incoming_pubrec_should_release_publish_from_queue_and_add_relid_to_rel_queue() {
        let mut mqtt = build_mqttstate();

        let publish1 = build_outgoing_publish(QoS::AtLeastOnce);
        let publish2 = build_outgoing_publish(QoS::ExactlyOnce);

        let _publish_out = mqtt.outgoing_publish(publish1);
        let _publish_out = mqtt.outgoing_publish(publish2);

        mqtt.handle_incoming_pubrec(&PubRec::new(2, None)).unwrap();
        assert_eq!(mqtt.inflight, 2);

        // check if the remaining element's pkid is 1
        let backup = mqtt.outgoing_pub[1].clone();
        assert_eq!(backup.unwrap().pkid, 1);

        // check if the qos2 element's release pkid is 2
        assert!(mqtt.outgoing_rel.contains(2));
    }

    #[test]
    fn incoming_pubrec_should_send_release_to_network_and_nothing_to_user() {
        let mut mqtt = build_mqttstate();

        let publish = build_outgoing_publish(QoS::ExactlyOnce);
        match mqtt.outgoing_publish(publish).unwrap().unwrap() {
            Packet::Publish(publish) => assert_eq!(publish.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }

        match mqtt
            .handle_incoming_pubrec(&PubRec::new(1, None))
            .unwrap()
            .unwrap()
        {
            Packet::PubRel(pubrel) => assert_eq!(pubrel.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }
    }

    #[test]
    fn outgoing_publish_cannot_reuse_pkid_reserved_for_pubrel_replay() {
        let mut mqtt = build_mqttstate();

        mqtt.outgoing_publish(build_outgoing_publish(QoS::ExactlyOnce))
            .unwrap();
        mqtt.handle_incoming_pubrec(&PubRec::new(1, None)).unwrap();
        let mut pending = mqtt.clean();
        assert_eq!(pending.len(), 1);

        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.pkid = 1;
        let got = mqtt
            .handle_outgoing_packet(Request::Publish(publish))
            .unwrap_err();
        assert!(matches!(got, StateError::InvalidState));
        assert!(mqtt.outgoing_pub[1].is_none());
        assert!(mqtt.outgoing_rel_replay.contains(1));

        let packet = mqtt
            .handle_outgoing_packet(pending.remove(0))
            .unwrap()
            .unwrap();
        match packet {
            Packet::PubRel(pubrel) => assert_eq!(pubrel.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }
        assert!(!mqtt.outgoing_rel_replay.contains(1));
        assert!(mqtt.outgoing_rel.contains(1));
    }

    #[test]
    fn incoming_pubrec_failure_without_collision_decrements_inflight() {
        let mut mqtt = build_mqttstate();
        let first_notice =
            queue_publish_with_notice(&mut mqtt, build_outgoing_publish(QoS::ExactlyOnce));

        let mut pubrec = PubRec::new(1, None);
        pubrec.reason = PubRecReason::ImplementationSpecificError;

        assert!(
            mqtt.handle_incoming_pubrec(&pubrec)
                .unwrap()
                .complete_notices_and_has_no_outgoing()
        );
        assert_eq!(
            first_notice.wait(),
            Ok(PublishResult::Qos2PubRecRejected(pubrec))
        );
        assert_eq!(mqtt.inflight, 0);
        assert!(mqtt.outgoing_pub[1].is_none());
        assert!(!mqtt.outgoing_rel.contains(1));
    }

    #[test]
    fn incoming_pubrec_failure_releases_inflight_and_replays_collision() {
        let mut mqtt = build_mqttstate();
        let first_notice =
            queue_publish_with_notice(&mut mqtt, build_outgoing_publish(QoS::ExactlyOnce));

        let (collided_tx, _collided_notice) = PublishNoticeTx::new();
        let mut collided = build_outgoing_publish(QoS::ExactlyOnce);
        collided.pkid = 1;
        let (packet, flush_notice) = mqtt
            .outgoing_publish_with_notice(collided, Some(collided_tx))
            .unwrap();
        assert!(packet.is_none());
        assert!(flush_notice.is_none());
        assert!(mqtt.collision.is_some());

        let mut pubrec = PubRec::new(1, None);
        pubrec.reason = PubRecReason::ImplementationSpecificError;

        let packet = mqtt.handle_incoming_pubrec(&pubrec).unwrap().unwrap();
        match packet {
            Packet::Publish(publish) => assert_eq!(publish.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }

        assert_eq!(
            first_notice.wait(),
            Ok(PublishResult::Qos2PubRecRejected(pubrec))
        );
        assert_eq!(mqtt.inflight, 1);
        assert!(mqtt.collision.is_none());
        assert!(!mqtt.outgoing_rel.contains(1));

        let packet = mqtt
            .handle_incoming_pubrec(&PubRec::new(1, None))
            .unwrap()
            .unwrap();
        match packet {
            Packet::PubRel(release) => assert_eq!(release.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }
    }

    #[test]
    fn incoming_pubrel_should_send_comp_to_network_and_nothing_to_user() {
        let mut mqtt = build_mqttstate();
        let mut publish = build_incoming_publish(QoS::ExactlyOnce, 1);

        match mqtt.handle_incoming_publish(&mut publish).unwrap().unwrap() {
            Packet::PubRec(pubrec) => assert_eq!(pubrec.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }

        match mqtt
            .handle_incoming_pubrel(&PubRel::new(1, None))
            .unwrap()
            .unwrap()
        {
            Packet::PubComp(pubcomp) => assert_eq!(pubcomp.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }

        match mqtt
            .handle_incoming_pubrel(&PubRel::new(1, None))
            .unwrap()
            .unwrap()
        {
            Packet::PubComp(pubcomp) => {
                assert_eq!(pubcomp.pkid, 1);
                assert_eq!(pubcomp.reason, PubCompReason::PacketIdentifierNotFound);
            }
            packet => panic!("Invalid recovery network request: {packet:?}"),
        }
    }

    #[test]
    fn incoming_pubrel_before_pubrec_is_unsolicited() {
        let mut mqtt = build_mqttstate();
        mqtt.ack_mode = AckMode::Manual;
        let mut publish = build_incoming_publish(QoS::ExactlyOnce, 1);

        assert!(
            mqtt.handle_incoming_publish(&mut publish)
                .unwrap()
                .is_none()
        );

        assert!(matches!(
            mqtt.handle_incoming_pubrel(&PubRel::new(1, None)),
            Err(StateError::Unsolicited(1))
        ));
    }

    #[test]
    fn incoming_pubcomp_should_release_correct_pkid_from_release_queue() {
        let mut mqtt = build_mqttstate();
        let publish = build_outgoing_publish(QoS::ExactlyOnce);

        mqtt.outgoing_publish(publish).unwrap();
        mqtt.handle_incoming_pubrec(&PubRec::new(1, None)).unwrap();

        mqtt.handle_incoming_pubcomp(&PubComp::new(1, None))
            .unwrap();
        assert_eq!(mqtt.inflight, 0);
    }

    #[test]
    fn incoming_pubcomp_failure_without_collision_decrements_inflight() {
        let mut mqtt = build_mqttstate();
        let first_notice =
            queue_publish_with_notice(&mut mqtt, build_outgoing_publish(QoS::ExactlyOnce));
        mqtt.handle_incoming_pubrec(&PubRec::new(1, None)).unwrap();

        let mut pubcomp = PubComp::new(1, None);
        pubcomp.reason = PubCompReason::PacketIdentifierNotFound;

        assert!(
            mqtt.handle_incoming_pubcomp(&pubcomp)
                .unwrap()
                .complete_notices_and_has_no_outgoing()
        );
        assert_eq!(
            first_notice.wait(),
            Ok(PublishResult::Qos2Completed(pubcomp))
        );
        assert_eq!(mqtt.inflight, 0);
        assert!(!mqtt.outgoing_rel.contains(1));
    }

    #[test]
    fn incoming_pubcomp_collision_replay_should_restore_qos2_tracking() {
        let mut mqtt = build_mqttstate();
        let publish = build_outgoing_publish(QoS::ExactlyOnce);
        mqtt.outgoing_publish(publish).unwrap();

        let mut collided = build_outgoing_publish(QoS::ExactlyOnce);
        collided.pkid = 1;
        assert!(mqtt.outgoing_publish(collided).unwrap().is_none());
        assert!(mqtt.collision.is_some());

        mqtt.handle_incoming_pubrec(&PubRec::new(1, None)).unwrap();
        let packet = mqtt
            .handle_incoming_pubcomp(&PubComp::new(1, None))
            .unwrap()
            .unwrap();
        match packet {
            Packet::Publish(publish) => assert_eq!(publish.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }

        assert!(mqtt.outgoing_pub[1].is_some());
        assert_eq!(mqtt.inflight, 1);

        let packet = mqtt
            .handle_incoming_pubrec(&PubRec::new(1, None))
            .unwrap()
            .unwrap();
        match packet {
            Packet::PubRel(pubrel) => assert_eq!(pubrel.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }
    }

    #[test]
    fn incoming_pubcomp_failure_replays_collision_and_preserves_qos2_tracking() {
        let mut mqtt = build_mqttstate();
        let first_notice =
            queue_publish_with_notice(&mut mqtt, build_outgoing_publish(QoS::ExactlyOnce));

        let (collided_tx, _collided_notice) = PublishNoticeTx::new();
        let mut collided = build_outgoing_publish(QoS::ExactlyOnce);
        collided.pkid = 1;
        let (packet, flush_notice) = mqtt
            .outgoing_publish_with_notice(collided, Some(collided_tx))
            .unwrap();
        assert!(packet.is_none());
        assert!(flush_notice.is_none());
        assert!(mqtt.collision.is_some());

        mqtt.handle_incoming_pubrec(&PubRec::new(1, None)).unwrap();

        let mut pubcomp = PubComp::new(1, None);
        pubcomp.reason = PubCompReason::PacketIdentifierNotFound;

        let packet = mqtt.handle_incoming_pubcomp(&pubcomp).unwrap().unwrap();
        match packet {
            Packet::Publish(publish) => assert_eq!(publish.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }

        assert_eq!(
            first_notice.wait(),
            Ok(PublishResult::Qos2Completed(pubcomp))
        );
        assert_eq!(mqtt.inflight, 1);
        assert!(mqtt.collision.is_none());
        assert!(!mqtt.outgoing_rel.contains(1));
        assert!(mqtt.outgoing_pub[1].is_some());

        let packet = mqtt
            .handle_incoming_pubrec(&PubRec::new(1, None))
            .unwrap()
            .unwrap();
        match packet {
            Packet::PubRel(pubrel) => assert_eq!(pubrel.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }
    }

    #[test]
    fn outgoing_disconnect_should_preserve_reason_and_properties() {
        let mut mqtt = build_mqttstate();
        let properties = DisconnectProperties {
            session_expiry_interval: Some(60),
            reason_string: Some("disconnect test".to_string()),
            user_properties: vec![("key".to_string(), "value".to_string())],
            server_reference: Some("broker-2".to_string()),
        };
        let disconnect = Disconnect::new_with_properties(
            DisconnectReasonCode::ImplementationSpecificError,
            properties,
        );

        let packet = mqtt
            .handle_outgoing_packet(Request::DisconnectNow(disconnect.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(packet, Packet::Disconnect(disconnect));
        assert!(matches!(
            mqtt.events.back(),
            Some(Event::Outgoing(Outgoing::Disconnect))
        ));
    }

    #[test]
    fn outgoing_ping_handle_should_throw_errors_for_no_pingresp() {
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_ping().unwrap();

        // network activity other than pingresp
        let publish = build_outgoing_publish(QoS::AtLeastOnce);
        mqtt.handle_outgoing_packet(Request::Publish(publish))
            .unwrap();
        mqtt.handle_incoming_packet(Incoming::PubAck(PubAck::new(1, None)))
            .unwrap();

        // should throw error because we didn't get pingresp for previous ping
        match mqtt.outgoing_ping() {
            Ok(_) => panic!("Should throw pingresp await error"),
            Err(StateError::AwaitPingResp) => (),
            Err(e) => panic!("Should throw pingresp await error. Error = {e:?}"),
        }
    }

    #[test]
    fn outgoing_ping_handle_should_succeed_if_pingresp_is_received() {
        let mut mqtt = build_mqttstate();

        // should ping
        mqtt.outgoing_ping().unwrap();
        mqtt.handle_incoming_packet(Incoming::PingResp).unwrap();

        // should ping
        mqtt.outgoing_ping().unwrap();
    }

    // Defensive handling for MQTT-3.1.4-3 server takeover: if the broker sends
    // DISCONNECT with reason code 0x8E (SessionTakenOver), surface that reason
    // through StateError::ServerDisconnect.
    #[test]
    fn incoming_disconnect_with_session_taken_over_surfaces_reason_code() {
        let mut mqtt = build_mqttstate();
        let disconnect = Disconnect::new(DisconnectReasonCode::SessionTakenOver);

        let err = mqtt
            .handle_incoming_packet(Incoming::Disconnect(disconnect.clone()))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::ServerDisconnect {
                reason_code: DisconnectReasonCode::SessionTakenOver,
                reason_string: None,
            }
        ));
        assert_eq!(
            mqtt.events.pop_front(),
            Some(Event::Incoming(Incoming::Disconnect(disconnect)))
        );
    }

    // Preserve the server-provided reason_string alongside SessionTakenOver.
    #[test]
    fn incoming_disconnect_with_session_taken_over_extracts_reason_string() {
        let mut mqtt = build_mqttstate();
        let disconnect = Disconnect::new_with_properties(
            DisconnectReasonCode::SessionTakenOver,
            DisconnectProperties {
                session_expiry_interval: None,
                reason_string: Some("another client connected".to_owned()),
                user_properties: vec![],
                server_reference: None,
            },
        );

        let err = mqtt
            .handle_incoming_packet(Incoming::Disconnect(disconnect.clone()))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::ServerDisconnect {
                reason_code: DisconnectReasonCode::SessionTakenOver,
                reason_string: Some(ref s),
            } if s == "another client connected"
        ));
        assert!(matches!(
            mqtt.events.pop_front(),
            Some(Event::Incoming(Incoming::Disconnect(_)))
        ));
    }

    // Verify that a generic server DISCONNECT (e.g., ServerShuttingDown) is
    // also correctly surfaced, confirming the handling path works for all
    // reason codes — not just SessionTakenOver.
    #[test]
    fn incoming_disconnect_with_other_reason_code_surfaces_reason_code() {
        let mut mqtt = build_mqttstate();
        let disconnect = Disconnect::new(DisconnectReasonCode::ServerShuttingDown);

        let err = mqtt
            .handle_incoming_packet(Incoming::Disconnect(disconnect.clone()))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::ServerDisconnect {
                reason_code: DisconnectReasonCode::ServerShuttingDown,
                reason_string: None,
            }
        ));
        assert_eq!(
            mqtt.events.pop_front(),
            Some(Event::Incoming(Incoming::Disconnect(disconnect)))
        );
    }
}
