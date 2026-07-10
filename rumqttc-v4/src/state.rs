use crate::notice::{
    DeferredNotice, PublishNoticeTx, PublishResult, SubscribeNoticeTx, TrackedNoticeTx,
    UnsubscribeNoticeTx,
};
use crate::session::{
    PersistedAckMode, PersistedFilter, PersistedIncomingQos2, PersistedPubRel, PersistedPublish,
    PersistedQoS, PersistedRequest, PersistedSession, PersistedSubscribe, PersistedUnsubscribe,
    SessionRestoreError,
};
use crate::{AckMode, Event, Incoming, NoticeFailureReason, Outgoing, PublishNoticeError, Request};

use crate::mqttbytes::v4::{
    Packet, PubAck, PubComp, PubRec, PubRel, Publish, SubAck, Subscribe, UnsubAck, Unsubscribe,
};
use crate::mqttbytes::{self, PacketType, QoS};
use bytes::Bytes;
use fixedbitset::FixedBitSet;
use std::collections::{BTreeMap, VecDeque};
use std::{io, time::Instant};

/// Errors during state handling
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// Io Error while state is passed to network
    #[error("Io error: {0:?}")]
    Io(#[from] io::Error),
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
    Deserialization(#[from] mqttbytes::Error),
    #[error("MQTT protocol violation: {0}")]
    ProtocolViolation(ProtocolViolation),
    #[error("Connection closed by peer abruptly")]
    ConnectionAborted,
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

/// State of the mqtt connection.
// Design: Methods will just modify the state of the object without doing any network operations
// Design: All inflight queues are maintained in a pkid-indexed vec/bitset structure.
// This is done for 2 reasons
// Bad acks or out of order acks aren't O(n) causing cpu spikes
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
    fn is_none(&self) -> bool {
        self.outgoing.is_none()
    }
}

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
    /// Maximum number of allowed inflight
    pub(crate) max_inflight: u16,
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
}

/// Builder for low-level MQTT 3.1.1 protocol state.
///
/// Most users should configure clients through [`crate::MqttOptions`] and
/// construct them with [`crate::Client::builder`] or [`crate::AsyncClient::builder`].
/// This builder is intended for users driving [`MqttState`] directly.
#[derive(Debug)]
pub struct MqttStateBuilder {
    max_inflight: u16,
    ack_mode: AckMode,
}

impl MqttStateBuilder {
    /// Create a new [`MqttState`] builder.
    #[must_use]
    pub const fn new(max_inflight: u16) -> Self {
        Self {
            max_inflight,
            ack_mode: AckMode::Automatic,
        }
    }

    /// Set how incoming publish acknowledgements are handled.
    #[must_use]
    pub const fn ack_mode(mut self, ack_mode: AckMode) -> Self {
        self.ack_mode = ack_mode;
        self
    }

    /// Build the configured [`MqttState`].
    #[must_use]
    pub fn build(self) -> MqttState {
        MqttState::new_internal(self.max_inflight, self.ack_mode)
    }
}

impl MqttState {
    const WARM_TRACKING_SLOTS: usize = 32;

    const fn initial_events_capacity() -> usize {
        128
    }

    const fn full_packet_identifier_len() -> usize {
        u16::MAX as usize + 1
    }

    const fn outgoing_tracking_len(max_inflight: u16) -> usize {
        max_inflight as usize + 1
    }

    const fn warm_tracking_len(max_inflight: u16) -> usize {
        let full_len = Self::outgoing_tracking_len(max_inflight);
        let warm_len = Self::WARM_TRACKING_SLOTS + 1;
        if full_len < warm_len {
            full_len
        } else {
            warm_len
        }
    }

    fn new_notice_slots_with_len(len: usize) -> Vec<Option<PublishNoticeTx>> {
        std::iter::repeat_with(|| None).take(len).collect()
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

        if self.outgoing_pub_flush_attempted.len() < target_len {
            self.outgoing_pub_flush_attempted.grow(target_len);
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
            && self.outgoing_pub_flush_attempted.ones().next().is_none()
            && self.outgoing_rel_notice.iter().all(Option::is_none)
            && self.outgoing_pub_ack.ones().next().is_none()
            && self.outgoing_rel.ones().next().is_none()
            && self.outgoing_rel_replay.ones().next().is_none()
            && self.outbound_pkid_count == 0
    }

    pub(crate) fn outbound_drain_diagnostics(&self) -> String {
        format!(
            "inflight={}, collision={}, collision_notice={}, pending_subscribe={}, \
             pending_unsubscribe={}, outgoing_pub={}, outgoing_pub_notice={}, \
             outgoing_pub_flush_attempted={}, outgoing_rel_notice={}, outgoing_pub_ack={}, \
             outgoing_rel={}, outgoing_rel_replay={}, incoming_pub={}, incoming_puback={}, incoming_pubrec={}",
            self.inflight,
            self.collision.is_some(),
            self.collision_notice.is_some(),
            self.pending_subscribe.len(),
            self.pending_unsubscribe.len(),
            self.outgoing_pub
                .iter()
                .filter(|publish| publish.is_some())
                .count(),
            self.outgoing_pub_notice
                .iter()
                .filter(|notice| notice.is_some())
                .count(),
            self.outgoing_pub_flush_attempted.ones().count(),
            self.outgoing_rel_notice
                .iter()
                .filter(|notice| notice.is_some())
                .count(),
            self.outgoing_pub_ack.ones().count(),
            self.outgoing_rel.ones().count(),
            self.outgoing_rel_replay.ones().count(),
            self.incoming_pub.ones().count(),
            self.incoming_puback.ones().count(),
            self.incoming_pubrec.ones().count()
        )
    }

    fn maybe_shrink_outgoing_tracking_capacity(&mut self) {
        let target_len = Self::warm_tracking_len(self.max_inflight);
        if self.outgoing_pub.len() <= target_len || !self.outbound_requests_drained() {
            return;
        }

        self.outgoing_pub.truncate(target_len);
        self.outgoing_pub_notice.truncate(target_len);
        self.outgoing_rel_notice.truncate(target_len);
        self.outgoing_pub_flush_attempted = FixedBitSet::with_capacity(target_len);
        self.outgoing_pub_ack = FixedBitSet::with_capacity(target_len);
        self.outgoing_rel = FixedBitSet::with_capacity(target_len);
        self.last_pkid = 0;
        self.last_puback = 0;
    }

    const fn validate_outgoing_pkid_bound(&self, pkid: u16) -> Result<(), StateError> {
        if pkid == 0 || pkid > self.max_inflight {
            return Err(StateError::Unsolicited(pkid));
        }

        Ok(())
    }

    const fn next_publish_pkid_after(&self, pkid: u16) -> u16 {
        if pkid >= self.max_inflight {
            1
        } else {
            pkid + 1
        }
    }

    fn packet_identifier_in_use(&self, pkid: u16) -> bool {
        self.outbound_pkid_in_use.contains(usize::from(pkid))
    }

    pub(crate) fn can_send_publish(&self, publish: &Publish) -> bool {
        if publish.qos == QoS::AtMostOnce {
            return true;
        }

        if self.inflight >= self.max_inflight || self.collision.is_some() {
            return false;
        }

        if publish.pkid == 0 {
            return self.next_publish_pkid().is_some();
        }

        self.validate_outgoing_pkid_bound(publish.pkid).is_ok()
            && !self.packet_identifier_in_use(publish.pkid)
    }

    pub(crate) const fn control_packet_identifier_available(&self) -> bool {
        self.outbound_pkid_count < u16::MAX
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

    /// Create a builder for low-level MQTT 3.1.1 protocol state.
    #[must_use]
    pub const fn builder(max_inflight: u16) -> MqttStateBuilder {
        MqttStateBuilder::new(max_inflight)
    }

    /// Creates new mqtt state. Same state should be used during a
    /// connection for persistent sessions while new state should
    /// instantiated for clean sessions.
    #[must_use]
    pub(crate) fn new_internal(max_inflight: u16, ack_mode: AckMode) -> Self {
        let tracking_len = Self::warm_tracking_len(max_inflight);
        Self {
            ping: PingState::new(),
            last_incoming: Instant::now(),
            last_outgoing: Instant::now(),
            last_pkid: 0,
            last_puback: 0,
            inflight: 0,
            max_inflight,
            // index 0 is wasted as 0 is not a valid packet id
            outgoing_pub: std::iter::repeat_with(|| None).take(tracking_len).collect(),
            outgoing_pub_notice: Self::new_notice_slots_with_len(tracking_len),
            outgoing_pub_flush_attempted: FixedBitSet::with_capacity(tracking_len),
            outgoing_pub_ack: FixedBitSet::with_capacity(tracking_len),
            outgoing_rel: FixedBitSet::with_capacity(tracking_len),
            outgoing_rel_replay: FixedBitSet::with_capacity(tracking_len),
            outgoing_rel_notice: Self::new_notice_slots_with_len(tracking_len),
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
        }
    }

    /// Returns whether the current connection is waiting for a `PINGRESP`.
    pub const fn await_pingresp(&self) -> bool {
        self.ping.await_pingresp()
    }

    /// Returns the number of consecutive collision pings observed.
    pub const fn collision_ping_count(&self) -> usize {
        self.ping.collision_count()
    }

    pub(crate) fn clean_with_notices(&mut self) -> Vec<(Request, Option<TrackedNoticeTx>)> {
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
                released_publish_pkids.push(publish.pkid);
                let request = Request::Publish(publish);
                pending.push((request, notice.take().map(TrackedNoticeTx::Publish)));
            } else {
                _ = notice.take();
            }
        }
        for pkid in released_publish_pkids {
            self.release_outbound_pkid(pkid);
        }

        if let Some(publish) = self.collision.take() {
            pending.push((
                Request::Publish(publish),
                self.collision_notice.take().map(TrackedNoticeTx::Publish),
            ));
        }

        // remove and collect pending releases
        for pkid in self.outgoing_rel.ones() {
            let pkid = u16::try_from(pkid).expect("fixedbitset index always fits in u16");
            let request = Request::PubRel(PubRel::new(pkid));
            pending.push((
                request,
                self.outgoing_rel_notice
                    .get_mut(usize::from(pkid))
                    .expect("outgoing_rel pkid within notice vec bounds")
                    .take()
                    .map(TrackedNoticeTx::Publish),
            ));
        }
        self.outgoing_rel_replay = self.outgoing_rel.clone();
        self.outgoing_rel.clear();
        self.outgoing_pub_flush_attempted.clear();
        self.outgoing_pub_ack.clear();

        for (pkid, mut pending_subscribe) in std::mem::take(&mut self.pending_subscribe) {
            pending_subscribe.subscribe.pkid = pkid;
            self.release_outbound_pkid(pkid);
            pending.push((
                Request::Subscribe(pending_subscribe.subscribe),
                pending_subscribe.notice.map(TrackedNoticeTx::Subscribe),
            ));
        }

        for (pkid, mut pending_unsubscribe) in std::mem::take(&mut self.pending_unsubscribe) {
            pending_unsubscribe.unsubscribe.pkid = pkid;
            self.release_outbound_pkid(pkid);
            pending.push((
                Request::Unsubscribe(pending_unsubscribe.unsubscribe),
                pending_unsubscribe.notice.map(TrackedNoticeTx::Unsubscribe),
            ));
        }

        // QoS 1 receives are not part of the client session state. Incomplete QoS 2
        // receives are, so keep incoming_pub/incoming_pubrec for persistent-session reconnects.
        self.incoming_puback.clear();

        self.ping.reset();
        self.inflight = 0;
        if pending.is_empty() {
            self.maybe_shrink_outgoing_tracking_capacity();
        }
        pending
    }

