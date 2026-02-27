use super::mqttbytes::v5::{
    Auth, AuthReasonCode, ConnAck, ConnectReturnCode, Disconnect, DisconnectReasonCode, Packet,
    PingReq, PubAck, PubAckReason, PubComp, PubCompReason, PubRec, PubRecReason, PubRel,
    PubRelReason, Publish, SubAck, Subscribe, SubscribeReasonCode, UnsubAck, UnsubAckReason,
    Unsubscribe,
};
use super::mqttbytes::{self, Error as MqttError, QoS};
use crate::notice::{PublishNoticeTx, RequestNoticeTx, TrackedNoticeTx};
use crate::{PublishNoticeError, RequestNoticeError};

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
                StateError::OutgoingPacketTooLarge { pkt_size, max }
            }
            e => StateError::Deserialization(e),
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
    /// Outgoing QoS 1, 2 publishes which aren't acked yet
    pub(crate) outgoing_pub: Vec<Option<Publish>>,
    /// Notice handles for outgoing QoS 1, 2 publishes
    pub(crate) outgoing_pub_notice: Vec<Option<PublishNoticeTx>>,
    /// Packet ids acked by broker while waiting to advance last contiguous ack boundary
    pub(crate) outgoing_pub_ack: FixedBitSet,
    /// Packet ids of released QoS 2 publishes
    pub(crate) outgoing_rel: FixedBitSet,
    /// Notice handles for outgoing QoS 2 pubrels
    pub(crate) outgoing_rel_notice: Vec<Option<PublishNoticeTx>>,
    /// Packet ids on incoming QoS 2 publishes
    pub(crate) incoming_pub: FixedBitSet,
    /// Last collision due to broker not acking in order
    pub collision: Option<Publish>,
    /// Notice handle for the collision publish
    pub(crate) collision_notice: Option<PublishNoticeTx>,
    /// Tracked subscribe requests waiting for SubAck
    pub(crate) tracked_subscribe: BTreeMap<u16, (Subscribe, RequestNoticeTx)>,
    /// Tracked unsubscribe requests waiting for UnsubAck
    pub(crate) tracked_unsubscribe: BTreeMap<u16, (Unsubscribe, RequestNoticeTx)>,
    /// Buffered incoming packets
    pub events: VecDeque<Event>,
    /// Indicates if acknowledgements should be send immediately
    pub manual_acks: bool,
    /// Map of alias_id->topic
    topic_alises: HashMap<u16, Bytes>,
    /// `topic_alias_maximum` RECEIVED via connack packet
    pub broker_topic_alias_max: u16,
    /// Maximum number of allowed inflight QoS1 & QoS2 requests
    pub(crate) max_outgoing_inflight: u16,
    /// Upper limit on the maximum number of allowed inflight QoS1 & QoS2 requests
    max_outgoing_inflight_upper_limit: u16,
    /// Authentication manager
    auth_manager: Option<Arc<Mutex<dyn AuthManager>>>,
}

impl MqttState {
    fn new_notice_slots(max_inflight: u16) -> Vec<Option<PublishNoticeTx>> {
        let size = max_inflight as usize + 1;
        std::iter::repeat_with(|| None).take(size).collect()
    }

    /// Creates new mqtt state. Same state should be used during a
    /// connection for persistent sessions while new state should
    /// instantiated for clean sessions
    pub fn new(
        max_inflight: u16,
        manual_acks: bool,
        auth_manager: Option<Arc<Mutex<dyn AuthManager>>>,
    ) -> Self {
        MqttState {
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
            // TODO: Optimize these sizes later
            events: VecDeque::with_capacity(100),
            manual_acks,
            topic_alises: HashMap::new(),
            // Set via CONNACK
            broker_topic_alias_max: 0,
            max_outgoing_inflight: max_inflight,
            max_outgoing_inflight_upper_limit: max_inflight,
            auth_manager,
        }
    }

