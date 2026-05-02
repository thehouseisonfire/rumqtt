use super::mqttbytes::v5::{
    Auth, AuthProperties, AuthReasonCode, ConnAck, ConnectReturnCode, Disconnect,
    DisconnectReasonCode, Packet, PingReq, PubAck, PubAckReason, PubComp, PubCompReason, PubRec,
    PubRecReason, PubRel, PubRelReason, Publish, SubAck, Subscribe, SubscribeReasonCode, UnsubAck,
    UnsubAckReason, Unsubscribe,
};
use super::mqttbytes::{self, Error as MqttError, QoS};
use crate::notice::{
    PublishNoticeTx, PublishResult, SubscribeNoticeTx, TrackedNoticeTx, UnsubscribeNoticeTx,
};
use crate::{NoticeFailureReason, PublishNoticeError};

use super::{AuthManager, Event, Incoming, Outgoing, Request};

use bytes::Bytes;
use fixedbitset::FixedBitSet;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::{io, time::Instant};

/// Errors during state handling
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
    /// Received a wrong packet while waiting for another packet
    #[error("Received a wrong packet while waiting for another packet")]
    WrongPacket,
    #[error("Timeout while waiting to resolve collision")]
    CollisionTimeout,
    #[error("A Subscribe packet must contain atleast one filter")]
    EmptySubscription,
    #[error("Mqtt serialization/deserialization error: {0}")]
    Deserialization(MqttError),
    #[error(
        "Cannot use topic alias '{alias:?}'. It's greater than the broker's maximum of '{max:?}'."
    )]
    InvalidAlias { alias: u16, max: u16 },
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
    #[error("Auth Manager not set")]
    AuthManagerNotSet,
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

/// State of the mqtt connection.
// Design: Methods will just modify the state of the object without doing any network operations
// Design: All inflight queues are maintained in a pre initialized vec with index as packet id.
// This is done for 2 reasons
// Bad acks or out of order acks aren't O(n) causing cpu spikes
// Any missing acks from the broker are detected during the next recycled use of packet ids
#[derive(Debug)]
pub struct MqttState {
    /// Status of last ping
    pub await_pingresp: bool,
    /// Collision ping count. Collisions stop user requests
    /// which inturn trigger pings. Multiple pings without
    /// resolving collisions will result in error
    pub collision_ping_count: usize,
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
    /// Packet ids acked by broker while waiting to advance last contiguous ack boundary
    pub(crate) outgoing_pub_ack: FixedBitSet,
    /// Packet ids of released `QoS` 2 publishes
    pub(crate) outgoing_rel: FixedBitSet,
    /// Notice handles for outgoing `QoS` 2 pubrels
    pub(crate) outgoing_rel_notice: Vec<Option<PublishNoticeTx>>,
    /// Packet ids on incoming `QoS` 2 publishes
    pub(crate) incoming_pub: FixedBitSet,
    /// Last collision due to broker not acking in order
    pub collision: Option<Publish>,
    /// Notice handle for the collision publish
    pub(crate) collision_notice: Option<PublishNoticeTx>,
    /// Tracked subscribe requests waiting for `SubAck`
    pub(crate) tracked_subscribe: BTreeMap<u16, (Subscribe, SubscribeNoticeTx)>,
    /// Tracked unsubscribe requests waiting for `UnsubAck`
    pub(crate) tracked_unsubscribe: BTreeMap<u16, (Unsubscribe, UnsubscribeNoticeTx)>,
    /// Buffered incoming packets
    pub events: VecDeque<Event>,
    /// Indicates if acknowledgements should be send immediately
    pub manual_acks: bool,
    /// Server-to-client topic aliases scoped to the current network connection.
    incoming_topic_aliases: HashMap<u16, Bytes>,
    /// Client-to-server topic aliases scoped to the current network connection.
    outgoing_topic_aliases: HashMap<u16, Bytes>,
    /// `topic_alias_maximum` RECEIVED via connack packet
    pub broker_topic_alias_max: u16,
    /// Maximum number of allowed inflight `QoS1` & `QoS2` requests
    pub(crate) max_outgoing_inflight: u16,
    /// Upper limit on the maximum number of allowed inflight `QoS1` & `QoS2` requests
    max_outgoing_inflight_upper_limit: u16,
    /// Authentication manager
    auth_manager: Option<Arc<Mutex<dyn AuthManager>>>,
    /// Authentication Method configured on the CONNECT packet.
    authentication_method: Option<String>,
}

/// Builder for low-level MQTT 5 protocol state.
///
/// Most users should configure clients through [`crate::MqttOptions`] and
/// construct them with [`crate::Client::builder`] or [`crate::AsyncClient::builder`].
/// This builder is intended for users driving [`MqttState`] directly.
#[derive(Debug)]
pub struct MqttStateBuilder {
    max_inflight: u16,
    manual_acks: bool,
    auth_manager: Option<Arc<Mutex<dyn AuthManager>>>,
    authentication_method: Option<String>,
}

impl MqttStateBuilder {
    /// Create a new [`MqttState`] builder.
    #[must_use]
    pub const fn new(max_inflight: u16) -> Self {
        Self {
            max_inflight,
            manual_acks: false,
            auth_manager: None,
            authentication_method: None,
        }
    }

    /// Set whether incoming publish acknowledgements should be sent manually.
    #[must_use]
    pub const fn manual_acks(mut self, manual_acks: bool) -> Self {
        self.manual_acks = manual_acks;
        self
    }

    /// Set the Authentication Method used in the CONNECT packet.
    #[must_use]
    pub fn authentication_method(mut self, authentication_method: Option<String>) -> Self {
        self.authentication_method = authentication_method;
        self
    }

    /// Set the authentication manager used for MQTT 5 enhanced authentication.
    #[must_use]
    pub fn auth_manager(mut self, auth_manager: Arc<Mutex<dyn AuthManager>>) -> Self {
        self.auth_manager = Some(auth_manager);
        self
    }