    pub(crate) fn mark_outgoing_publishes_flush_attempted(&mut self) {
        for (pkid, publish) in self.outgoing_pub.iter().enumerate() {
            if publish.is_some() {
                self.outgoing_pub_flush_attempted.set(pkid, true);
            }
        }
    }

    /// Returns inflight outgoing packets and clears internal queues
    pub fn clean(&mut self) -> Vec<Request> {
        self.clean_with_notices()
            .into_iter()
            .map(|(request, _)| request)
            .collect()
    }

    pub const fn inflight(&self) -> u16 {
        self.inflight
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

        self.maybe_shrink_outgoing_tracking_capacity();
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
        self.maybe_shrink_outgoing_tracking_capacity();
    }

    pub(crate) fn reset_session_state(&mut self) {
        self.fail_pending_notices();
        *self = Self::new_internal(self.max_inflight, self.ack_mode);
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

        self.last_outgoing = Instant::now();
        Ok(packet)
    }

    pub(crate) fn handle_outgoing_packet_with_notice(
        &mut self,
        request: Request,
        notice: Option<TrackedNoticeTx>,
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
                self.outgoing_pubrel_with_notice(pubrel, publish_notice)?
            }
            Request::Subscribe(subscribe) => {
                let request_notice = match notice {
                    Some(TrackedNoticeTx::Subscribe(notice)) => Some(notice),
                    Some(TrackedNoticeTx::Publish(_) | TrackedNoticeTx::Unsubscribe(_)) | None => {
                        None
                    }
                };
                (self.outgoing_subscribe(subscribe, request_notice)?, None)
            }
            Request::Unsubscribe(unsubscribe) => {
                let request_notice = match notice {
                    Some(TrackedNoticeTx::Unsubscribe(notice)) => Some(notice),
                    Some(TrackedNoticeTx::Publish(_) | TrackedNoticeTx::Subscribe(_)) | None => {
                        None
                    }
                };
                (
                    Some(self.outgoing_unsubscribe(unsubscribe, request_notice)?),
                    None,
                )
            }
            Request::PingReq => (self.outgoing_ping()?, None),
            Request::Disconnect(_) | Request::DisconnectWithTimeout(_, _) => {
                unreachable!("graceful disconnect requests are handled by the event loop")
            }
            Request::DisconnectNow(_) => (Some(self.outgoing_disconnect()), None),
            Request::PubAck(puback) => (Some(self.outgoing_puback(puback)?), None),
            Request::PubRec(pubrec) => (Some(self.outgoing_pubrec(pubrec)?), None),
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
        packet: Incoming,
    ) -> Result<IncomingPacketEffects, StateError> {
        let events_len_before = self.events.len();
        let forward_incoming = !self.is_duplicate_incoming_qos2_publish(&packet);
        let outgoing = match &packet {
            Incoming::PingResp => Ok(IncomingPacketEffects::outgoing(
                self.handle_incoming_pingresp(),
            )),
            Incoming::Publish(publish) => self.handle_incoming_publish(publish),
            Incoming::SubAck(suback) => self.handle_incoming_suback(suback),
            Incoming::UnsubAck(unsuback) => self.handle_incoming_unsuback(unsuback),
            Incoming::PubAck(puback) => self.handle_incoming_puback(puback),
            Incoming::PubRec(pubrec) => self.handle_incoming_pubrec(pubrec),
            Incoming::PubRel(pubrel) => self
                .handle_incoming_pubrel(pubrel)
                .map(IncomingPacketEffects::outgoing),
            Incoming::PubComp(pubcomp) => self.handle_incoming_pubcomp(pubcomp),
            Incoming::ConnAck(_) => Err(StateError::ProtocolViolation(
                ProtocolViolation::DuplicateConnAck,
            )),
            _ => {
                error!("Invalid incoming packet = {packet:?}");
                Err(StateError::ProtocolViolation(
                    ProtocolViolation::UnexpectedIncomingPacket(packet.packet_type()),
                ))
            }
        };

        let effects = outgoing?;
        // Preserve original event ordering (Incoming first, derived Outgoing next)
        // without cloning the incoming packet.
        if forward_incoming {
            self.events
                .insert(events_len_before, Event::Incoming(packet));
        }
        self.last_incoming = Instant::now();

        Ok(effects)
    }

    fn is_duplicate_incoming_qos2_publish(&self, packet: &Incoming) -> bool {
        matches!(
            packet,
            Incoming::Publish(publish)
                if publish.qos == QoS::ExactlyOnce
                    && self.incoming_pubrec.contains(usize::from(publish.pkid))
        )
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
        if expected != actual {
            error!(
                "Suback return code count mismatch for pkid {:?}: expected {:?}, got {:?}",
                suback.pkid, expected, actual
            );
            return Err(StateError::ProtocolViolation(
                ProtocolViolation::SubAckReturnCodeCountMismatch {
                    pkid: suback.pkid,
                    expected,
                    actual,
                },
            ));
        }

        let pending_subscribe = self
            .pending_subscribe
            .remove(&suback.pkid)
            .ok_or(StateError::InvalidState)?;
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
        let mut effects = IncomingPacketEffects::outgoing(None);
        if let Some(pending_unsubscribe) = self.pending_unsubscribe.remove(&unsuback.pkid) {
            self.mark_control_packet_id_complete(unsuback.pkid);
            self.release_outbound_pkid(unsuback.pkid);
            if let Some(notice) = pending_unsubscribe.notice {
                effects =
                    effects.with_notice(DeferredNotice::Unsubscribe(notice, unsuback.clone()));
            }
        } else {
            error!("Unsolicited unsuback packet: {:?}", unsuback.pkid);
            return Err(StateError::Unsolicited(unsuback.pkid));
        }

        Ok(effects)
    }

    /// Results in a publish notification in all the `QoS` cases. Replys with an ack
    /// in case of `QoS1` and Replys rec in case of `QoS` while also storing the message
    fn handle_incoming_publish(
        &mut self,
        publish: &Publish,
    ) -> Result<IncomingPacketEffects, StateError> {
        let qos = publish.qos;

        match qos {
            QoS::AtMostOnce => Ok(IncomingPacketEffects::outgoing(None)),
            QoS::AtLeastOnce => {
                let pkid = publish.pkid;
                self.incoming_puback.insert(usize::from(pkid));

                if self.ack_mode == AckMode::Automatic {
                    let puback = PubAck::new(pkid);
                    return Ok(IncomingPacketEffects::outgoing(Some(
                        self.outgoing_puback(puback)?,
                    )));
                }
                Ok(IncomingPacketEffects::outgoing(None))
            }
            QoS::ExactlyOnce => {
                let pkid = publish.pkid;
                self.incoming_pub.insert(usize::from(pkid));

                if self.ack_mode == AckMode::Automatic
                    || self.incoming_pubrec.contains(usize::from(pkid))
                {
                    let pubrec = PubRec::new(pkid);
                    return Ok(IncomingPacketEffects::outgoing(Some(
                        self.outgoing_pubrec_for_incoming_publish(pubrec)?,
                    )));
                }
                Ok(IncomingPacketEffects::outgoing(None))
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

        let notice = self
            .outgoing_pub_notice
            .get_mut(usize::from(puback.pkid))
            .expect("puback pkid within outgoing_pub_notice bounds")
            .take();

        self.inflight -= 1;
        let packet = self.replay_collision_publish(puback.pkid);
        if packet.is_none() {
            self.maybe_shrink_outgoing_tracking_capacity();
        }

        let mut effects = IncomingPacketEffects::outgoing(packet);
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

        let notice = self
            .outgoing_pub_notice
            .get_mut(usize::from(pubrec.pkid))
            .expect("pubrec pkid within outgoing_pub_notice bounds")
            .take();
        // NOTE: Inflight - 1 for qos2 in comp
        self.outgoing_rel.insert(usize::from(pubrec.pkid));
        *self
            .outgoing_rel_notice
            .get_mut(usize::from(pubrec.pkid))
            .expect("pubrec pkid within outgoing_rel_notice bounds") = notice;
        let release = PubRel { pkid: pubrec.pkid };
        let event = Event::Outgoing(Outgoing::PubRel(pubrec.pkid));
        self.events.push_back(event);

        Ok(IncomingPacketEffects::outgoing(Some(Packet::PubRel(
            release,
        ))))
    }

    fn handle_incoming_pubrel(&mut self, pubrel: &PubRel) -> Result<Option<Packet>, StateError> {
        if !self.incoming_pub.contains(usize::from(pubrel.pkid)) {
            warn!(
                "Untracked pubrel packet: {:?}. Sending pubcomp to complete QoS2 recovery",
                pubrel.pkid
            );
            let event = Event::Outgoing(Outgoing::PubComp(pubrel.pkid));
            let pubcomp = PubComp::new(pubrel.pkid);
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
        let event = Event::Outgoing(Outgoing::PubComp(pubrel.pkid));
        let pubcomp = PubComp { pkid: pubrel.pkid };
        self.events.push_back(event);

        Ok(Some(Packet::PubComp(pubcomp)))
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
        self.mark_outgoing_packet_id_complete(pubcomp.pkid);
        self.release_outbound_pkid(pubcomp.pkid);
        let notice = self
            .outgoing_rel_notice
            .get_mut(usize::from(pubcomp.pkid))
            .expect("pubcomp pkid within outgoing_rel_notice bounds")
            .take();
        self.inflight -= 1;
        let packet = self.replay_collision_publish(pubcomp.pkid);
        if packet.is_none() {
            self.maybe_shrink_outgoing_tracking_capacity();
        }

        let mut effects = IncomingPacketEffects::outgoing(packet);
        if let Some(tx) = notice {
            effects = effects.with_notice(DeferredNotice::Publish(
                tx,
                PublishResult::Qos2Completed(pubcomp.clone()),
            ));
        }

        Ok(effects)
    }

    const fn handle_incoming_pingresp(&mut self) -> Option<Packet> {
        self.ping.finish_waiting_for_resp();

        None
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
        if publish.qos != QoS::AtMostOnce {
            if publish.pkid == 0 {
                publish.pkid = self.next_pkid().ok_or(StateError::InvalidState)?;
            }

            let pkid = publish.pkid;
            self.validate_outgoing_pkid_bound(pkid)?;
            self.ensure_outgoing_tracking_capacity(usize::from(pkid) + 1);
            if self
                .outgoing_pub
                .get(usize::from(publish.pkid))
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

            if self.outgoing_rel.contains(usize::from(pkid))
                || self.outgoing_rel_replay.contains(usize::from(pkid))
                || self.pending_subscribe.contains_key(&pkid)
                || self.pending_unsubscribe.contains_key(&pkid)
            {
                return Err(StateError::InvalidState);
            }

            // if there is an existing publish at this pkid, this implies that broker hasn't acked this
            // packet yet. This error is possible only when broker isn't acking sequentially
            self.reserve_outbound_pkid(pkid)?;
            self.outgoing_pub[usize::from(pkid)] = Some(publish.clone());
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

        let event = Event::Outgoing(Outgoing::Publish(publish.pkid));
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

        Ok((Some(Packet::PubRel(pubrel)), None))
    }

    fn outgoing_puback(&mut self, puback: PubAck) -> Result<Packet, StateError> {
        if !self.incoming_puback.contains(usize::from(puback.pkid)) {
            error!("Unsolicited puback request: {:?}", puback.pkid);
            return Err(StateError::Unsolicited(puback.pkid));
        }

        self.incoming_puback.set(usize::from(puback.pkid), false);
        let event = Event::Outgoing(Outgoing::PubAck(puback.pkid));
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
                let event = Event::Outgoing(Outgoing::PubRec(pubrec.pkid));
                self.events.push_back(event);
                return Ok(Packet::PubRec(pubrec));
            }

            error!("Duplicate pubrec request: {:?}", pubrec.pkid);
            return Err(StateError::Unsolicited(pubrec.pkid));
        }

        self.incoming_pubrec.insert(usize::from(pubrec.pkid));
        let event = Event::Outgoing(Outgoing::PubRec(pubrec.pkid));
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
            "Pingreq,
            last incoming packet before {} millisecs,
            last outgoing request before {} millisecs",
            elapsed_in.as_millis(),
            elapsed_out.as_millis()
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

        if subscription.pkid == 0 {
            subscription.pkid = self.next_control_pkid()?;
        } else {
            self.reserve_control_pkid(subscription.pkid)?;
        }

        debug!(
            "Subscribe. Topics = {:?}, Pkid = {:?}",
            subscription.filters, subscription.pkid
        );

        let event = Event::Outgoing(Outgoing::Subscribe(subscription.pkid));
        self.events.push_back(event);
        self.pending_subscribe.insert(
            subscription.pkid,
            PendingSubscribe {
                subscribe: subscription.clone(),
                notice,
            },
        );

        Ok(Some(Packet::Subscribe(subscription)))
    }

    fn outgoing_unsubscribe(
        &mut self,
        mut unsub: Unsubscribe,
        notice: Option<UnsubscribeNoticeTx>,
    ) -> Result<Packet, StateError> {
        if unsub.pkid == 0 {
            unsub.pkid = self.next_control_pkid()?;
        } else {
            self.reserve_control_pkid(unsub.pkid)?;
        }

        debug!(
            "Unsubscribe. Topics = {:?}, Pkid = {:?}",
            unsub.topics, unsub.pkid
        );

        let event = Event::Outgoing(Outgoing::Unsubscribe(unsub.pkid));
        self.events.push_back(event);
        self.pending_unsubscribe.insert(
            unsub.pkid,
            PendingUnsubscribe {
                unsubscribe: unsub.clone(),
                notice,
            },
        );

        Ok(Packet::Unsubscribe(unsub))
    }

    fn outgoing_disconnect(&mut self) -> Packet {
        debug!("Disconnect");

        let event = Event::Outgoing(Outgoing::Disconnect);
        self.events.push_back(event);

        Packet::Disconnect
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
        self.ensure_outgoing_tracking_capacity(usize::from(pubrel.pkid) + 1);
        self.outgoing_rel.insert(pkid_index);
        self.outgoing_rel_notice[pkid_index] = notice;
        self.inflight += 1;
        Ok(pubrel)
    }

    fn replay_collision_publish(&mut self, pkid: u16) -> Option<Packet> {
        self.check_collision(pkid).map(|(publish, notice)| {
            let publish_pkid = publish.pkid;
            self.ensure_outgoing_tracking_capacity(usize::from(publish_pkid) + 1);
            self.reserve_outbound_pkid(publish_pkid)
                .expect("collision replay packet identifier should have been released");
            self.outgoing_pub[usize::from(publish_pkid)] = Some(publish.clone());
            self.outgoing_pub_notice[usize::from(publish_pkid)] = notice;
            self.outgoing_pub_flush_attempted
                .set(usize::from(publish_pkid), false);
            self.inflight += 1;

            let event = Event::Outgoing(Outgoing::Publish(publish_pkid));
            self.events.push_back(event);
            self.ping.reset_collision_count();

            Packet::Publish(publish)
        })
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
            && pkid <= self.max_inflight
            && pkid != self.last_puback
            && (self.last_puback >= self.max_inflight || pkid > self.last_puback)
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
        if self.max_inflight == 0 {
            return 0;
        }

        if pkid >= self.max_inflight {
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
        for _ in 0..usize::from(self.max_inflight) {
            if !self.packet_identifier_in_use(pkid) {
                return Some(pkid);
            }
            pkid = self.next_publish_pkid_after(pkid);
        }

        None
    }

    fn next_pkid(&mut self) -> Option<u16> {
        let pkid = self.next_publish_pkid()?;
        if pkid == self.max_inflight {
            self.last_pkid = 0;
        } else {
            self.last_pkid = pkid;
        }

        Some(pkid)
    }

    fn next_control_pkid(&mut self) -> Result<u16, StateError> {
        let pkid = self
            .next_free_outbound_pkid_after(self.last_pkid)
            .ok_or(StateError::InvalidState)?;
        self.reserve_outbound_pkid(pkid)?;
        self.last_pkid = pkid;
        Ok(pkid)
    }

    fn reserve_control_pkid(&mut self, pkid: u16) -> Result<(), StateError> {
        self.reserve_outbound_pkid(pkid)
    }

    pub(crate) fn persisted_session<'a>(
        &self,
        options: &crate::MqttOptions,
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
            clean_session: options.clean_session(),
            max_inflight: options.inflight(),
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
        options: &crate::MqttOptions,
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
            if let PersistedRequest::PubRel(pubrel) = request {
                self.ensure_outgoing_tracking_capacity(usize::from(pubrel.pkid) + 1);
                self.outgoing_rel_replay.insert(usize::from(pubrel.pkid));
            }
        }
        self.rebuild_outbound_pkid_index();

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
        validate_last_puback(session.last_puback, self.max_inflight)?;

        for incoming in &session.incoming_qos2 {
            validate_pkid(incoming.pkid)?;
        }

        session.replay.iter().try_for_each(|request| {
            validate_persisted_replay_request_pkid(request, self.max_inflight)
        })?;

        Ok(())
    }
}