    pub(crate) fn clean_with_notices(&mut self) -> Vec<(Request, Option<TrackedNoticeTx>)> {
        let mut pending = Vec::with_capacity(100);
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
                Some(TrackedNoticeTx::Request(notice)),
            ));
        }
        for (pkid, (mut unsubscribe, notice)) in std::mem::take(&mut self.tracked_unsubscribe) {
            unsubscribe.pkid = pkid;
            pending.push((
                Request::Unsubscribe(unsubscribe),
                Some(TrackedNoticeTx::Request(notice)),
            ));
        }

        // remove packed ids of incoming qos2 publishes
        self.incoming_pub.clear();

        self.await_pingresp = false;
        self.collision_ping_count = 0;
        self.inflight = 0;
        pending
    }

    /// Returns inflight outgoing packets and clears internal queues
    pub fn clean(&mut self) -> Vec<Request> {
        self.clean_with_notices()
            .into_iter()
            .map(|(request, _)| request)
            .collect()
    }

    pub fn inflight(&self) -> u16 {
        self.inflight
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
        for (_, (_, notice)) in std::mem::take(&mut self.tracked_subscribe) {
            notice.error(RequestNoticeError::SessionReset);
        }
        for (_, (_, notice)) in std::mem::take(&mut self.tracked_unsubscribe) {
            notice.error(RequestNoticeError::SessionReset);
        }
        self.clear_collision();
    }

    /// Consolidates handling of all outgoing mqtt packet logic. Returns a packet which should
    /// be put on to the network by the eventloop
    pub fn handle_outgoing_packet(
        &mut self,
        request: Request,
    ) -> Result<Option<Packet>, StateError> {
        let (packet, flush_notice) = self.handle_outgoing_packet_with_notice(request, None)?;
        if let Some(tx) = flush_notice {
            tx.success();
        }
        Ok(packet)
    }

    pub(crate) fn handle_outgoing_packet_with_notice(
        &mut self,
        request: Request,
        notice: Option<TrackedNoticeTx>,
    ) -> Result<(Option<Packet>, Option<PublishNoticeTx>), StateError> {
        let result = match request {
            Request::Publish(publish) => {
                let publish_notice = match notice {
                    Some(TrackedNoticeTx::Publish(notice)) => Some(notice),
                    Some(TrackedNoticeTx::Request(_)) => None,
                    None => None,
                };
                self.outgoing_publish_with_notice(publish, publish_notice)?
            }
            Request::PubRel(pubrel) => {
                let publish_notice = match notice {
                    Some(TrackedNoticeTx::Publish(notice)) => Some(notice),
                    Some(TrackedNoticeTx::Request(_)) => None,
                    None => None,
                };
                self.outgoing_pubrel_with_notice(pubrel, publish_notice)?
            }
            Request::Subscribe(subscribe) => {
                let request_notice = match notice {
                    Some(TrackedNoticeTx::Request(notice)) => Some(notice),
                    Some(TrackedNoticeTx::Publish(_)) => None,
                    None => None,
                };
                (self.outgoing_subscribe(subscribe, request_notice)?, None)
            }
            Request::Unsubscribe(unsubscribe) => {
                let request_notice = match notice {
                    Some(TrackedNoticeTx::Request(notice)) => Some(notice),
                    Some(TrackedNoticeTx::Publish(_)) => None,
                    None => None,
                };
                (
                    self.outgoing_unsubscribe(unsubscribe, request_notice)?,
                    None,
                )
            }
            Request::PingReq => (self.outgoing_ping()?, None),
            Request::Disconnect(disconnect) => (self.outgoing_disconnect(disconnect)?, None),
            Request::PubAck(puback) => (self.outgoing_puback(puback)?, None),
            Request::PubRec(pubrec) => (self.outgoing_pubrec(pubrec)?, None),
            Request::Auth(auth) => (self.outgoing_auth(auth)?, None),
            _ => unimplemented!(),
        };

        self.last_outgoing = Instant::now();
        Ok(result)
    }

    /// Consolidates handling of all incoming mqtt packets. Returns a `Notification` which for the
    /// user to consume and `Packet` which for the eventloop to put on the network
    /// E.g For incoming QoS1 publish packet, this method returns (Publish, Puback). Publish packet will
    /// be forwarded to user and Pubck packet will be written to network
    pub fn handle_incoming_packet(
        &mut self,
        mut packet: Incoming,
    ) -> Result<Option<Packet>, StateError> {
        let events_len_before = self.events.len();
        let outgoing = match &mut packet {
            Incoming::PingResp(_) => self.handle_incoming_pingresp(),
            Incoming::Publish(publish) => self.handle_incoming_publish(publish),
            Incoming::SubAck(suback) => self.handle_incoming_suback(suback),
            Incoming::UnsubAck(unsuback) => self.handle_incoming_unsuback(unsuback),
            Incoming::PubAck(puback) => self.handle_incoming_puback(puback),
            Incoming::PubRec(pubrec) => self.handle_incoming_pubrec(pubrec),
            Incoming::PubRel(pubrel) => self.handle_incoming_pubrel(pubrel),
            Incoming::PubComp(pubcomp) => self.handle_incoming_pubcomp(pubcomp),
            Incoming::ConnAck(connack) => self.handle_incoming_connack(connack),
            Incoming::Disconnect(disconn) => Self::handle_incoming_disconn(disconn),
            Incoming::Auth(auth) => self.handle_incoming_auth(auth),
            _ => {
                error!("Invalid incoming packet = {:?}", packet);
                Err(StateError::WrongPacket)
            }
        };

        // Preserve original event ordering (Incoming first, derived Outgoing next)
        // without cloning the incoming packet.
        self.events
            .insert(events_len_before, Event::Incoming(packet));

        let outgoing = outgoing?;
        self.last_incoming = Instant::now();
        Ok(outgoing)
    }

    pub fn handle_protocol_error(&mut self) -> Result<Option<Packet>, StateError> {
        // send DISCONNECT packet with REASON_CODE 0x82
        let disconnect = Disconnect::new(DisconnectReasonCode::ProtocolError);
        self.outgoing_disconnect(disconnect)
    }

    pub fn clear_collision(&mut self) {
        self.collision = None;
        self.collision_notice = None;
        self.collision_ping_count = 0;
    }

    fn handle_incoming_suback(&mut self, suback: &SubAck) -> Result<Option<Packet>, StateError> {
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
            let failures: Vec<_> = suback
                .return_codes
                .iter()
                .filter(|reason| !matches!(reason, SubscribeReasonCode::Success(_)))
                .cloned()
                .collect();
            if failures.is_empty() {
                notice.success();
            } else {
                notice.error(RequestNoticeError::V5SubAckFailure(failures));
            }
        }
        Ok(None)
    }

    fn handle_incoming_unsuback(
        &mut self,
        unsuback: &UnsubAck,
    ) -> Result<Option<Packet>, StateError> {
        for reason in &unsuback.reasons {
            if reason != &UnsubAckReason::Success {
                warn!("UnsubAck Pkid = {:?}, Reason = {:?}", unsuback.pkid, reason);
            }
        }
        if let Some((_, notice)) = self.tracked_unsubscribe.remove(&unsuback.pkid) {
            let failures: Vec<_> = unsuback
                .reasons
                .iter()
                .filter(|reason| {
                    **reason != UnsubAckReason::Success
                        && **reason != UnsubAckReason::NoSubscriptionExisted
                })
                .cloned()
                .collect();
            if failures.is_empty() {
                notice.success();
            } else {
                notice.error(RequestNoticeError::V5UnsubAckFailure(failures));
            }
        }
        Ok(None)
    }

    fn handle_incoming_connack(&mut self, connack: &ConnAck) -> Result<Option<Packet>, StateError> {
        if connack.code != ConnectReturnCode::Success {
            return Err(StateError::ConnFail {
                reason: connack.code,
            });
        }

        if let Some(props) = &connack.properties {
            if let Some(topic_alias_max) = props.topic_alias_max {
                self.broker_topic_alias_max = topic_alias_max;
            }

            if let Some(max_inflight) = props.receive_max {
                self.max_outgoing_inflight =
                    max_inflight.min(self.max_outgoing_inflight_upper_limit);
                // FIXME: Maybe resize the pubrec and pubrel queues here
                // to save some space.
            }
        }
        Ok(None)
    }

    fn handle_incoming_disconn(disconn: &Disconnect) -> Result<Option<Packet>, StateError> {
        let reason_code = disconn.reason_code;
        let reason_string = if let Some(props) = &disconn.properties {
            props.reason_string.clone()
        } else {
            None
        };
        Err(StateError::ServerDisconnect {
            reason_code,
            reason_string,
        })
    }

    /// Results in a publish notification in all the QoS cases. Replys with an ack
    /// in case of QoS1 and Replys rec in case of QoS while also storing the message
    fn handle_incoming_publish(
        &mut self,
        publish: &mut Publish,
    ) -> Result<Option<Packet>, StateError> {
        let qos = publish.qos;

        let topic_alias = match &publish.properties {
            Some(props) => props.topic_alias,
            None => None,
        };

        if !publish.topic.is_empty() {
            if let Some(alias) = topic_alias {
                self.topic_alises.insert(alias, publish.topic.clone());
            }
        } else if let Some(alias) = topic_alias {
            if let Some(topic) = self.topic_alises.get(&alias) {
                topic.clone_into(&mut publish.topic);
            } else {
                self.handle_protocol_error()?;
            }
        }

        match qos {
            QoS::AtMostOnce => Ok(None),
            QoS::AtLeastOnce => {
                if !self.manual_acks {
                    let puback = PubAck::new(publish.pkid, None);
                    return self.outgoing_puback(puback);
                }
                Ok(None)
            }
            QoS::ExactlyOnce => {
                let pkid = publish.pkid;
                self.incoming_pub.insert(pkid as usize);

                if !self.manual_acks {
                    let pubrec = PubRec::new(pkid, None);
                    return self.outgoing_pubrec(pubrec);
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
            if let Some(tx) = notice {
                tx.error(PublishNoticeError::V5PubAck(puback.reason));
            }
            return Ok(None);
        }
        if let Some(tx) = notice {
            tx.success();
        }

        if let Some((publish, notice)) = self.check_collision(puback.pkid) {
            self.outgoing_pub[publish.pkid as usize] = Some(publish.clone());
            self.outgoing_pub_notice[publish.pkid as usize] = notice;
            self.inflight += 1;

            let pkid = publish.pkid;
            let event = Event::Outgoing(Outgoing::Publish(pkid));
            self.events.push_back(event);
            self.collision_ping_count = 0;

            return Ok(Some(Packet::Publish(publish)));
        }

        Ok(None)
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
                tx.error(PublishNoticeError::V5PubRec(pubrec.reason));
            }
            return Ok(None);
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

        if pubcomp.reason != PubCompReason::Success {
            warn!(
                "PubComp Pkid = {:?}, reason: {:?}",
                pubcomp.pkid, pubcomp.reason
            );
            if let Some(tx) = notice {
                tx.error(PublishNoticeError::V5PubComp(pubcomp.reason));
            }
            return Ok(None);
        }
        if let Some(tx) = notice {
            tx.success();
        }

        self.inflight -= 1;
        if let Some((publish, notice)) = self.check_collision(pubcomp.pkid) {
            let pkid = publish.pkid;
            self.outgoing_pub[pkid as usize] = Some(publish.clone());
            self.outgoing_pub_notice[pkid as usize] = notice;
            self.inflight += 1;

            let event = Event::Outgoing(Outgoing::Publish(pkid));
            self.events.push_back(event);
            self.collision_ping_count = 0;

            return Ok(Some(Packet::Publish(publish)));
        }

        Ok(None)
    }

    fn handle_incoming_pingresp(&mut self) -> Result<Option<Packet>, StateError> {
        self.await_pingresp = false;
        Ok(None)
    }

    fn handle_incoming_auth(&mut self, auth: &Auth) -> Result<Option<Packet>, StateError> {
        match auth.code {
            AuthReasonCode::Success => Ok(None),
            AuthReasonCode::Continue => {
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

                self.outgoing_auth(client_auth)
            }
            AuthReasonCode::ReAuthenticate => {
                Err(StateError::AuthError("Authentication Failed!".to_string()))
            }
        }
    }

    /// Adds next packet identifier to QoS 1 and 2 publish packets and returns
    /// it buy wrapping publish in packet
    #[cfg(test)]
    fn outgoing_publish(&mut self, publish: Publish) -> Result<Option<Packet>, StateError> {
        let (packet, flush_notice) = self.outgoing_publish_with_notice(publish, None)?;
        if let Some(tx) = flush_notice {
            tx.success();
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
            self.outgoing_pub[pkid as usize] = Some(publish.clone());
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

        if let Some(props) = &publish.properties {
            if let Some(alias) = props.topic_alias {
                if alias > self.broker_topic_alias_max {
                    // We MUST NOT send a Topic Alias that is greater than the
                    // broker's Topic Alias Maximum.
                    return Err(StateError::InvalidAlias {
                        alias,
                        max: self.broker_topic_alias_max,
                    });
                }
            }
        }

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

    fn outgoing_puback(&mut self, puback: PubAck) -> Result<Option<Packet>, StateError> {
        let pkid = puback.pkid;
        let event = Event::Outgoing(Outgoing::PubAck(pkid));
        self.events.push_back(event);

        Ok(Some(Packet::PubAck(puback)))
    }

    fn outgoing_pubrec(&mut self, pubrec: PubRec) -> Result<Option<Packet>, StateError> {
        let pkid = pubrec.pkid;
        let event = Event::Outgoing(Outgoing::PubRec(pkid));
        self.events.push_back(event);

        Ok(Some(Packet::PubRec(pubrec)))
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
            "Pingreq, last incoming packet before {:?}, last outgoing request before {:?}",
            elapsed_in, elapsed_out,
        );

        let event = Event::Outgoing(Outgoing::PingReq);
        self.events.push_back(event);

        Ok(Some(Packet::PingReq(PingReq)))
    }

    fn outgoing_subscribe(
        &mut self,
        mut subscription: Subscribe,
        notice: Option<RequestNoticeTx>,
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
        notice: Option<RequestNoticeTx>,
    ) -> Result<Option<Packet>, StateError> {
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

        Ok(Some(Packet::Unsubscribe(unsub)))
    }

    fn outgoing_disconnect(
        &mut self,
        disconnect: Disconnect,
    ) -> Result<Option<Packet>, StateError> {
        let reason = disconnect.reason_code;
        debug!("Disconnect with {:?}", reason);
        let event = Event::Outgoing(Outgoing::Disconnect);
        self.events.push_back(event);

        Ok(Some(Packet::Disconnect(disconnect)))
    }

    fn outgoing_auth(&mut self, auth: Auth) -> Result<Option<Packet>, StateError> {
        let props = auth.properties.as_ref().unwrap();
        debug!(
            "Auth packet sent. Auth Method: {:?}. Auth Data: {:?}",
            props.method, props.data
        );
        let event = Event::Outgoing(Outgoing::Auth);
        self.events.push_back(event);
        Ok(Some(Packet::Auth(auth)))
    }

    fn check_collision(&mut self, pkid: u16) -> Option<(Publish, Option<PublishNoticeTx>)> {
        if let Some(publish) = &self.collision {
            if publish.pkid == pkid {
                return self
                    .collision
                    .take()
                    .map(|publish| (publish, self.collision_notice.take()));
            }
        }

        None
    }

    fn save_pubrel_with_notice(
        &mut self,
        mut pubrel: PubRel,
        notice: Option<PublishNoticeTx>,
    ) -> Result<PubRel, StateError> {
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
        Ok(pubrel)
    }

    fn advance_last_puback_frontier(&mut self) {
        let mut next = self.next_puback_boundary_pkid(self.last_puback);
        while next != 0 && self.outgoing_pub_ack.contains(next as usize) {
            self.outgoing_pub_ack.set(next as usize, false);
            self.last_puback = next;
            next = self.next_puback_boundary_pkid(self.last_puback);
        }
    }

    fn next_puback_boundary_pkid(&self, pkid: u16) -> u16 {
        if self.max_outgoing_inflight == 0 {
            return 0;
        }

        if pkid >= self.max_outgoing_inflight {
            1
        } else {
            pkid + 1
        }
    }

    /// http://stackoverflow.com/questions/11115364/mqtt-messageid-practical-implementation
    /// Packet ids are incremented till maximum set inflight messages and reset to 1 after that.
    ///
    fn next_pkid(&mut self) -> u16 {
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
            outgoing_pub_notice: Self::new_notice_slots(self.max_outgoing_inflight_upper_limit),
            outgoing_pub_ack: self.outgoing_pub_ack.clone(),
            outgoing_rel: self.outgoing_rel.clone(),
            outgoing_rel_notice: Self::new_notice_slots(self.max_outgoing_inflight_upper_limit),
            incoming_pub: self.incoming_pub.clone(),
            collision: self.collision.clone(),
            collision_notice: None,
            tracked_subscribe: BTreeMap::new(),
            tracked_unsubscribe: BTreeMap::new(),
            events: self.events.clone(),
            manual_acks: self.manual_acks,
            topic_alises: self.topic_alises.clone(),
            broker_topic_alias_max: self.broker_topic_alias_max,
            max_outgoing_inflight: self.max_outgoing_inflight,
            max_outgoing_inflight_upper_limit: self.max_outgoing_inflight_upper_limit,
            auth_manager: self.auth_manager.clone(),
        }
    }
}