    /// Build the configured [`MqttState`].
    #[must_use]
    pub fn build(self) -> MqttState {
        MqttState::new_internal(
            self.max_inflight,
            self.manual_acks,
            self.authentication_method,
            self.auth_manager,
        )
    }
}

impl MqttState {
    const fn initial_events_capacity() -> usize {
        128
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
            + self.outgoing_rel.ones().count()
            + self.tracked_subscribe.len()
            + self.tracked_unsubscribe.len()
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
        manual_acks: bool,
        authentication_method: Option<String>,
        auth_manager: Option<Arc<Mutex<dyn AuthManager>>>,
    ) -> Self {
        Self {
            await_pingresp: false,
            collision_ping_count: 0,
            last_incoming: Instant::now(),
            last_outgoing: Instant::now(),
            last_pkid: 0,
            last_puback: 0,
            inflight: 0,
            // index 0 is wasted as 0 is not a valid packet id
            outgoing_pub: vec![None; max_inflight as usize + 1],
            outgoing_pub_notice: Self::new_notice_slots(max_inflight),
            outgoing_pub_ack: FixedBitSet::with_capacity(max_inflight as usize + 1),
            outgoing_rel: FixedBitSet::with_capacity(max_inflight as usize + 1),
            outgoing_rel_notice: Self::new_notice_slots(max_inflight),
            incoming_pub: FixedBitSet::with_capacity(u16::MAX as usize + 1),
            collision: None,
            collision_notice: None,
            tracked_subscribe: BTreeMap::new(),
            tracked_unsubscribe: BTreeMap::new(),
            events: VecDeque::with_capacity(Self::initial_events_capacity()),
            manual_acks,
            incoming_topic_aliases: HashMap::new(),
            outgoing_topic_aliases: HashMap::new(),
            // Set via CONNACK
            broker_topic_alias_max: 0,
            max_outgoing_inflight: max_inflight,
            max_outgoing_inflight_upper_limit: max_inflight,
            auth_manager,
            authentication_method,
        }
    }

    /// Set the Authentication Method used in the CONNECT packet for this state.
    ///
    /// Low-level users that send or process MQTT 5 AUTH packets through
    /// [`MqttState`] must keep this value in sync with the CONNECT packet's
    /// Authentication Method property. The event loop updates it automatically
    /// before each CONNECT attempt.
    pub fn set_authentication_method(&mut self, authentication_method: Option<String>) {
        self.authentication_method = authentication_method;
    }