fn validate_persisted_session_signature(
    options: &crate::MqttOptions,
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

    if session.clean_session != options.clean_session() {
        return Err(SessionRestoreError::CleanSessionMismatch {
            persisted: session.clean_session,
            configured: options.clean_session(),
        });
    }

    if session.max_inflight != options.inflight() {
        return Err(SessionRestoreError::MaxInflightMismatch {
            persisted: session.max_inflight,
            configured: options.inflight(),
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

fn validate_outgoing_pkid(pkid: u16, max_inflight: u16) -> Result<(), SessionRestoreError> {
    validate_pkid(pkid)?;

    if pkid > max_inflight {
        return Err(SessionRestoreError::OutgoingPacketIdentifierOutOfRange { pkid, max_inflight });
    }

    Ok(())
}

const fn validate_last_puback(
    last_puback: u16,
    max_inflight: u16,
) -> Result<(), SessionRestoreError> {
    if last_puback > max_inflight {
        return Err(SessionRestoreError::LastPubAckOutOfRange {
            last_puback,
            max_inflight,
        });
    }

    Ok(())
}

fn validate_persisted_replay_request_pkid(
    request: &PersistedRequest,
    max_inflight: u16,
) -> Result<(), SessionRestoreError> {
    match request {
        PersistedRequest::Publish(publish) => validate_outgoing_pkid(publish.pkid, max_inflight),
        PersistedRequest::PubRel(pubrel) => validate_outgoing_pkid(pubrel.pkid, max_inflight),
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
    })
}

fn persisted_subscribe(subscribe: &Subscribe) -> PersistedSubscribe {
    PersistedSubscribe {
        pkid: subscribe.pkid,
        filters: subscribe.filters.iter().map(persisted_filter).collect(),
    }
}

fn persisted_filter(filter: &crate::mqttbytes::v4::SubscribeFilter) -> PersistedFilter {
    PersistedFilter {
        path: filter.path.clone(),
        qos: persisted_qos(filter.qos),
    }
}

fn persisted_unsubscribe(unsubscribe: &Unsubscribe) -> PersistedUnsubscribe {
    PersistedUnsubscribe {
        pkid: unsubscribe.pkid,
        topics: unsubscribe.topics.clone(),
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
            }))
        }
        PersistedRequest::PubRel(pubrel) => {
            validate_pkid(pubrel.pkid)?;
            Ok(Request::PubRel(PubRel::new(pubrel.pkid)))
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
            }))
        }
        PersistedRequest::Unsubscribe(unsubscribe) => {
            validate_pkid(unsubscribe.pkid)?;
            Ok(Request::Unsubscribe(Unsubscribe {
                pkid: unsubscribe.pkid,
                topics: unsubscribe.topics.clone(),
            }))
        }
    }
}