#[cfg(test)]
mod test {
    use super::mqttbytes::v5::*;
    use super::mqttbytes::*;
    use super::{Event, Incoming, Outgoing, Request};
    use super::{MqttState, StateError};

    fn build_outgoing_publish(qos: QoS) -> Publish {
        let topic = "hello/world".to_owned();
        let payload = vec![1, 2, 3];

        let mut publish = Publish::new(topic, QoS::AtLeastOnce, payload, None);
        publish.qos = qos;
        publish
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
        MqttState::new(u16::MAX, false, None)
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
        let mut mqtt = MqttState::new(2, false, None);

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
        mqtt.outgoing_publish(publish.clone()).unwrap();
        assert_eq!(mqtt.last_pkid, 0);
        assert_eq!(mqtt.inflight, 2);
    }

    #[test]
    fn clean_is_calculating_pending_correctly() {
        let mut mqtt = build_mqttstate();

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
            packet => panic!("Invalid network request: {:?}", packet),
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
            e => panic!("Unexpected error: {}", e),
        }
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
            packet => panic!("Invalid network request: {:?}", packet),
        }

        match mqtt
            .handle_incoming_pubrec(&PubRec::new(1, None))
            .unwrap()
            .unwrap()
        {
            Packet::PubRel(pubrel) => assert_eq!(pubrel.pkid, 1),
            packet => panic!("Invalid network request: {:?}", packet),
        }
    }

    #[test]
    fn incoming_pubrel_should_send_comp_to_network_and_nothing_to_user() {
        let mut mqtt = build_mqttstate();
        let mut publish = build_incoming_publish(QoS::ExactlyOnce, 1);

        match mqtt.handle_incoming_publish(&mut publish).unwrap().unwrap() {
            Packet::PubRec(pubrec) => assert_eq!(pubrec.pkid, 1),
            packet => panic!("Invalid network request: {:?}", packet),
        }

        match mqtt
            .handle_incoming_pubrel(&PubRel::new(1, None))
            .unwrap()
            .unwrap()
        {
            Packet::PubComp(pubcomp) => assert_eq!(pubcomp.pkid, 1),
            packet => panic!("Invalid network request: {:?}", packet),
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
            packet => panic!("Invalid network request: {:?}", packet),
        }

        assert!(mqtt.outgoing_pub[1].is_some());
        assert_eq!(mqtt.inflight, 1);

        let packet = mqtt
            .handle_incoming_pubrec(&PubRec::new(1, None))
            .unwrap()
            .unwrap();
        match packet {
            Packet::PubRel(pubrel) => assert_eq!(pubrel.pkid, 1),
            packet => panic!("Invalid network request: {:?}", packet),
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
            properties.clone(),
        );

        let packet = mqtt
            .handle_outgoing_packet(Request::Disconnect(disconnect.clone()))
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
            Err(e) => panic!("Should throw pingresp await error. Error = {:?}", e),
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