    fn ensure_outgoing_tracking_capacity(&mut self, target_len: usize) {
        if self.outgoing_pub.len() < target_len {
            self.outgoing_pub.resize_with(target_len, || None);
        }

        if self.outgoing_pub_notice.len() < target_len {
            self.outgoing_pub_notice.resize_with(target_len, || None);
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
    }

    pub(crate) fn outbound_requests_drained(&self) -> bool {
        self.inflight == 0
            && self.collision.is_none()
            && self.collision_notice.is_none()
            && self.tracked_subscribe.is_empty()
            && self.tracked_unsubscribe.is_empty()
            && self.outgoing_pub.iter().all(Option::is_none)
            && self.outgoing_pub_notice.iter().all(Option::is_none)
            && self.outgoing_rel_notice.iter().all(Option::is_none)
            && self.outgoing_pub_ack.ones().next().is_none()
            && self.outgoing_rel.ones().next().is_none()
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
        self.outgoing_pub_ack = FixedBitSet::with_capacity(target_len);
        self.outgoing_rel = FixedBitSet::with_capacity(target_len);
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
        self.incoming_topic_aliases.clear();
        self.outgoing_topic_aliases.clear();
        self.broker_topic_alias_max = 0;
    }

    pub(crate) fn replay_topic_aliases(&self) -> HashMap<u16, Bytes> {
        self.outgoing_topic_aliases.clone()
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
        let mut pending = Vec::with_capacity(self.clean_pending_capacity());
        let (first_half, second_half) = self
            .outgoing_pub
            .split_at_mut(self.last_puback as usize + 1);
        let (notice_first_half, notice_second_half) = self
            .outgoing_pub_notice
            .split_at_mut(self.last_puback as usize + 1);

        for (publish, notice) in second_half
            .iter_mut()
            .zip(notice_second_half.iter_mut())
            .chain(first_half.iter_mut().zip(notice_first_half.iter_mut()))
        {
            if let Some(publish) = publish.take() {
                let request = Request::Publish(publish);
                pending.push((request, notice.take().map(TrackedNoticeTx::Publish)));
            } else {
                _ = notice.take();
            }
        }

        // remove and collect pending releases
        for pkid in self.outgoing_rel.ones() {
            let pkid = u16::try_from(pkid).expect("fixedbitset index always fits in u16");
            let request = Request::PubRel(PubRel::new(pkid, None));
            pending.push((
                request,
                self.outgoing_rel_notice[pkid as usize]
                    .take()
                    .map(TrackedNoticeTx::Publish),
            ));
        }
        self.outgoing_rel.clear();
        self.outgoing_pub_ack.clear();

        for (pkid, (mut subscribe, notice)) in std::mem::take(&mut self.tracked_subscribe) {
            subscribe.pkid = pkid;
            pending.push((
                Request::Subscribe(subscribe),
                Some(TrackedNoticeTx::Subscribe(notice)),
            ));
        }
        for (pkid, (mut unsubscribe, notice)) in std::mem::take(&mut self.tracked_unsubscribe) {
            unsubscribe.pkid = pkid;
            pending.push((
                Request::Unsubscribe(unsubscribe),
                Some(TrackedNoticeTx::Unsubscribe(notice)),
            ));
        }

        // remove packed ids of incoming qos2 publishes
        self.incoming_pub.clear();

        self.await_pingresp = false;
        self.collision_ping_count = 0;
        self.inflight = 0;
        pending
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

    pub fn tracked_subscribe_len(&self) -> usize {
        self.tracked_subscribe.len()
    }

    pub fn tracked_unsubscribe_len(&self) -> usize {
        self.tracked_unsubscribe.len()
    }

    pub fn tracked_requests_is_empty(&self) -> bool {
        self.tracked_subscribe.is_empty() && self.tracked_unsubscribe.is_empty()
    }

    pub fn drain_tracked_requests_as_failed(&mut self, reason: NoticeFailureReason) -> usize {
        let mut drained = 0;
        for (_, (_, notice)) in std::mem::take(&mut self.tracked_subscribe) {
            drained += 1;
            notice.error(reason.subscribe_error());
        }
        for (_, (_, notice)) in std::mem::take(&mut self.tracked_unsubscribe) {
            drained += 1;
            notice.error(reason.unsubscribe_error());
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
        let result =
            match request {
                Request::Publish(publish) => {
                    let publish_notice = match notice {
                        Some(TrackedNoticeTx::Publish(notice)) => Some(notice),
                        Some(TrackedNoticeTx::Subscribe(_) | TrackedNoticeTx::Unsubscribe(_))
                        | None => None,
                    };
                    self.outgoing_publish_with_notice(publish, publish_notice)?
                }
                Request::PubRel(pubrel) => {
                    let publish_notice = match notice {
                        Some(TrackedNoticeTx::Publish(notice)) => Some(notice),
                        Some(TrackedNoticeTx::Subscribe(_) | TrackedNoticeTx::Unsubscribe(_))
                        | None => None,
                    };
                    self.outgoing_pubrel_with_notice(pubrel, publish_notice)
                }
                Request::Subscribe(subscribe) => {
                    let request_notice = match notice {
                        Some(TrackedNoticeTx::Subscribe(notice)) => Some(notice),
                        Some(TrackedNoticeTx::Publish(_) | TrackedNoticeTx::Unsubscribe(_))
                        | None => None,
                    };
                    (self.outgoing_subscribe(subscribe, request_notice)?, None)
                }
                Request::Unsubscribe(unsubscribe) => {
                    let request_notice = match notice {
                        Some(TrackedNoticeTx::Unsubscribe(notice)) => Some(notice),
                        Some(TrackedNoticeTx::Publish(_) | TrackedNoticeTx::Subscribe(_))
                        | None => None,
                    };
                    (
                        Some(self.outgoing_unsubscribe(unsubscribe, request_notice)),
                        None,
                    )
                }
                Request::PingReq => (self.outgoing_ping()?, None),
                Request::Disconnect(_) | Request::DisconnectWithTimeout(_, _) => {
                    unreachable!("graceful disconnect requests are handled by the event loop")
                }
                Request::DisconnectNow(disconnect) => {
                    (Some(self.outgoing_disconnect(disconnect)), None)
                }
                Request::PubAck(puback) => (Some(self.outgoing_puback(puback)), None),
                Request::PubRec(pubrec) => (Some(self.outgoing_pubrec(pubrec)), None),
                Request::Auth(auth) => (Some(self.outgoing_auth(auth)?), None),
                _ => unimplemented!(),
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
        mut packet: Incoming,
    ) -> Result<Option<Packet>, StateError> {
        let events_len_before = self.events.len();
        let outgoing = match &mut packet {
            Incoming::PingResp(_) => Ok(self.handle_incoming_pingresp()),
            Incoming::Publish(publish) => self.handle_incoming_publish(publish),
            Incoming::SubAck(suback) => Ok(self.handle_incoming_suback(suback)),
            Incoming::UnsubAck(unsuback) => Ok(self.handle_incoming_unsuback(unsuback)),
            Incoming::PubAck(puback) => self.handle_incoming_puback(puback),
            Incoming::PubRec(pubrec) => self.handle_incoming_pubrec(pubrec),
            Incoming::PubRel(pubrel) => self.handle_incoming_pubrel(pubrel),
            Incoming::PubComp(pubcomp) => self.handle_incoming_pubcomp(pubcomp),
            Incoming::ConnAck(connack) => self.handle_incoming_connack(connack),
            Incoming::Disconnect(disconn) => Self::handle_incoming_disconn(disconn),
            Incoming::Auth(auth) => self.handle_incoming_auth(auth),
            _ => {
                error!("Invalid incoming packet = {packet:?}");
                Err(StateError::WrongPacket)
            }
        };

        let skip_incoming_event = matches!(
            (&packet, &outgoing),
            (Incoming::Publish(_), Ok(Some(Packet::Disconnect(_))))
        );

        // Preserve original event ordering (Incoming first, derived Outgoing next)
        // without cloning the incoming packet.
        if !skip_incoming_event {
            self.events
                .insert(events_len_before, Event::Incoming(packet));
        }

        let outgoing = outgoing?;
        self.last_incoming = Instant::now();
        Ok(outgoing)
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
        self.collision_ping_count = 0;
    }

    fn handle_incoming_suback(&mut self, suback: &SubAck) -> Option<Packet> {
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
        if let Some((_, notice)) = self.tracked_subscribe.remove(&suback.pkid) {
            notice.success(suback.clone());
        }
        None
    }

    fn handle_incoming_unsuback(&mut self, unsuback: &UnsubAck) -> Option<Packet> {
        for reason in &unsuback.reasons {
            if reason != &UnsubAckReason::Success {
                warn!("UnsubAck Pkid = {:?}, Reason = {:?}", unsuback.pkid, reason);
            }
        }
        if let Some((_, notice)) = self.tracked_unsubscribe.remove(&unsuback.pkid) {
            notice.success(unsuback.clone());
        }
        None
    }

    fn handle_incoming_connack(&mut self, connack: &ConnAck) -> Result<Option<Packet>, StateError> {
        if connack.code != ConnectReturnCode::Success {
            return Err(StateError::ConnFail {
                reason: connack.code,
            });
        }

        self.reset_connection_scoped_state();

        if let Some(props) = &connack.properties
            && let Some(topic_alias_max) = props.topic_alias_max
        {
            self.broker_topic_alias_max = topic_alias_max;
        }

        if let Some(props) = &connack.properties
            && let Some(max_inflight) = props.receive_max
        {
            self.max_outgoing_inflight = max_inflight.min(self.max_outgoing_inflight_upper_limit);
            // Shrinking depends on pending retransmission state in eventloop.
            // Grow immediately so incoming/outgoing packet-id indexed tracking stays valid.
            self.reconcile_outgoing_tracking_capacity(false);
        }
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

        if !publish.topic.is_empty() {
            if let Some(alias) = topic_alias {
                self.incoming_topic_aliases
                    .insert(alias, publish.topic.clone());
            }
        } else if let Some(alias) = topic_alias
            && let Some(topic) = self.incoming_topic_aliases.get(&alias)
        {
            topic.clone_into(&mut publish.topic);
        } else if topic_alias.is_some() {
            return self.handle_protocol_error();
        }

        match qos {
            QoS::AtMostOnce => Ok(None),
            QoS::AtLeastOnce => {
                if !self.manual_acks {
                    let puback = PubAck::new(publish.pkid, None);
                    return Ok(Some(self.outgoing_puback(puback)));
                }
                Ok(None)
            }
            QoS::ExactlyOnce => {
                let pkid = publish.pkid;
                self.incoming_pub.insert(pkid as usize);

                if !self.manual_acks {
                    let pubrec = PubRec::new(pkid, None);
                    return Ok(Some(self.outgoing_pubrec(pubrec)));
                }
                Ok(None)
            }
        }
    }

    fn handle_incoming_puback(&mut self, puback: &PubAck) -> Result<Option<Packet>, StateError> {
        let publish = self
            .outgoing_pub
            .get_mut(puback.pkid as usize)
            .ok_or(StateError::Unsolicited(puback.pkid))?;

        if publish.take().is_none() {
            error!("Unsolicited puback packet: {:?}", puback.pkid);
            return Err(StateError::Unsolicited(puback.pkid));
        }
        self.outgoing_pub_ack.set(puback.pkid as usize, true);
        self.advance_last_puback_frontier();

        let notice = self.outgoing_pub_notice[puback.pkid as usize].take();
        self.inflight -= 1;

        if puback.reason != PubAckReason::Success
            && puback.reason != PubAckReason::NoMatchingSubscribers
        {
            warn!(
                "PubAck Pkid = {:?}, reason: {:?}",
                puback.pkid, puback.reason
            );
        }
        if let Some(tx) = notice {
            tx.success(PublishResult::Qos1(puback.clone()));
        }

        Ok(self.replay_collision_publish(puback.pkid))
    }

    fn handle_incoming_pubrec(&mut self, pubrec: &PubRec) -> Result<Option<Packet>, StateError> {
        let publish = self
            .outgoing_pub
            .get_mut(pubrec.pkid as usize)
            .ok_or(StateError::Unsolicited(pubrec.pkid))?;

        if publish.take().is_none() {
            error!("Unsolicited pubrec packet: {:?}", pubrec.pkid);
            return Err(StateError::Unsolicited(pubrec.pkid));
        }

        let notice = self.outgoing_pub_notice[pubrec.pkid as usize].take();
        if pubrec.reason != PubRecReason::Success
            && pubrec.reason != PubRecReason::NoMatchingSubscribers
        {
            warn!(
                "PubRec Pkid = {:?}, reason: {:?}",
                pubrec.pkid, pubrec.reason
            );
            if let Some(tx) = notice {
                tx.success(PublishResult::Qos2PubRecRejected(pubrec.clone()));
            }
            self.inflight -= 1;
            return Ok(self.replay_collision_publish(pubrec.pkid));
        }

        // NOTE: Inflight - 1 for qos2 in comp
        self.outgoing_rel.insert(pubrec.pkid as usize);
        self.outgoing_rel_notice[pubrec.pkid as usize] = notice;
        let event = Event::Outgoing(Outgoing::PubRel(pubrec.pkid));
        self.events.push_back(event);

        Ok(Some(Packet::PubRel(PubRel::new(pubrec.pkid, None))))
    }

    fn handle_incoming_pubrel(&mut self, pubrel: &PubRel) -> Result<Option<Packet>, StateError> {
        if !self.incoming_pub.contains(pubrel.pkid as usize) {
            error!("Unsolicited pubrel packet: {:?}", pubrel.pkid);
            return Err(StateError::Unsolicited(pubrel.pkid));
        }
        self.incoming_pub.set(pubrel.pkid as usize, false);

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

    fn handle_incoming_pubcomp(&mut self, pubcomp: &PubComp) -> Result<Option<Packet>, StateError> {
        if !self.outgoing_rel.contains(pubcomp.pkid as usize) {
            error!("Unsolicited pubcomp packet: {:?}", pubcomp.pkid);
            return Err(StateError::Unsolicited(pubcomp.pkid));
        }
        self.outgoing_rel.set(pubcomp.pkid as usize, false);
        let notice = self.outgoing_rel_notice[pubcomp.pkid as usize].take();
        self.inflight -= 1;

        if pubcomp.reason != PubCompReason::Success {
            warn!(
                "PubComp Pkid = {:?}, reason: {:?}",
                pubcomp.pkid, pubcomp.reason
            );
        }
        if let Some(tx) = notice {
            tx.success(PublishResult::Qos2Completed(pubcomp.clone()));
        }

        Ok(self.replay_collision_publish(pubcomp.pkid))
    }

    fn replay_collision_publish(&mut self, pkid: u16) -> Option<Packet> {
        self.check_collision(pkid).map(|(publish, notice)| {
            let pkid = publish.pkid;
            let replay_publish = self.publish_for_replay_tracking(&publish);
            self.outgoing_pub[pkid as usize] = Some(replay_publish);
            self.outgoing_pub_notice[pkid as usize] = notice;
            self.inflight += 1;
            self.record_outgoing_topic_alias(&publish);

            let event = Event::Outgoing(Outgoing::Publish(pkid));
            self.events.push_back(event);
            self.collision_ping_count = 0;

            Packet::Publish(publish)
        })
    }

    const fn handle_incoming_pingresp(&mut self) -> Option<Packet> {
        self.await_pingresp = false;
        None
    }

    fn handle_incoming_auth(&mut self, auth: &Auth) -> Result<Option<Packet>, StateError> {
        match auth.code {
            AuthReasonCode::Success => {
                self.validate_incoming_auth_method(auth.properties.as_ref())?;
                Ok(None)
            }
            AuthReasonCode::Continue => {
                self.validate_incoming_auth_method(auth.properties.as_ref())?;
                let props = auth.properties.clone();

                // Check if auth manager is set
                if self.auth_manager.is_none() {
                    return Err(StateError::AuthManagerNotSet);
                }

                let auth_manager = self.auth_manager.clone().unwrap();

                // Call auth_continue method of auth manager
                let auth_data = auth_manager.lock().unwrap().auth_continue(props);
                let out_auth_props = match auth_data {
                    Ok(data) => data,
                    Err(err) => return Err(StateError::AuthError(err)),
                };

                let client_auth = Auth::new(AuthReasonCode::Continue, out_auth_props);

                Ok(Some(self.outgoing_auth(client_auth)?))
            }
            AuthReasonCode::ReAuthenticate => Err(Self::auth_protocol_error()),
        }
    }

    fn configured_authentication_method(&self) -> Result<&str, StateError> {
        self.authentication_method.as_deref().ok_or_else(|| {
            StateError::AuthError("AUTH packet requires a CONNECT Authentication Method".to_owned())
        })
    }

    const fn auth_protocol_error() -> StateError {
        StateError::Deserialization(MqttError::ProtocolError)
    }

    fn validate_incoming_auth_method(
        &self,
        properties: Option<&AuthProperties>,
    ) -> Result<(), StateError> {
        let expected = self
            .configured_authentication_method()
            .map_err(|_| Self::auth_protocol_error())?;

        let Some(actual) = properties.and_then(|properties| properties.method.as_deref()) else {
            return Err(Self::auth_protocol_error());
        };

        if actual != expected {
            return Err(Self::auth_protocol_error());
        }

        Ok(())
    }

    fn normalize_outgoing_auth_properties(
        &self,
        properties: Option<AuthProperties>,
    ) -> Result<AuthProperties, StateError> {
        let expected = self.configured_authentication_method()?.to_owned();
        let mut properties = properties.unwrap_or_default();

        match &properties.method {
            Some(actual) if actual != &expected => {
                return Err(StateError::AuthError(format!(
                    "AUTH packet Authentication Method '{actual}' does not match CONNECT Authentication Method '{expected}'"
                )));
            }
            Some(_) => {}
            None => properties.method = Some(expected),
        }

        Ok(properties)
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
        mut publish: Publish,
        notice: Option<PublishNoticeTx>,
    ) -> Result<(Option<Packet>, Option<PublishNoticeTx>), StateError> {
        let mut notice = notice;
        self.validate_outgoing_topic_alias(&publish)?;

        if publish.qos != QoS::AtMostOnce {
            if publish.pkid == 0 {
                publish.pkid = self.next_pkid();
            }

            let pkid = publish.pkid;
            if self
                .outgoing_pub
                .get(publish.pkid as usize)
                .ok_or(StateError::Unsolicited(publish.pkid))?
                .is_some()
            {
                info!("Collision on packet id = {:?}", publish.pkid);
                self.collision = Some(publish);
                self.collision_notice = notice.take();
                let event = Event::Outgoing(Outgoing::AwaitAck(pkid));
                self.events.push_back(event);
                return Ok((None, None));
            }

            // if there is an existing publish at this pkid, this implies that broker hasn't acked this
            // packet yet. This error is possible only when broker isn't acking sequentially
            let replay_publish = self.publish_for_replay_tracking(&publish);
            self.outgoing_pub[pkid as usize] = Some(replay_publish);
            self.outgoing_pub_notice[pkid as usize] = notice.take();
            self.outgoing_pub_ack.set(pkid as usize, false);
            self.inflight += 1;
        }

        debug!(
            "Publish. Topic = {}, Pkid = {:?}, Payload Size = {:?}",
            String::from_utf8_lossy(&publish.topic),
            publish.pkid,
            publish.payload.len()
        );

        let pkid = publish.pkid;
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
    ) -> (Option<Packet>, Option<PublishNoticeTx>) {
        let pubrel = self.save_pubrel_with_notice(pubrel, notice);

        debug!("Pubrel. Pkid = {}", pubrel.pkid);

        let event = Event::Outgoing(Outgoing::PubRel(pubrel.pkid));
        self.events.push_back(event);

        (Some(Packet::PubRel(PubRel::new(pubrel.pkid, None))), None)
    }

    fn outgoing_puback(&mut self, puback: PubAck) -> Packet {
        let pkid = puback.pkid;
        let event = Event::Outgoing(Outgoing::PubAck(pkid));
        self.events.push_back(event);

        Packet::PubAck(puback)
    }

    fn outgoing_pubrec(&mut self, pubrec: PubRec) -> Packet {
        let pkid = pubrec.pkid;
        let event = Event::Outgoing(Outgoing::PubRec(pkid));
        self.events.push_back(event);

        Packet::PubRec(pubrec)
    }

    /// check when the last control packet/pingreq packet is received and return
    /// the status which tells if keep alive time has exceeded
    /// NOTE: status will be checked for zero keepalive times also
    fn outgoing_ping(&mut self) -> Result<Option<Packet>, StateError> {
        let elapsed_in = self.last_incoming.elapsed();
        let elapsed_out = self.last_outgoing.elapsed();

        if self.collision.is_some() {
            self.collision_ping_count += 1;
            if self.collision_ping_count >= 2 {
                return Err(StateError::CollisionTimeout);
            }
        }

        // raise error if last ping didn't receive ack
        if self.await_pingresp {
            return Err(StateError::AwaitPingResp);
        }

        self.await_pingresp = true;

        debug!(
            "Pingreq, last incoming packet before {elapsed_in:?}, last outgoing request before {elapsed_out:?}",
        );

        let event = Event::Outgoing(Outgoing::PingReq);
        self.events.push_back(event);

        Ok(Some(Packet::PingReq(PingReq)))
    }

    fn outgoing_subscribe(
        &mut self,
        mut subscription: Subscribe,
        notice: Option<SubscribeNoticeTx>,
    ) -> Result<Option<Packet>, StateError> {
        if subscription.filters.is_empty() {
            return Err(StateError::EmptySubscription);
        }

        let pkid = self.next_pkid();
        subscription.pkid = pkid;

        debug!(
            "Subscribe. Topics = {:?}, Pkid = {:?}",
            subscription.filters, subscription.pkid
        );

        let pkid = subscription.pkid;
        let event = Event::Outgoing(Outgoing::Subscribe(pkid));
        self.events.push_back(event);
        if let Some(notice) = notice {
            self.tracked_subscribe
                .insert(subscription.pkid, (subscription.clone(), notice));
        }

        Ok(Some(Packet::Subscribe(subscription)))
    }

    fn outgoing_unsubscribe(
        &mut self,
        mut unsub: Unsubscribe,
        notice: Option<UnsubscribeNoticeTx>,
    ) -> Packet {
        let pkid = self.next_pkid();
        unsub.pkid = pkid;

        debug!(
            "Unsubscribe. Topics = {:?}, Pkid = {:?}",
            unsub.filters, unsub.pkid
        );

        let pkid = unsub.pkid;
        let event = Event::Outgoing(Outgoing::Unsubscribe(pkid));
        self.events.push_back(event);
        if let Some(notice) = notice {
            self.tracked_unsubscribe
                .insert(unsub.pkid, (unsub.clone(), notice));
        }

        Packet::Unsubscribe(unsub)
    }

    fn outgoing_disconnect(&mut self, disconnect: Disconnect) -> Packet {
        let reason = disconnect.reason_code;
        debug!("Disconnect with {reason:?}");
        let event = Event::Outgoing(Outgoing::Disconnect);
        self.events.push_back(event);

        Packet::Disconnect(disconnect)
    }

    fn outgoing_auth(&mut self, mut auth: Auth) -> Result<Packet, StateError> {
        let props = self.normalize_outgoing_auth_properties(auth.properties.take())?;
        debug!(
            "Auth packet sent. Auth Method: {:?}. Auth Data: {:?}",
            props.method, props.data
        );
        auth.properties = Some(props);
        let event = Event::Outgoing(Outgoing::Auth);
        self.events.push_back(event);
        Ok(Packet::Auth(auth))
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
        mut pubrel: PubRel,
        notice: Option<PublishNoticeTx>,
    ) -> PubRel {
        let pubrel = match pubrel.pkid {
            // consider PacketIdentifier(0) as uninitialized packets
            0 => {
                pubrel.pkid = self.next_pkid();
                pubrel
            }
            _ => pubrel,
        };

        self.outgoing_rel.insert(pubrel.pkid as usize);
        self.outgoing_rel_notice[pubrel.pkid as usize] = notice;
        self.inflight += 1;
        pubrel
    }

    fn advance_last_puback_frontier(&mut self) {
        let mut next = self.next_puback_boundary_pkid(self.last_puback);
        while next != 0 && self.outgoing_pub_ack.contains(next as usize) {
            self.outgoing_pub_ack.set(next as usize, false);
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

    /// <http://stackoverflow.com/questions/11115364/mqtt-messageid-practical-implementation>
    /// Packet ids are incremented till maximum set inflight messages and reset to 1 after that.
    ///
    const fn next_pkid(&mut self) -> u16 {
        let next_pkid = self.last_pkid + 1;

        // When next packet id is at the edge of inflight queue,
        // set await flag. This instructs eventloop to stop
        // processing requests until all the inflight publishes
        // are acked
        if next_pkid == self.max_outgoing_inflight {
            self.last_pkid = 0;
            return next_pkid;
        }

        self.last_pkid = next_pkid;
        next_pkid
    }

    fn publish_topic_alias(publish: &Publish) -> Option<u16> {
        publish
            .properties
            .as_ref()
            .and_then(|props| props.topic_alias)
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
            && let Some(topic) = self.outgoing_topic_aliases.get(&alias)
        {
            topic.clone_into(&mut replay_publish.topic);
        }

        replay_publish
    }

    fn validate_outgoing_topic_alias(&self, publish: &Publish) -> Result<(), StateError> {
        if let Some(alias) = Self::publish_topic_alias(publish)
            && alias > self.broker_topic_alias_max
        {
            // We MUST NOT send a Topic Alias that is greater than the
            // broker's Topic Alias Maximum.
            return Err(StateError::InvalidAlias {
                alias,
                max: self.broker_topic_alias_max,
            });
        }

        Ok(())
    }

    fn record_outgoing_topic_alias(&mut self, publish: &Publish) {
        if !publish.topic.is_empty()
            && let Some(alias) = Self::publish_topic_alias(publish)
        {
            self.outgoing_topic_aliases
                .insert(alias, publish.topic.clone());
        }
    }
}

impl Clone for MqttState {
    fn clone(&self) -> Self {
        Self {
            await_pingresp: self.await_pingresp,
            collision_ping_count: self.collision_ping_count,
            last_incoming: self.last_incoming,
            last_outgoing: self.last_outgoing,
            last_pkid: self.last_pkid,
            last_puback: self.last_puback,
            inflight: self.inflight,
            outgoing_pub: self.outgoing_pub.clone(),
            outgoing_pub_notice: Self::new_notice_slots_with_len(self.outgoing_pub.len()),
            outgoing_pub_ack: self.outgoing_pub_ack.clone(),
            outgoing_rel: self.outgoing_rel.clone(),
            outgoing_rel_notice: Self::new_notice_slots_with_len(self.outgoing_rel_notice.len()),
            incoming_pub: self.incoming_pub.clone(),
            collision: self.collision.clone(),
            collision_notice: None,
            tracked_subscribe: BTreeMap::new(),
            tracked_unsubscribe: BTreeMap::new(),
            events: self.events.clone(),
            manual_acks: self.manual_acks,
            incoming_topic_aliases: self.incoming_topic_aliases.clone(),
            outgoing_topic_aliases: self.outgoing_topic_aliases.clone(),
            broker_topic_alias_max: self.broker_topic_alias_max,
            max_outgoing_inflight: self.max_outgoing_inflight,
            max_outgoing_inflight_upper_limit: self.max_outgoing_inflight_upper_limit,
            auth_manager: self.auth_manager.clone(),
            authentication_method: self.authentication_method.clone(),
        }
    }
}

#[cfg(test)]
mod test {
    use super::mqttbytes::v5::*;
    use super::mqttbytes::*;
    use super::{Event, Incoming, Outgoing, Request};
    use super::{MqttState, StateError};
    use crate::NoticeFailureReason;
    use crate::notice::{
        PublishNotice, PublishNoticeError, PublishNoticeTx, PublishResult, SubscribeNoticeError,
        SubscribeNoticeTx, UnsubscribeNoticeError, UnsubscribeNoticeTx,
    };
    use bytes::Bytes;
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};

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

    fn build_auth_mqttstate(authentication_method: Option<&str>) -> MqttState {
        MqttState::builder(10)
            .authentication_method(authentication_method.map(str::to_owned))
            .build()
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

    impl crate::AuthManager for StaticAuthManager {
        fn auth_continue(
            &mut self,
            _auth_prop: Option<AuthProperties>,
        ) -> Result<Option<AuthProperties>, String> {
            self.response.clone()
        }
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

    #[test]
    fn new_state_preallocates_event_queue_for_read_batch_bursts() {
        let mqtt = MqttState::builder(10).build();
        assert!(mqtt.events.capacity() >= MqttState::initial_events_capacity());
    }

    #[test]
    fn clean_pending_capacity_counts_publish_rel_and_tracked_requests() {
        let mut mqtt = MqttState::builder(10).build();
        mqtt.outgoing_pub[1] = Some(build_outgoing_publish(QoS::AtLeastOnce));
        mqtt.outgoing_pub[2] = Some(build_outgoing_publish(QoS::ExactlyOnce));
        mqtt.outgoing_rel.insert(3);
        mqtt.outgoing_rel.insert(4);

        let filter = Filter::new("a/b", QoS::AtMostOnce);
        let (sub_notice, _) = SubscribeNoticeTx::new();
        mqtt.tracked_subscribe
            .insert(5, (Subscribe::new(filter, None), sub_notice));

        let (unsub_notice, _) = UnsubscribeNoticeTx::new();
        mqtt.tracked_unsubscribe
            .insert(6, (Unsubscribe::new("a/b", None), unsub_notice));

        assert_eq!(mqtt.clean_pending_capacity(), 6);
    }

    #[test]
    fn tracked_request_len_helpers_report_counts() {
        let mut mqtt = MqttState::builder(10).build();
        let filter = Filter::new("a/b", QoS::AtMostOnce);
        let (sub_notice, _) = SubscribeNoticeTx::new();
        mqtt.tracked_subscribe
            .insert(5, (Subscribe::new(filter, None), sub_notice));
        let (unsub_notice, _) = UnsubscribeNoticeTx::new();
        mqtt.tracked_unsubscribe
            .insert(6, (Unsubscribe::new("a/b", None), unsub_notice));

        assert_eq!(mqtt.tracked_subscribe_len(), 1);
        assert_eq!(mqtt.tracked_unsubscribe_len(), 1);
        assert!(!mqtt.tracked_requests_is_empty());

        mqtt.drain_tracked_requests_as_failed(NoticeFailureReason::SessionReset);
        assert!(mqtt.tracked_requests_is_empty());
    }

    #[test]
    fn drain_tracked_requests_as_failed_reports_session_reset_and_returns_count() {
        let mut mqtt = MqttState::builder(10).build();
        let filter = Filter::new("a/b", QoS::AtMostOnce);
        let (sub_notice_tx, sub_notice) = SubscribeNoticeTx::new();
        mqtt.tracked_subscribe
            .insert(5, (Subscribe::new(filter, None), sub_notice_tx));
        let (unsub_notice_tx, unsub_notice) = UnsubscribeNoticeTx::new();
        mqtt.tracked_unsubscribe
            .insert(6, (Unsubscribe::new("a/b", None), unsub_notice_tx));

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
    fn tracked_suback_returns_ack_with_properties_and_preserves_incoming_event() {
        let mut mqtt = build_mqttstate();
        let (tx, notice) = SubscribeNoticeTx::new();
        mqtt.outgoing_subscribe(
            Subscribe::new(Filter::new("a/b", QoS::AtMostOnce), None),
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
    fn tracked_unsuback_returns_ack_with_properties_and_preserves_incoming_event() {
        let mut mqtt = build_mqttstate();
        let (tx, notice) = UnsubscribeNoticeTx::new();
        mqtt.outgoing_unsubscribe(Unsubscribe::new("a/b", None), Some(tx));
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

    fn build_connack_with_receive_max(receive_max: u16) -> ConnAck {
        ConnAck {
            session_present: false,
            code: ConnectReturnCode::Success,
            properties: Some(ConnAckProperties {
                session_expiry_interval: None,
                receive_max: Some(receive_max),
                max_qos: None,
                retain_available: None,
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
    fn connack_receive_max_can_grow_tracking_capacity_after_previous_shrink() {
        let mut mqtt = MqttState::builder(10).build();
        mqtt.handle_incoming_packet(Incoming::ConnAck(build_connack_with_receive_max(4)))
            .unwrap();
        mqtt.reconcile_outgoing_tracking_capacity(true);
        assert_eq!(mqtt.outgoing_pub.len(), 5);

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
        mqtt.broker_topic_alias_max = 10;
        mqtt.outgoing_publish(build_outgoing_publish_with_alias(
            "hello/replay",
            QoS::AtMostOnce,
            2,
        ))
        .unwrap();

        mqtt.handle_incoming_packet(Incoming::ConnAck(build_connack_with_receive_max(5)))
            .unwrap();

        assert_eq!(mqtt.broker_topic_alias_max, 0);
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
            let pkid = mqtt.next_pkid();

            // loops between 0-99. % 100 == 0 implies border
            let expected = i % 100;
            if expected == 0 {
                break;
            }

            assert_eq!(expected, pkid);
        }
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

        // This should cause a collition
        mqtt.outgoing_publish(publish.clone()).unwrap();
        assert_eq!(mqtt.last_pkid, 1);
        assert_eq!(mqtt.inflight, 2);
        assert!(mqtt.collision.is_some());

        mqtt.handle_incoming_puback(&PubAck::new(1, None)).unwrap();
        mqtt.handle_incoming_puback(&PubAck::new(2, None)).unwrap();
        assert_eq!(mqtt.inflight, 1);

        // Now there should be space in the outgoing queue
        mqtt.outgoing_publish(publish).unwrap();
        assert_eq!(mqtt.last_pkid, 0);
        assert_eq!(mqtt.inflight, 2);
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
    fn incoming_publish_should_be_added_to_queue_correctly() {
        let mut mqtt = build_mqttstate();

        // QoS0, 1, 2 Publishes
        let mut publish1 = build_incoming_publish(QoS::AtMostOnce, 1);
        let mut publish2 = build_incoming_publish(QoS::AtLeastOnce, 2);
        let mut publish3 = build_incoming_publish(QoS::ExactlyOnce, 3);

        mqtt.handle_incoming_publish(&mut publish1).unwrap();
        mqtt.handle_incoming_publish(&mut publish2).unwrap();
        mqtt.handle_incoming_publish(&mut publish3).unwrap();

        // only qos2 publish should be add to queue
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
    fn incoming_publish_should_not_be_acked_with_manual_acks() {
        let mut mqtt = build_mqttstate();
        mqtt.manual_acks = true;

        // QoS0, 1, 2 Publishes
        let mut publish1 = build_incoming_publish(QoS::AtMostOnce, 1);
        let mut publish2 = build_incoming_publish(QoS::AtLeastOnce, 2);
        let mut publish3 = build_incoming_publish(QoS::ExactlyOnce, 3);

        mqtt.handle_incoming_publish(&mut publish1).unwrap();
        mqtt.handle_incoming_publish(&mut publish2).unwrap();
        mqtt.handle_incoming_publish(&mut publish3).unwrap();

        assert!(mqtt.incoming_pub.contains(3));
        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn unknown_incoming_topic_alias_returns_protocol_error_disconnect() {
        let mut mqtt = build_mqttstate();
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
        let mut mqtt = build_mqttstate();
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
    fn incoming_auth_success_accepts_matching_authentication_method() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
        let auth = Auth::new(
            AuthReasonCode::Success,
            Some(auth_properties(Some(AUTH_METHOD))),
        );

        let packet = mqtt.handle_incoming_packet(Incoming::Auth(auth)).unwrap();

        assert!(packet.is_none());
    }

    #[test]
    fn incoming_auth_success_rejects_missing_authentication_method() {
        let mut mqtt = build_auth_mqttstate(Some(AUTH_METHOD));
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
        let mut mqtt = build_mqttstate();
        mqtt.broker_topic_alias_max = 10;
        let mut aliased = build_incoming_publish(QoS::AtMostOnce, 0);
        aliased.properties = Some(publish_properties_with_alias(1));
        mqtt.handle_incoming_publish(&mut aliased).unwrap();

        let mut alias_only = build_incoming_publish(QoS::AtMostOnce, 0);
        alias_only.topic = Bytes::new();
        alias_only.properties = Some(publish_properties_with_alias(1));
        mqtt.handle_incoming_publish(&mut alias_only).unwrap();
        assert_eq!(alias_only.topic, Bytes::from_static(b"hello/world"));

        mqtt.reset_connection_scoped_state();

        assert_eq!(mqtt.broker_topic_alias_max, 0);
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
        mqtt.broker_topic_alias_max = 10;
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
        next_connection.broker_topic_alias_max = 0;

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
    fn public_clean_repairs_alias_only_publish_when_mapping_is_known() {
        let mut mqtt = build_mqttstate();
        mqtt.broker_topic_alias_max = 10;
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
        assert_eq!(mqtt.broker_topic_alias_max, 0);
    }

    #[test]
    fn public_clean_preserves_alias_only_publish_topic_from_send_time_after_rebind() {
        let mut mqtt = build_mqttstate();
        mqtt.broker_topic_alias_max = 10;
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
        mqtt.broker_topic_alias_max = 10;
        mqtt.outgoing_publish(build_outgoing_publish_with_alias("", QoS::AtLeastOnce, 3))
            .unwrap();

        let requests = mqtt.clean();

        assert!(requests.is_empty());
        assert_eq!(mqtt.broker_topic_alias_max, 0);
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
    fn incoming_pubrec_failure_without_collision_decrements_inflight() {
        let mut mqtt = build_mqttstate();
        let first_notice =
            queue_publish_with_notice(&mut mqtt, build_outgoing_publish(QoS::ExactlyOnce));

        let mut pubrec = PubRec::new(1, None);
        pubrec.reason = PubRecReason::ImplementationSpecificError;

        assert!(mqtt.handle_incoming_pubrec(&pubrec).unwrap().is_none());
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

        assert!(mqtt.handle_incoming_pubcomp(&pubcomp).unwrap().is_none());
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
        mqtt.handle_incoming_packet(Incoming::PingResp(PingResp))
            .unwrap();

        // should ping
        mqtt.outgoing_ping().unwrap();
    }
}