fn filter_from_persisted(filter: &PersistedFilter) -> crate::mqttbytes::v4::SubscribeFilter {
    crate::mqttbytes::v4::SubscribeFilter {
        path: filter.path.clone(),
        qos: qos_from_persisted(filter.qos),
    }
}

impl Clone for MqttState {
    fn clone(&self) -> Self {
        let tracking_len = self.outgoing_pub_notice.len();
        Self {
            ping: self.ping,
            last_incoming: self.last_incoming,
            last_outgoing: self.last_outgoing,
            last_pkid: self.last_pkid,
            last_puback: self.last_puback,
            inflight: self.inflight,
            max_inflight: self.max_inflight,
            outgoing_pub: self.outgoing_pub.clone(),
            outgoing_pub_notice: Self::new_notice_slots_with_len(tracking_len),
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
        }
    }
}

#[cfg(test)]
mod test {
    use super::{MqttState, PendingSubscribe, PendingUnsubscribe, ProtocolViolation, StateError};
    use crate::mqttbytes::v4::*;
    use crate::mqttbytes::*;
    use crate::notice::{
        PublishNoticeTx, PublishResult, SubscribeNoticeError, SubscribeNoticeTx,
        UnsubscribeNoticeError, UnsubscribeNoticeTx,
    };
    use crate::session::{
        PersistedAckMode, PersistedPubRel, PersistedRequest, PersistedSession, SessionRestoreError,
    };
    use crate::{
        AckMode, Event, Incoming, MqttOptions, NoticeFailureReason, Outgoing, Request, SessionMode,
    };
    use bytes::Bytes;

    fn build_outgoing_publish(qos: QoS) -> Publish {
        let topic = "hello/world".to_owned();
        let payload = vec![1, 2, 3];

        let mut publish = Publish::new(topic, QoS::AtLeastOnce, payload);
        publish.qos = qos;
        publish
    }

    fn build_incoming_publish(qos: QoS, pkid: u16) -> Publish {
        let topic = "hello/world".to_owned();
        let payload = vec![1, 2, 3];

        let mut publish = Publish::new(topic, QoS::AtLeastOnce, payload);
        publish.pkid = pkid;
        publish.qos = qos;
        publish
    }

    fn build_mqttstate() -> MqttState {
        MqttState::builder(100).build()
    }

    fn persistent_options() -> MqttOptions {
        MqttOptions::builder("client", "localhost")
            .session_mode(SessionMode::Persistent)
            .build()
    }

    fn queue_publish_with_notice(mqtt: &mut MqttState, publish: Publish) -> crate::PublishNotice {
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
    fn new_state_uses_warm_tracking_floor() {
        let mqtt = MqttState::builder(100).build();

        assert_eq!(mqtt.outgoing_pub.len(), 33);
        assert_eq!(mqtt.outgoing_pub_notice.len(), 33);
        assert_eq!(mqtt.outgoing_rel_notice.len(), 33);
        assert_eq!(mqtt.outgoing_pub_ack.len(), 33);
        assert_eq!(mqtt.outgoing_rel.len(), 33);
    }

    #[test]
    fn new_state_uses_full_tracking_len_when_max_inflight_is_below_warm_floor() {
        let mqtt = MqttState::builder(10).build();

        assert_eq!(mqtt.outgoing_pub.len(), 11);
        assert_eq!(mqtt.outgoing_pub_notice.len(), 11);
        assert_eq!(mqtt.outgoing_rel_notice.len(), 11);
        assert_eq!(mqtt.outgoing_pub_ack.len(), 11);
        assert_eq!(mqtt.outgoing_rel.len(), 11);
    }

    #[test]
    fn clean_pending_capacity_counts_publish_rel_and_tracked_requests() {
        let mut mqtt = MqttState::builder(10).build();
        mqtt.outgoing_pub[1] = Some(build_outgoing_publish(QoS::AtLeastOnce));
        mqtt.outgoing_pub[2] = Some(build_outgoing_publish(QoS::ExactlyOnce));
        mqtt.outgoing_rel.insert(3);
        mqtt.outgoing_rel.insert(4);

        let (sub_notice, _) = SubscribeNoticeTx::new();
        mqtt.pending_subscribe.insert(
            5,
            PendingSubscribe {
                subscribe: Subscribe::new("a/b", QoS::AtMostOnce),
                notice: Some(sub_notice),
            },
        );

        let (unsub_notice, _) = UnsubscribeNoticeTx::new();
        mqtt.pending_unsubscribe.insert(
            6,
            PendingUnsubscribe {
                unsubscribe: Unsubscribe::new("a/b"),
                notice: Some(unsub_notice),
            },
        );

        assert_eq!(mqtt.clean_pending_capacity(), 6);
    }

    #[test]
    fn tracked_request_len_helpers_report_counts() {
        let mut mqtt = MqttState::builder(10).build();
        let (sub_notice, _) = SubscribeNoticeTx::new();
        mqtt.pending_subscribe.insert(
            5,
            PendingSubscribe {
                subscribe: Subscribe::new("a/b", QoS::AtMostOnce),
                notice: Some(sub_notice),
            },
        );
        let (unsub_notice, _) = UnsubscribeNoticeTx::new();
        mqtt.pending_unsubscribe.insert(
            6,
            PendingUnsubscribe {
                unsubscribe: Unsubscribe::new("a/b"),
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
                subscribe: Subscribe::new("tracked", QoS::AtMostOnce),
                notice: Some(sub_notice),
            },
        );
        mqtt.pending_subscribe.insert(
            6,
            PendingSubscribe {
                subscribe: Subscribe::new("untracked", QoS::AtMostOnce),
                notice: None,
            },
        );
        mqtt.pending_unsubscribe.insert(
            7,
            PendingUnsubscribe {
                unsubscribe: Unsubscribe::new("untracked"),
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
        let (sub_notice_tx, sub_notice) = SubscribeNoticeTx::new();
        mqtt.pending_subscribe.insert(
            5,
            PendingSubscribe {
                subscribe: Subscribe::new("a/b", QoS::AtMostOnce),
                notice: Some(sub_notice_tx),
            },
        );
        let (unsub_notice_tx, unsub_notice) = UnsubscribeNoticeTx::new();
        mqtt.pending_unsubscribe.insert(
            6,
            PendingUnsubscribe {
                unsubscribe: Unsubscribe::new("a/b"),
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
                subscribe: Subscribe::new("a/b", QoS::AtMostOnce),
                notice: None,
            },
        );
        mqtt.pending_unsubscribe.insert(
            6,
            PendingUnsubscribe {
                unsubscribe: Unsubscribe::new("a/b"),
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

        let puback = PubAck::new(1);
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
    fn clean_marks_unacked_qos1_publish_dup_for_reconnect_replay() {
        let mut mqtt = build_mqttstate();
        let publish = build_outgoing_publish(QoS::AtLeastOnce);
        queue_publish_with_notice(&mut mqtt, publish);
        mqtt.mark_outgoing_publishes_flush_attempted();

        let pending = mqtt.clean_with_notices();

        let replay = pending
            .into_iter()
            .find_map(|(request, _)| match request {
                Request::Publish(publish) => Some(publish),
                _ => None,
            })
            .expect("expected pending publish replay");

        assert!(replay.dup);
        assert_eq!(replay.qos, QoS::AtLeastOnce);
    }

    #[test]
    fn clean_does_not_mark_unflushed_qos1_publish_dup_for_reconnect_replay() {
        let mut mqtt = build_mqttstate();
        let publish = build_outgoing_publish(QoS::AtLeastOnce);
        queue_publish_with_notice(&mut mqtt, publish);

        let pending = mqtt.clean_with_notices();

        let replay = pending
            .into_iter()
            .find_map(|(request, _)| match request {
                Request::Publish(publish) => Some(publish),
                _ => None,
            })
            .expect("expected pending publish replay");

        assert!(!replay.dup);
        assert_eq!(replay.qos, QoS::AtLeastOnce);
    }

    #[test]
    fn clean_replays_untracked_subscribe_and_unsubscribe_without_notices() {
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_subscribe(Subscribe::new("a/b", QoS::AtMostOnce), None)
            .unwrap();
        mqtt.outgoing_unsubscribe(Unsubscribe::new("c/d"), None)
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
                    && unsubscribe.topics.len() == 1
                    && unsubscribe.topics[0] == "c/d"
        )));
    }

    #[test]
    fn clone_preserves_pending_control_protocol_state_without_notices() {
        let mut mqtt = build_mqttstate();
        let (sub_tx, _) = SubscribeNoticeTx::new();
        let (unsub_tx, _) = UnsubscribeNoticeTx::new();
        mqtt.outgoing_subscribe(Subscribe::new("a/b", QoS::AtMostOnce), Some(sub_tx))
            .unwrap();
        mqtt.outgoing_unsubscribe(Unsubscribe::new("c/d"), Some(unsub_tx))
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

        let suback = SubAck::new(1, vec![SubscribeReasonCode::Success(QoS::AtMostOnce)]);
        assert!(
            cloned
                .handle_incoming_packet(Incoming::SubAck(suback))
                .unwrap()
                .is_none()
        );
        let unsuback = UnsubAck::new(2);
        assert!(
            cloned
                .handle_incoming_packet(Incoming::UnsubAck(unsuback))
                .unwrap()
                .is_none()
        );

        assert!(cloned.pending_control_requests_is_empty());
    }

    #[test]
    fn tracked_suback_returns_ack_and_preserves_incoming_event() {
        let mut mqtt = build_mqttstate();
        let (tx, notice) = SubscribeNoticeTx::new();
        mqtt.outgoing_subscribe(Subscribe::new("a/b", QoS::AtMostOnce), Some(tx))
            .unwrap();
        mqtt.events.clear();

        let suback = SubAck::new(1, vec![SubscribeReasonCode::Failure]);
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
    fn untracked_suback_returns_ack_and_preserves_incoming_event() {
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_subscribe(Subscribe::new("a/b", QoS::AtMostOnce), None)
            .unwrap();
        mqtt.events.clear();

        let suback = SubAck::new(1, vec![SubscribeReasonCode::Success(QoS::AtMostOnce)]);
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
    fn tracked_unsuback_returns_ack_and_preserves_incoming_event() {
        let mut mqtt = build_mqttstate();
        let (tx, notice) = UnsubscribeNoticeTx::new();
        mqtt.outgoing_unsubscribe(Unsubscribe::new("a/b"), Some(tx))
            .unwrap();
        mqtt.events.clear();

        let unsuback = UnsubAck::new(1);
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
    fn untracked_unsuback_returns_ack_and_preserves_incoming_event() {
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_unsubscribe(Unsubscribe::new("a/b"), None)
            .unwrap();
        mqtt.events.clear();

        let unsuback = UnsubAck::new(1);
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
        let pkid = 34;

        mqtt.pending_subscribe.insert(
            pkid,
            PendingSubscribe {
                subscribe: Subscribe::new("a/b", QoS::AtMostOnce),
                notice: None,
            },
        );
        mqtt.pending_unsubscribe.insert(
            pkid + 1,
            PendingUnsubscribe {
                unsubscribe: Unsubscribe::new("c/d"),
                notice: None,
            },
        );

        assert!(mqtt.outgoing_pub_ack.len() <= usize::from(pkid));
        mqtt.handle_incoming_suback(&SubAck::new(
            pkid,
            vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
        ))
        .unwrap();
        mqtt.handle_incoming_unsuback(&UnsubAck::new(pkid + 1))
            .unwrap();

        assert!(mqtt.outbound_requests_drained());
        assert!(mqtt.outgoing_pub_ack.ones().next().is_none());
        assert_eq!(mqtt.last_puback, 0);
    }

    #[test]
    fn incoming_suback_with_untracked_pkid_is_unsolicited() {
        let mut mqtt = build_mqttstate();

        let suback = SubAck::new(5, vec![SubscribeReasonCode::Failure]);
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

        let mut subscribe = Subscribe::new("a/b", QoS::AtMostOnce);
        subscribe.add("c/d".to_owned(), QoS::AtLeastOnce);
        mqtt.outgoing_subscribe(subscribe, None).unwrap();
        mqtt.events.clear();

        let suback = SubAck::new(1, vec![SubscribeReasonCode::Success(QoS::AtMostOnce)]);
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

        mqtt.outgoing_subscribe(Subscribe::new("a/b", QoS::AtMostOnce), None)
            .unwrap();
        mqtt.events.clear();

        let suback = SubAck::new(
            1,
            vec![
                SubscribeReasonCode::Success(QoS::AtMostOnce),
                SubscribeReasonCode::Success(QoS::AtLeastOnce),
            ],
        );
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

        let unsuback = UnsubAck::new(7);
        let got = mqtt
            .handle_incoming_packet(Incoming::UnsubAck(unsuback))
            .unwrap_err();

        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 7),
            e => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn incoming_pubrec_with_untracked_pkid_is_unsolicited() {
        let mut mqtt = build_mqttstate();

        let got = mqtt.handle_incoming_pubrec(&PubRec::new(5)).unwrap_err();

        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 5),
            e => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn incoming_pubrel_with_untracked_pkid_sends_pubcomp() {
        let mut mqtt = build_mqttstate();

        let packet = mqtt
            .handle_incoming_pubrel(&PubRel::new(7))
            .unwrap()
            .unwrap();

        match packet {
            Packet::PubComp(pubcomp) => assert_eq!(pubcomp.pkid, 7),
            packet => panic!("Invalid network request: {packet:?}"),
        }
    }

    #[test]
    fn incoming_pubcomp_with_untracked_pkid_is_unsolicited() {
        let mut mqtt = build_mqttstate();

        let got = mqtt.handle_incoming_pubcomp(&PubComp::new(11)).unwrap_err();

        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 11),
            e => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn outgoing_publish_grows_tracking_capacity_on_demand() {
        let mut mqtt = build_mqttstate();
        mqtt.last_pkid = 32;

        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();

        assert_eq!(mqtt.outgoing_pub.len(), 34);
        assert_eq!(mqtt.outgoing_pub_notice.len(), 34);
        assert_eq!(mqtt.outgoing_rel_notice.len(), 34);
        assert_eq!(mqtt.outgoing_pub_ack.len(), 34);
        assert_eq!(mqtt.outgoing_rel.len(), 34);
        assert!(mqtt.outgoing_pub[33].is_some());
    }

    #[test]
    fn incoming_puback_shrinks_tracking_when_state_becomes_empty() {
        let mut mqtt = build_mqttstate();
        mqtt.last_pkid = 32;
        mqtt.last_puback = 32;

        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        assert_eq!(mqtt.outgoing_pub.len(), 34);

        mqtt.handle_incoming_puback(&PubAck::new(33)).unwrap();

        assert_eq!(mqtt.outgoing_pub.len(), 33);
        assert_eq!(mqtt.outgoing_pub_notice.len(), 33);
        assert_eq!(mqtt.outgoing_rel_notice.len(), 33);
        assert_eq!(mqtt.outgoing_pub_ack.len(), 33);
        assert_eq!(mqtt.outgoing_rel.len(), 33);
        assert_eq!(mqtt.last_pkid, 0);
        assert_eq!(mqtt.last_puback, 0);
    }

    #[test]
    fn incoming_puback_does_not_shrink_tracking_when_state_is_non_empty() {
        let mut mqtt = build_mqttstate();
        mqtt.last_pkid = 32;
        mqtt.last_puback = 32;

        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        assert_eq!(mqtt.outgoing_pub.len(), 35);

        mqtt.handle_incoming_puback(&PubAck::new(33)).unwrap();

        assert_eq!(mqtt.outgoing_pub.len(), 35);
        assert_eq!(mqtt.inflight, 1);
    }

    #[test]
    fn clean_preserves_packet_id_frontier_when_pending_state_is_exported() {
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        assert_eq!(mqtt.last_pkid, 2);

        let pending = mqtt.clean();
        assert_eq!(pending.len(), 2);
        assert_eq!(mqtt.last_pkid, 2);
        assert_eq!(mqtt.last_puback, 0);

        for request in pending {
            let packet = mqtt.handle_outgoing_packet(request).unwrap().unwrap();
            match packet {
                Packet::Publish(publish) => assert!(matches!(publish.pkid, 1 | 2)),
                packet => panic!("Unexpected replay packet: {packet:?}"),
            }
        }

        let packet = mqtt
            .handle_outgoing_packet(Request::Publish(build_outgoing_publish(QoS::AtLeastOnce)))
            .unwrap()
            .unwrap();
        match packet {
            Packet::Publish(publish) => assert_eq!(publish.pkid, 3),
            packet => panic!("Unexpected fresh packet after replay: {packet:?}"),
        }

        assert!(mqtt.collision.is_none());
    }

    #[test]
    fn clone_preserves_current_tracking_lengths_after_shrink() {
        let mut mqtt = build_mqttstate();
        mqtt.last_pkid = 32;
        mqtt.last_puback = 32;

        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        mqtt.handle_incoming_puback(&PubAck::new(33)).unwrap();

        let cloned = mqtt.clone();
        assert_eq!(cloned.outgoing_pub.len(), 33);
        assert_eq!(cloned.outgoing_pub_notice.len(), 33);
        assert_eq!(cloned.outgoing_rel_notice.len(), 33);
        assert_eq!(cloned.outgoing_pub_ack.len(), 33);
        assert_eq!(cloned.outgoing_rel.len(), 33);
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
            let (notice, _) = SubscribeNoticeTx::new();
            mqtt.pending_subscribe.insert(
                pkid,
                PendingSubscribe {
                    subscribe: Subscribe::new("a/b", QoS::AtMostOnce),
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

        let (notice, _) = SubscribeNoticeTx::new();
        mqtt.pending_subscribe.insert(
            3,
            PendingSubscribe {
                subscribe: Subscribe::new("a/b", QoS::AtMostOnce),
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
                unsubscribe: Unsubscribe::new("a/b"),
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
        mqtt.outgoing_subscribe(Subscribe::new("a/b", QoS::AtMostOnce), None)
            .unwrap();

        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.pkid = 1;
        assert!(matches!(
            mqtt.outgoing_publish(publish),
            Err(StateError::InvalidState)
        ));

        mqtt.outgoing_unsubscribe(Unsubscribe::new("c/d"), None)
            .unwrap();

        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.pkid = 2;
        assert!(matches!(
            mqtt.outgoing_publish(publish),
            Err(StateError::InvalidState)
        ));
    }

    #[test]
    fn replayed_subscribe_keeps_original_packet_identifier_until_suback() {
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_subscribe(Subscribe::new("a/b", QoS::AtMostOnce), None)
            .unwrap();

        let mut pending = mqtt.clean();
        let replay = pending.remove(0);
        match &replay {
            Request::Subscribe(subscribe) => assert_eq!(subscribe.pkid, 1),
            request => panic!("unexpected replay request: {request:?}"),
        }

        mqtt.last_pkid = 42;
        let packet = mqtt.handle_outgoing_packet(replay).unwrap().unwrap();
        match packet {
            Packet::Subscribe(subscribe) => assert_eq!(subscribe.pkid, 1),
            packet => panic!("unexpected packet: {packet:?}"),
        }

        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.pkid = 1;
        assert!(matches!(
            mqtt.outgoing_publish(publish.clone()),
            Err(StateError::InvalidState)
        ));

        mqtt.handle_incoming_suback(&SubAck::new(
            1,
            vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
        ))
        .unwrap();
        assert!(mqtt.outgoing_publish(publish).is_ok());
    }

    #[test]
    fn replayed_unsubscribe_keeps_original_packet_identifier_until_unsuback() {
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_unsubscribe(Unsubscribe::new("a/b"), None)
            .unwrap();

        let mut pending = mqtt.clean();
        let replay = pending.remove(0);
        match &replay {
            Request::Unsubscribe(unsubscribe) => assert_eq!(unsubscribe.pkid, 1),
            request => panic!("unexpected replay request: {request:?}"),
        }

        mqtt.last_pkid = 42;
        let packet = mqtt.handle_outgoing_packet(replay).unwrap().unwrap();
        match packet {
            Packet::Unsubscribe(unsubscribe) => assert_eq!(unsubscribe.pkid, 1),
            packet => panic!("unexpected packet: {packet:?}"),
        }

        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.pkid = 1;
        assert!(matches!(
            mqtt.outgoing_publish(publish.clone()),
            Err(StateError::InvalidState)
        ));

        mqtt.handle_incoming_unsuback(&UnsubAck::new(1)).unwrap();
        assert!(mqtt.outgoing_publish(publish).is_ok());
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
    fn incoming_publish_should_be_added_to_queue_correctly() {
        let mut mqtt = build_mqttstate();

        // QoS0, 1, 2 Publishes
        let publish1 = build_incoming_publish(QoS::AtMostOnce, 1);
        let publish2 = build_incoming_publish(QoS::AtLeastOnce, 2);
        let publish3 = build_incoming_publish(QoS::ExactlyOnce, 3);

        drop(mqtt.handle_incoming_publish(&publish1).unwrap());
        drop(mqtt.handle_incoming_publish(&publish2).unwrap());
        drop(mqtt.handle_incoming_publish(&publish3).unwrap());

        // qos1 and qos2 publishes are tracked until their acknowledgement flow completes
        assert!(!mqtt.incoming_puback.contains(1));
        assert!(!mqtt.incoming_puback.contains(2));
        assert!(mqtt.incoming_pub.contains(3));
    }

    #[test]
    fn incoming_publish_should_be_acked() {
        let mut mqtt = build_mqttstate();

        // QoS0, 1, 2 Publishes
        let publish1 = build_incoming_publish(QoS::AtMostOnce, 1);
        let publish2 = build_incoming_publish(QoS::AtLeastOnce, 2);
        let publish3 = build_incoming_publish(QoS::ExactlyOnce, 3);

        assert!(mqtt.handle_incoming_publish(&publish1).unwrap().is_none());

        let packet = mqtt.handle_incoming_publish(&publish2).unwrap();
        if let Some(Packet::PubAck(puback)) = packet.outgoing {
            let pkid = puback.pkid;
            assert_eq!(pkid, 2);
        } else {
            panic!("missing puback");
        }

        let packet = mqtt.handle_incoming_publish(&publish3).unwrap();
        if let Some(Packet::PubRec(pubrec)) = packet.outgoing {
            let pkid = pubrec.pkid;
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
        let publish1 = build_incoming_publish(QoS::AtMostOnce, 1);
        let publish2 = build_incoming_publish(QoS::AtLeastOnce, 2);
        let publish3 = build_incoming_publish(QoS::ExactlyOnce, 3);

        assert!(mqtt.handle_incoming_publish(&publish1).unwrap().is_none());
        assert!(mqtt.handle_incoming_publish(&publish2).unwrap().is_none());
        assert!(mqtt.handle_incoming_publish(&publish3).unwrap().is_none());

        assert!(mqtt.incoming_puback.contains(2));
        assert!(mqtt.incoming_pub.contains(3));

        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn manual_puback_must_match_received_qos1_publish() {
        let mut mqtt = build_mqttstate();
        mqtt.ack_mode = AckMode::Manual;
        let publish = build_incoming_publish(QoS::AtLeastOnce, 10);

        assert!(mqtt.handle_incoming_publish(&publish).unwrap().is_none());

        let got = mqtt
            .handle_outgoing_packet(Request::PubAck(PubAck::new(11)))
            .unwrap_err();
        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 11),
            e => panic!("Unexpected error: {e}"),
        }

        let packet = mqtt
            .handle_outgoing_packet(Request::PubAck(PubAck::new(10)))
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
        let publish = build_incoming_publish(QoS::ExactlyOnce, 10);

        assert!(mqtt.handle_incoming_publish(&publish).unwrap().is_none());

        let got = mqtt
            .handle_outgoing_packet(Request::PubRec(PubRec::new(11)))
            .unwrap_err();
        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 11),
            e => panic!("Unexpected error: {e}"),
        }

        let packet = mqtt
            .handle_outgoing_packet(Request::PubRec(PubRec::new(10)))
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
        let qos1 = build_incoming_publish(QoS::AtLeastOnce, 10);
        let qos2 = build_incoming_publish(QoS::ExactlyOnce, 11);

        mqtt.handle_incoming_publish(&qos1).unwrap();
        mqtt.handle_incoming_publish(&qos2).unwrap();

        mqtt.handle_outgoing_packet(Request::PubAck(PubAck::new(10)))
            .unwrap();
        assert!(matches!(
            mqtt.handle_outgoing_packet(Request::PubAck(PubAck::new(10))),
            Err(StateError::Unsolicited(10))
        ));

        mqtt.handle_outgoing_packet(Request::PubRec(PubRec::new(11)))
            .unwrap();
        assert!(matches!(
            mqtt.handle_outgoing_packet(Request::PubRec(PubRec::new(11))),
            Err(StateError::Unsolicited(11))
        ));
    }

    #[test]
    fn manual_ack_rejects_qos_mismatch() {
        let mut mqtt = build_mqttstate();
        mqtt.ack_mode = AckMode::Manual;
        let qos1 = build_incoming_publish(QoS::AtLeastOnce, 10);
        let qos2 = build_incoming_publish(QoS::ExactlyOnce, 11);

        mqtt.handle_incoming_publish(&qos1).unwrap();
        mqtt.handle_incoming_publish(&qos2).unwrap();

        assert!(matches!(
            mqtt.handle_outgoing_packet(Request::PubRec(PubRec::new(10))),
            Err(StateError::Unsolicited(10))
        ));
        assert!(matches!(
            mqtt.handle_outgoing_packet(Request::PubAck(PubAck::new(11))),
            Err(StateError::Unsolicited(11))
        ));
    }

    #[test]
    fn outgoing_pubrel_without_original_publish_is_unsolicited() {
        let mut mqtt = build_mqttstate();

        let got = mqtt
            .handle_outgoing_packet(Request::PubRel(PubRel::new(7)))
            .unwrap_err();

        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 7),
            e => panic!("Unexpected error: {e}"),
        }
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
        mqtt.handle_outgoing_packet(Request::PubRec(PubRec::new(43)))
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
        mqtt.handle_incoming_packet(Incoming::PubRel(PubRel::new(43)))
            .unwrap();
        mqtt.events.clear();

        mqtt.handle_incoming_packet(Incoming::Publish(publish.clone()))
            .unwrap();

        assert_eq!(mqtt.events.len(), 2);
        assert_eq!(mqtt.events[0], Event::Incoming(Incoming::Publish(publish)));
        assert_eq!(mqtt.events[1], Event::Outgoing(Outgoing::PubRec(43)));
    }

    #[test]
    fn handle_incoming_packet_preserves_publish_retain_flag() {
        let mut mqtt = build_mqttstate();
        let mut publish = build_incoming_publish(QoS::AtMostOnce, 0);
        publish.retain = true;

        mqtt.handle_incoming_packet(Incoming::Publish(publish.clone()))
            .unwrap();

        assert_eq!(mqtt.events.len(), 1);
        assert_eq!(mqtt.events[0], Event::Incoming(Incoming::Publish(publish)));
    }

    #[test]
    fn handle_incoming_packet_preserves_retained_empty_payload_publish() {
        let mut mqtt = build_mqttstate();
        let mut publish = build_incoming_publish(QoS::AtMostOnce, 0);
        publish.retain = true;
        publish.payload = Bytes::new();

        mqtt.handle_incoming_packet(Incoming::Publish(publish.clone()))
            .unwrap();

        assert_eq!(mqtt.events.len(), 1);
        assert_eq!(mqtt.events[0], Event::Incoming(Incoming::Publish(publish)));
    }

    #[test]
    fn incoming_qos2_publish_should_send_rec_to_network_and_publish_to_user() {
        let mut mqtt = build_mqttstate();
        let publish = build_incoming_publish(QoS::ExactlyOnce, 1);

        let packet = mqtt.handle_incoming_publish(&publish).unwrap();
        match packet.outgoing {
            Some(Packet::PubRec(pubrec)) => assert_eq!(pubrec.pkid, 1),
            _ => panic!("Invalid network request: {packet:?}"),
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

        mqtt.handle_incoming_puback(&PubAck::new(1)).unwrap();
        assert_eq!(mqtt.inflight, 1);

        mqtt.handle_incoming_puback(&PubAck::new(2)).unwrap();
        assert_eq!(mqtt.inflight, 0);

        assert!(mqtt.outgoing_pub[1].is_none());
        assert!(mqtt.outgoing_pub[2].is_none());
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

        mqtt.handle_incoming_puback(&PubAck::new(2)).unwrap();
        assert_eq!(mqtt.last_puback, 0);

        mqtt.handle_incoming_puback(&PubAck::new(1)).unwrap();
        assert_eq!(mqtt.last_puback, 2);

        mqtt.handle_incoming_puback(&PubAck::new(3)).unwrap();
        assert_eq!(mqtt.last_puback, 3);
    }

    #[test]
    fn control_packet_id_gap_does_not_leave_completed_publish_undrained() {
        let mut mqtt = build_mqttstate();

        mqtt.outgoing_subscribe(Subscribe::new("a/b", QoS::AtMostOnce), None)
            .unwrap();
        mqtt.handle_incoming_suback(&SubAck::new(
            1,
            vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
        ))
        .unwrap();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();
        mqtt.handle_incoming_puback(&PubAck::new(2)).unwrap();

        assert_eq!(mqtt.inflight, 0);
        assert!(mqtt.outbound_requests_drained());
        assert!(mqtt.outgoing_pub_ack.ones().next().is_none());
    }

    #[test]
    fn replayed_control_packet_id_behind_frontier_does_not_leave_stale_ack_bit() {
        let mut mqtt = build_mqttstate();
        mqtt.last_puback = 1;
        mqtt.last_pkid = 1;

        let mut subscribe = Subscribe::new("a/b", QoS::AtMostOnce);
        subscribe.pkid = 1;
        mqtt.outgoing_subscribe(subscribe, None).unwrap();
        mqtt.handle_incoming_suback(&SubAck::new(
            1,
            vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
        ))
        .unwrap();

        assert!(mqtt.outbound_requests_drained());
        assert!(mqtt.outgoing_pub_ack.ones().next().is_none());
    }

    #[test]
    fn control_packet_ids_above_warm_tracking_capacity_do_not_panic() {
        let mut mqtt = build_mqttstate();

        for i in 0..40 {
            mqtt.outgoing_subscribe(Subscribe::new(format!("a/{i}"), QoS::AtMostOnce), None)
                .unwrap();
        }

        for pkid in 1..=40 {
            mqtt.handle_incoming_suback(&SubAck::new(
                pkid,
                vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
            ))
            .unwrap();
        }

        assert!(mqtt.outgoing_pub_ack.len() > MqttState::WARM_TRACKING_SLOTS + 1);
        assert!(mqtt.outbound_requests_drained());
        assert!(mqtt.outgoing_pub_ack.ones().next().is_none());
    }

    #[test]
    fn control_packet_identifier_skips_reserved_publish_identifier() {
        let mut mqtt = MqttState::builder(1).build();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::AtLeastOnce))
            .unwrap();

        let packet = mqtt
            .outgoing_subscribe(Subscribe::new("a/b", QoS::AtMostOnce), None)
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
        mqtt.outgoing_subscribe(Subscribe::new("a/b", QoS::AtMostOnce), None)
            .unwrap();

        mqtt.handle_incoming_suback(&SubAck::new(
            1,
            vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
        ))
        .unwrap();

        assert!(!mqtt.packet_identifier_in_use(1));
        mqtt.reserve_control_pkid(1).unwrap();
        assert!(mqtt.packet_identifier_in_use(1));
    }

    #[test]
    fn qos2_publish_identifier_remains_reserved_until_pubcomp() {
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::ExactlyOnce))
            .unwrap();

        mqtt.handle_incoming_pubrec(&PubRec::new(1)).unwrap();
        assert!(mqtt.packet_identifier_in_use(1));

        mqtt.handle_incoming_pubcomp(&PubComp::new(1)).unwrap();
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

        mqtt.handle_incoming_pubrec(&PubRec::new(1)).unwrap();
        mqtt.handle_incoming_puback(&PubAck::new(2)).unwrap();
        mqtt.handle_incoming_puback(&PubAck::new(3)).unwrap();
        mqtt.handle_incoming_pubcomp(&PubComp::new(1)).unwrap();
        mqtt.handle_incoming_pubrec(&PubRec::new(4)).unwrap();
        mqtt.handle_incoming_pubcomp(&PubComp::new(4)).unwrap();

        assert_eq!(mqtt.inflight, 0);
        assert!(mqtt.outbound_requests_drained());
        assert!(mqtt.outgoing_pub_ack.ones().next().is_none());
        assert!(mqtt.outgoing_rel.ones().next().is_none());
    }

    #[test]
    fn incoming_puback_with_pkid_greater_than_max_inflight_should_be_handled_gracefully() {
        let mut mqtt = build_mqttstate();

        let got = mqtt.handle_incoming_puback(&PubAck::new(101)).unwrap_err();

        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 101),
            e => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn incoming_puback_with_pkid_beyond_allocated_tracking_is_unsolicited() {
        let mut mqtt = build_mqttstate();

        let got = mqtt.handle_incoming_puback(&PubAck::new(50)).unwrap_err();

        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 50),
            e => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn outgoing_publish_with_pkid_above_max_inflight_is_unsolicited_and_does_not_grow_tracking() {
        let mut mqtt = MqttState::builder(10).build();
        let mut publish = build_outgoing_publish(QoS::AtLeastOnce);
        publish.pkid = 50;

        let got = mqtt
            .handle_outgoing_packet(Request::Publish(publish))
            .unwrap_err();

        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 50),
            e => panic!("Unexpected error: {e}"),
        }
        assert_eq!(mqtt.outgoing_pub.len(), 11);
        assert_eq!(mqtt.outgoing_pub_notice.len(), 11);
        assert_eq!(mqtt.outgoing_rel_notice.len(), 11);
        assert_eq!(mqtt.inflight, 0);
    }

    #[test]
    fn outgoing_pubrel_with_pkid_above_max_inflight_is_unsolicited_and_does_not_grow_tracking() {
        let mut mqtt = MqttState::builder(10).build();

        let got = mqtt
            .handle_outgoing_packet(Request::PubRel(PubRel::new(50)))
            .unwrap_err();

        match got {
            StateError::Unsolicited(pkid) => assert_eq!(pkid, 50),
            e => panic!("Unexpected error: {e}"),
        }
        assert_eq!(mqtt.outgoing_pub.len(), 11);
        assert_eq!(mqtt.outgoing_pub_notice.len(), 11);
        assert_eq!(mqtt.outgoing_rel_notice.len(), 11);
        assert_eq!(mqtt.inflight, 0);
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

        mqtt.handle_incoming_puback(&PubAck::new(2)).unwrap();
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
    fn incoming_pubrec_should_release_publish_from_queue_and_add_relid_to_rel_queue() {
        let mut mqtt = build_mqttstate();

        let publish1 = build_outgoing_publish(QoS::AtLeastOnce);
        let publish2 = build_outgoing_publish(QoS::ExactlyOnce);

        let _publish_out = mqtt.outgoing_publish(publish1);
        let _publish_out = mqtt.outgoing_publish(publish2);

        mqtt.handle_incoming_pubrec(&PubRec::new(2)).unwrap();
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
        let packet = mqtt.outgoing_publish(publish).unwrap().unwrap();
        match packet {
            Packet::Publish(publish) => assert_eq!(publish.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }

        let packet = mqtt
            .handle_incoming_pubrec(&PubRec::new(1))
            .unwrap()
            .outgoing
            .unwrap();
        match packet {
            Packet::PubRel(pubrel) => assert_eq!(pubrel.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }
    }

    #[test]
    fn outgoing_publish_cannot_reuse_pkid_reserved_for_pubrel_replay() {
        let mut mqtt = build_mqttstate();

        mqtt.outgoing_publish(build_outgoing_publish(QoS::ExactlyOnce))
            .unwrap();
        mqtt.handle_incoming_pubrec(&PubRec::new(1)).unwrap();
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
    fn incoming_pubrel_should_send_comp_to_network_and_nothing_to_user() {
        let mut mqtt = build_mqttstate();
        let publish = build_incoming_publish(QoS::ExactlyOnce, 1);

        let packet = mqtt.handle_incoming_publish(&publish).unwrap();
        match packet.outgoing {
            Some(Packet::PubRec(pubrec)) => assert_eq!(pubrec.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }

        let packet = mqtt
            .handle_incoming_pubrel(&PubRel::new(1))
            .unwrap()
            .unwrap();
        match packet {
            Packet::PubComp(pubcomp) => assert_eq!(pubcomp.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }

        let packet = mqtt
            .handle_incoming_pubrel(&PubRel::new(1))
            .unwrap()
            .unwrap();
        match packet {
            Packet::PubComp(pubcomp) => assert_eq!(pubcomp.pkid, 1),
            packet => panic!("Invalid recovery network request: {packet:?}"),
        }
    }

    #[test]
    fn incoming_pubrel_before_pubrec_is_unsolicited() {
        let mut mqtt = build_mqttstate();
        mqtt.ack_mode = AckMode::Manual;
        let publish = build_incoming_publish(QoS::ExactlyOnce, 1);

        assert!(mqtt.handle_incoming_publish(&publish).unwrap().is_none());

        assert!(matches!(
            mqtt.handle_incoming_pubrel(&PubRel::new(1)),
            Err(StateError::Unsolicited(1))
        ));
    }

    #[test]
    fn clean_preserves_incomplete_incoming_qos2_state_for_session_resume() {
        let mut mqtt = build_mqttstate();
        let publish = build_incoming_publish(QoS::ExactlyOnce, 1);

        let packet = mqtt.handle_incoming_publish(&publish).unwrap();
        match packet.outgoing {
            Some(Packet::PubRec(pubrec)) => assert_eq!(pubrec.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }

        let pending = mqtt.clean();

        assert!(pending.is_empty());
        assert!(mqtt.incoming_pub.contains(1));
        assert!(mqtt.incoming_pubrec.contains(1));

        let packet = mqtt
            .handle_incoming_pubrel(&PubRel::new(1))
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
    fn clean_resends_pubrec_for_duplicate_incoming_qos2_publish_after_session_resume() {
        let mut mqtt = build_mqttstate();
        let publish = build_incoming_publish(QoS::ExactlyOnce, 1);

        mqtt.handle_incoming_publish(&publish).unwrap();
        mqtt.clean();

        let packet = mqtt.handle_incoming_publish(&publish).unwrap();

        match packet.outgoing {
            Some(Packet::PubRec(pubrec)) => assert_eq!(pubrec.pkid, 1),
            packet => panic!("Invalid network request for duplicate QoS2 publish: {packet:?}"),
        }
        assert!(mqtt.incoming_pub.contains(1));
        assert!(mqtt.incoming_pubrec.contains(1));
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
        match &replay[0] {
            Request::Publish(publish) => {
                assert_eq!(publish.pkid, 1);
                assert!(publish.dup);
                assert_eq!(publish.qos, QoS::AtLeastOnce);
                assert_eq!(publish.topic.as_ref(), b"hello/world");
            }
            request => panic!("expected restored publish replay, got {request:?}"),
        }
    }

    #[test]
    fn persisted_session_restores_qos2_pubrel_replay() {
        let options = persistent_options();
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_publish(build_outgoing_publish(QoS::ExactlyOnce))
            .unwrap();
        mqtt.handle_incoming_pubrec(&PubRec::new(1)).unwrap();

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
    fn persisted_session_restores_pubrel_replay_above_warm_tracking_capacity() {
        let options = persistent_options();
        let session = PersistedSession {
            format_version: 1,
            client_id: "client".to_owned(),
            clean_session: false,
            max_inflight: 100,
            ack_mode: PersistedAckMode::Automatic,
            last_pkid: 33,
            last_puback: 0,
            replay: vec![PersistedRequest::PubRel(PersistedPubRel { pkid: 33 })],
            incoming_qos2: vec![],
        };
        let mut restored = build_mqttstate();

        let mut replay = restored
            .restore_persisted_session(&options, &session)
            .expect("valid pubrel replay above warm capacity should restore");

        assert!(restored.outgoing_rel_replay.contains(33));
        let packet = restored
            .handle_outgoing_packet(replay.remove(0))
            .expect("restored pubrel should be accepted")
            .expect("restored pubrel should produce a packet");
        match packet {
            Packet::PubRel(pubrel) => assert_eq!(pubrel.pkid, 33),
            packet => panic!("expected restored PUBREL replay, got {packet:?}"),
        }
    }

    #[test]
    fn persisted_session_replays_control_packets_with_original_identifiers() {
        let options = persistent_options();
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_subscribe(Subscribe::new("a/b", QoS::AtMostOnce), None)
            .unwrap();
        mqtt.outgoing_unsubscribe(Unsubscribe::new("c/d"), None)
            .unwrap();

        let session = mqtt.persisted_session(&options, std::iter::empty());
        let mut restored = build_mqttstate();
        let replay = restored
            .restore_persisted_session(&options, &session)
            .expect("persisted session should restore");

        assert_eq!(replay.len(), 2);
        for request in replay {
            let packet = restored
                .handle_outgoing_packet(request)
                .expect("restored control replay should be accepted")
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
            Request::Subscribe(Subscribe::new("a/b", QoS::AtMostOnce)),
            Request::Unsubscribe(Unsubscribe::new("c/d")),
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
            .inflight(1)
            .build();
        let mut subscribe = Subscribe::new("a/b", QoS::AtMostOnce);
        subscribe.pkid = 2;
        let mut unsubscribe = Unsubscribe::new("c/d");
        unsubscribe.pkid = 3;
        let session = PersistedSession {
            format_version: 1,
            client_id: "client".to_owned(),
            clean_session: false,
            max_inflight: 1,
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
            .inflight(1)
            .build();
        let session = PersistedSession {
            format_version: 1,
            client_id: "client".to_owned(),
            clean_session: false,
            max_inflight: 1,
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
                max_inflight: 1,
            }
        );
        assert_eq!(restored.last_pkid, 9);
    }

    #[test]
    fn persisted_session_rejects_last_puback_above_outgoing_inflight_limit() {
        let options = MqttOptions::builder("client", "localhost")
            .session_mode(SessionMode::Persistent)
            .inflight(10)
            .build();
        let session = PersistedSession {
            format_version: 1,
            client_id: "client".to_owned(),
            clean_session: false,
            max_inflight: 10,
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
                max_inflight: 10,
            }
        );
        assert_eq!(restored.last_puback, 4);
    }

    #[test]
    fn reset_session_state_discards_incomplete_incoming_qos2_state() {
        let mut mqtt = build_mqttstate();
        let publish = build_incoming_publish(QoS::ExactlyOnce, 1);

        mqtt.handle_incoming_publish(&publish).unwrap();
        assert!(mqtt.incoming_pub.contains(1));
        assert!(mqtt.incoming_pubrec.contains(1));

        mqtt.reset_session_state();

        assert!(!mqtt.incoming_pub.contains(1));
        assert!(!mqtt.incoming_pubrec.contains(1));
        let packet = mqtt
            .handle_incoming_pubrel(&PubRel::new(1))
            .unwrap()
            .unwrap();
        match packet {
            Packet::PubComp(pubcomp) => assert_eq!(pubcomp.pkid, 1),
            packet => panic!("Invalid recovery network request after session reset: {packet:?}"),
        }
    }

    #[test]
    fn incoming_pubcomp_should_release_correct_pkid_from_release_queue() {
        let mut mqtt = build_mqttstate();
        let publish = build_outgoing_publish(QoS::ExactlyOnce);

        mqtt.outgoing_publish(publish).unwrap();
        mqtt.handle_incoming_pubrec(&PubRec::new(1)).unwrap();

        mqtt.handle_incoming_pubcomp(&PubComp::new(1)).unwrap();
        assert_eq!(mqtt.inflight, 0);
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

        mqtt.handle_incoming_pubrec(&PubRec::new(1)).unwrap();
        let packet = mqtt
            .handle_incoming_pubcomp(&PubComp::new(1))
            .unwrap()
            .outgoing
            .unwrap();
        match packet {
            Packet::Publish(publish) => assert_eq!(publish.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }

        assert!(mqtt.outgoing_pub[1].is_some());
        assert_eq!(mqtt.inflight, 1);

        let packet = mqtt
            .handle_incoming_pubrec(&PubRec::new(1))
            .unwrap()
            .outgoing
            .unwrap();
        match packet {
            Packet::PubRel(pubrel) => assert_eq!(pubrel.pkid, 1),
            packet => panic!("Invalid network request: {packet:?}"),
        }
    }

    #[test]
    fn outgoing_ping_handle_should_throw_errors_for_no_pingresp() {
        let mut mqtt = build_mqttstate();
        mqtt.outgoing_ping().unwrap();

        // network activity other than pingresp
        let publish = build_outgoing_publish(QoS::AtLeastOnce);
        mqtt.handle_outgoing_packet(Request::Publish(publish))
            .unwrap();
        mqtt.handle_incoming_packet(Incoming::PubAck(PubAck::new(1)))
            .unwrap();

        // should throw error because we didn't get pingresp for previous ping
        match mqtt.outgoing_ping() {
            Ok(_) => panic!("Should throw pingresp await error"),
            Err(StateError::AwaitPingResp) => (),
            Err(e) => panic!("Should throw pingresp await error. Error = {e:?}"),
        }
    }

    #[test]
    fn handle_outgoing_packet_rejects_incoming_only_request_variants() {
        let mut mqtt = build_mqttstate();

        let err = mqtt.handle_outgoing_packet(Request::PingResp).unwrap_err();

        assert!(matches!(err, StateError::InvalidState));
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

    #[test]
    fn duplicate_connack_after_connection_establishment_is_protocol_error() {
        let mut mqtt = build_mqttstate();

        let err = mqtt
            .handle_incoming_packet(Incoming::ConnAck(ConnAck::new(
                ConnectReturnCode::Success,
                false,
            )))
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::ProtocolViolation(ProtocolViolation::DuplicateConnAck)
        ));
        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn inbound_connect_after_connection_establishment_is_protocol_error() {
        let mut mqtt = build_mqttstate();

        let err = mqtt
            .handle_incoming_packet(Incoming::Connect(Connect::new("client")))
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
        let mut subscribe = Subscribe::new("topic/one", QoS::AtLeastOnce);
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
        let mut unsubscribe = Unsubscribe::new("topic/one");
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
    fn inbound_disconnect_after_connection_establishment_is_protocol_error() {
        let mut mqtt = build_mqttstate();

        let err = mqtt
            .handle_incoming_packet(Incoming::Disconnect)
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::ProtocolViolation(ProtocolViolation::UnexpectedIncomingPacket(
                PacketType::Disconnect
            ))
        ));
        assert!(mqtt.events.is_empty());
    }

    #[test]
    fn clean_is_calculating_pending_correctly() {
        fn build_outgoing_pub() -> Vec<Option<Publish>> {
            vec![
                None,
                Some(Publish {
                    dup: false,
                    qos: QoS::AtMostOnce,
                    retain: false,
                    topic: Bytes::from_static(b"test"),
                    pkid: 1,
                    payload: "".into(),
                }),
                Some(Publish {
                    dup: false,
                    qos: QoS::AtMostOnce,
                    retain: false,
                    topic: Bytes::from_static(b"test"),
                    pkid: 2,
                    payload: "".into(),
                }),
                Some(Publish {
                    dup: false,
                    qos: QoS::AtMostOnce,
                    retain: false,
                    topic: Bytes::from_static(b"test"),
                    pkid: 3,
                    payload: "".into(),
                }),
                None,
                None,
                Some(Publish {
                    dup: false,
                    qos: QoS::AtMostOnce,
                    retain: false,
                    topic: Bytes::from_static(b"test"),
                    pkid: 6,
                    payload: "".into(),
                }),
            ]
        }

        let mut mqtt = build_mqttstate();
        mqtt.outgoing_pub = build_outgoing_pub();
        mqtt.last_puback = 3;
        let requests = mqtt.clean();
        let res = vec![6, 1, 2, 3];
        for (req, idx) in requests.iter().zip(res) {
            if let Request::Publish(publish) = req {
                assert_eq!(publish.pkid, idx);
            } else {
                unreachable!()
            }
        }

        mqtt.outgoing_pub = build_outgoing_pub();
        mqtt.last_puback = 0;
        let requests = mqtt.clean();
        let res = vec![1, 2, 3, 6];
        for (req, idx) in requests.iter().zip(res) {
            if let Request::Publish(publish) = req {
                assert_eq!(publish.pkid, idx);
            } else {
                unreachable!()
            }
        }

        mqtt.outgoing_pub = build_outgoing_pub();
        mqtt.last_puback = 6;
        let requests = mqtt.clean();
        let res = vec![1, 2, 3, 6];
        for (req, idx) in requests.iter().zip(res) {
            if let Request::Publish(publish) = req {
                assert_eq!(publish.pkid, idx);
            } else {
                unreachable!()
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
}
