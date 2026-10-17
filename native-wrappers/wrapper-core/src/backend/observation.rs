use crate::{
    ObservationProperties, PacketDirection, PacketKind, ProtocolObservation, ProtocolVersion,
    WrapperEvent,
};
use std::sync::Arc;

pub(super) fn outgoing_v4(
    outgoing: &rumqttc_v4::Outgoing,
    generation: Option<u64>,
) -> Option<Arc<ProtocolObservation>> {
    use rumqttc_v4::Outgoing as O;
    let (kind, id) = match outgoing {
        O::Publish(id) => (PacketKind::Publish, (*id != 0).then_some(*id)),
        O::Subscribe(id) => (PacketKind::Subscribe, (*id != 0).then_some(*id)),
        O::Unsubscribe(id) => (PacketKind::Unsubscribe, (*id != 0).then_some(*id)),
        O::PubAck(id) => (PacketKind::PubAck, (*id != 0).then_some(*id)),
        O::PubRec(id) => (PacketKind::PubRec, (*id != 0).then_some(*id)),
        O::PubRel(id) => (PacketKind::PubRel, (*id != 0).then_some(*id)),
        O::PubComp(id) => (PacketKind::PubComp, (*id != 0).then_some(*id)),
        O::PingReq => (PacketKind::PingReq, None),
        O::PingResp => (PacketKind::PingResp, None),
        O::Disconnect => (PacketKind::Disconnect, None),
        O::AwaitAck(_) => return None,
    };
    Some(Arc::new(ProtocolObservation::new(
        ProtocolVersion::V4,
        PacketDirection::Outgoing,
        kind,
        id,
        generation,
    )))
}

pub(super) fn incoming_v4(
    packet: rumqttc_v4::Packet,
    generation: Option<u64>,
) -> Option<WrapperEvent> {
    use rumqttc_v4::Packet as P;
    let mut value = ProtocolObservation::new(
        ProtocolVersion::V4,
        PacketDirection::Incoming,
        PacketKind::PingResp,
        None,
        generation,
    );
    match packet {
        P::PingResp => {}
        P::PubAck(ack) => {
            value.kind = PacketKind::PubAck;
            value.packet_id = Some(ack.pkid);
        }
        P::PubRec(ack) => {
            value.kind = PacketKind::PubRec;
            value.packet_id = Some(ack.pkid);
        }
        P::PubRel(ack) => {
            value.kind = PacketKind::PubRel;
            value.packet_id = Some(ack.pkid);
        }
        P::PubComp(ack) => {
            value.kind = PacketKind::PubComp;
            value.packet_id = Some(ack.pkid);
        }
        P::SubAck(ack) => {
            value.kind = PacketKind::SubAck;
            value.packet_id = Some(ack.pkid);
            value.filter_reason_codes = Some(
                ack.return_codes
                    .into_iter()
                    .map(|reason| match reason {
                        rumqttc_v4::SubscribeReasonCode::Success(qos) => qos as u8,
                        rumqttc_v4::SubscribeReasonCode::Failure => 0x80,
                    })
                    .collect(),
            );
        }
        P::UnsubAck(ack) => {
            value.kind = PacketKind::UnsubAck;
            value.packet_id = Some(ack.pkid);
        }
        _ => return None,
    }
    Some(crate::observation::event(value))
}

pub(super) fn outgoing_v5(
    outgoing: &rumqttc_v5::Outgoing,
    generation: Option<u64>,
) -> Option<Arc<ProtocolObservation>> {
    use rumqttc_v5::Outgoing as O;
    let (kind, id) = match outgoing {
        O::Publish(id) => (PacketKind::Publish, (*id != 0).then_some(*id)),
        O::Subscribe(id) => (PacketKind::Subscribe, (*id != 0).then_some(*id)),
        O::Unsubscribe(id) => (PacketKind::Unsubscribe, (*id != 0).then_some(*id)),
        O::PubAck(id) => (PacketKind::PubAck, (*id != 0).then_some(*id)),
        O::PubRec(id) => (PacketKind::PubRec, (*id != 0).then_some(*id)),
        O::PubRel(id) => (PacketKind::PubRel, (*id != 0).then_some(*id)),
        O::PubComp(id) => (PacketKind::PubComp, (*id != 0).then_some(*id)),
        O::PingReq => (PacketKind::PingReq, None),
        O::PingResp => (PacketKind::PingResp, None),
        O::Disconnect => (PacketKind::Disconnect, None),
        O::Auth => (PacketKind::Auth, None),
        O::AwaitAck(_) => return None,
    };
    Some(Arc::new(ProtocolObservation::new(
        ProtocolVersion::V5,
        PacketDirection::Outgoing,
        kind,
        id,
        generation,
    )))
}

pub(super) fn incoming_v5(
    packet: rumqttc_v5::Packet,
    generation: Option<u64>,
) -> Option<WrapperEvent> {
    use rumqttc_v5::Packet as P;
    let mut value = ProtocolObservation::new(
        ProtocolVersion::V5,
        PacketDirection::Incoming,
        PacketKind::PingResp,
        None,
        generation,
    );
    match packet {
        P::PingResp => {}
        P::PubAck(ack) => {
            value.kind = PacketKind::PubAck;
            value.packet_id = Some(ack.pkid);
            value.reason_code = Some(ack.reason as u8);
            value.properties_available = true;
            value.properties = ack.properties.map(|p| ObservationProperties {
                reason_string: p.reason_string,
                user_properties: p.user_properties,
                ..ObservationProperties::default()
            });
        }
        P::PubRec(ack) => {
            value.kind = PacketKind::PubRec;
            value.packet_id = Some(ack.pkid);
            value.reason_code = Some(ack.reason as u8);
            value.properties_available = true;
            value.properties = ack.properties.map(|p| ObservationProperties {
                reason_string: p.reason_string,
                user_properties: p.user_properties,
                ..ObservationProperties::default()
            });
        }
        P::PubRel(ack) => {
            value.kind = PacketKind::PubRel;
            value.packet_id = Some(ack.pkid);
            value.reason_code = Some(ack.reason as u8);
            value.properties_available = true;
            value.properties = ack.properties.map(|p| ObservationProperties {
                reason_string: p.reason_string,
                user_properties: p.user_properties,
                ..ObservationProperties::default()
            });
        }
        P::PubComp(ack) => {
            value.kind = PacketKind::PubComp;
            value.packet_id = Some(ack.pkid);
            value.reason_code = Some(ack.reason as u8);
            value.properties_available = true;
            value.properties = ack.properties.map(|p| ObservationProperties {
                reason_string: p.reason_string,
                user_properties: p.user_properties,
                ..ObservationProperties::default()
            });
        }
        P::SubAck(ack) => {
            value.kind = PacketKind::SubAck;
            value.packet_id = Some(ack.pkid);
            value.filter_reason_codes = Some(
                ack.return_codes
                    .into_iter()
                    .map(super::v5::v5_suback_code)
                    .collect(),
            );
            value.properties_available = true;
            value.properties = ack.properties.map(|p| ObservationProperties {
                reason_string: p.reason_string,
                user_properties: p.user_properties,
                ..ObservationProperties::default()
            });
        }
        P::UnsubAck(ack) => {
            value.kind = PacketKind::UnsubAck;
            value.packet_id = Some(ack.pkid);
            value.filter_reason_codes =
                Some(ack.reasons.into_iter().map(|reason| reason as u8).collect());
            value.properties_available = true;
            value.properties = ack.properties.map(|p| ObservationProperties {
                reason_string: p.reason_string,
                user_properties: p.user_properties,
                ..ObservationProperties::default()
            });
        }
        _ => return None,
    }
    Some(crate::observation::event(value))
}
pub(super) fn raw_auth(
    reason: u8,
    properties: Option<&crate::AuthProperties>,
    generation: Option<u64>,
) -> WrapperEvent {
    let mut value = ProtocolObservation::new(
        ProtocolVersion::V5,
        PacketDirection::Incoming,
        PacketKind::Auth,
        None,
        generation,
    );
    value.reason_code = Some(reason);
    value.properties_available = true;
    value.properties = properties.map(ObservationProperties::from);
    crate::observation::event(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PacketDirection, PacketKind};

    #[test]
    fn outgoing_kinds_are_exact_and_contents_are_unavailable() {
        let v4 = [
            rumqttc_v4::Outgoing::Publish(0),
            rumqttc_v4::Outgoing::Subscribe(1),
            rumqttc_v4::Outgoing::Unsubscribe(2),
            rumqttc_v4::Outgoing::PubAck(3),
            rumqttc_v4::Outgoing::PubRec(4),
            rumqttc_v4::Outgoing::PubRel(5),
            rumqttc_v4::Outgoing::PubComp(6),
            rumqttc_v4::Outgoing::PingReq,
            rumqttc_v4::Outgoing::PingResp,
            rumqttc_v4::Outgoing::Disconnect,
        ];
        let v5 = [
            rumqttc_v5::Outgoing::Publish(0),
            rumqttc_v5::Outgoing::Subscribe(1),
            rumqttc_v5::Outgoing::Unsubscribe(2),
            rumqttc_v5::Outgoing::PubAck(3),
            rumqttc_v5::Outgoing::PubRec(4),
            rumqttc_v5::Outgoing::PubRel(5),
            rumqttc_v5::Outgoing::PubComp(6),
            rumqttc_v5::Outgoing::PingReq,
            rumqttc_v5::Outgoing::PingResp,
            rumqttc_v5::Outgoing::Disconnect,
            rumqttc_v5::Outgoing::Auth,
        ];
        let kinds = [
            PacketKind::Publish,
            PacketKind::Subscribe,
            PacketKind::Unsubscribe,
            PacketKind::PubAck,
            PacketKind::PubRec,
            PacketKind::PubRel,
            PacketKind::PubComp,
            PacketKind::PingReq,
            PacketKind::PingResp,
            PacketKind::Disconnect,
            PacketKind::Auth,
        ];
        for (value, expected) in v4
            .iter()
            .map(|v| outgoing_v4(v, Some(7)).unwrap())
            .zip(kinds)
            .chain(
                v5.iter()
                    .map(|v| outgoing_v5(v, Some(7)).unwrap())
                    .zip(kinds),
            )
        {
            assert_eq!(value.kind(), expected);
            assert_eq!(value.direction(), PacketDirection::Outgoing);
            assert_eq!(value.connection_generation(), Some(7));
            assert!(value.reason_code().is_none());
            assert!(!value.properties_available());
            assert!(value.properties().is_none());
            assert!(value.filter_reason_codes().is_none());
            if expected == PacketKind::Publish {
                assert!(value.packet_id().is_none());
            }
        }
        assert!(outgoing_v4(&rumqttc_v4::Outgoing::AwaitAck(42), None).is_none());
        assert!(outgoing_v5(&rumqttc_v5::Outgoing::AwaitAck(42), None).is_none());
    }

    #[test]
    fn intermediate_pubrec_retains_order_presence_and_redacts_properties() {
        let event = incoming_v5(
            rumqttc_v5::Packet::PubRec(rumqttc_v5::PubRec {
                pkid: 65535,
                reason: rumqttc_v5::PubRecReason::NoMatchingSubscribers,
                properties: Some(rumqttc_v5::PubRecProperties {
                    reason_string: Some(String::new()),
                    user_properties: vec![
                        ("private-key".into(), "private-value".into()),
                        ("private-key".into(), String::new()),
                    ],
                }),
            }),
            Some(2),
        )
        .unwrap();
        let value = event.protocol_observation().unwrap();
        assert_eq!(value.kind(), PacketKind::PubRec);
        assert_eq!(value.packet_id(), Some(65535));
        assert_eq!(value.reason_code(), Some(0x10));
        assert_eq!(value.properties().unwrap().reason_string(), Some(""));
        assert_eq!(value.properties().unwrap().user_properties()[1].1, "");
        assert!(!format!("{event:?}").contains("private"));
        let WrapperEvent::ProtocolObservation(owner) = event else {
            unreachable!()
        };
        let weak = Arc::downgrade(&owner);
        let cloned = Arc::clone(&owner);
        drop(owner);
        assert!(weak.upgrade().is_some());
        drop(cloned);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn raw_auth_is_an_owned_redacted_snapshot_without_authority() {
        let properties = crate::AuthProperties {
            method: Some("private-method".into()),
            data: Some(bytes::Bytes::from_static(b"private-auth-data")),
            reason_string: Some("private-auth-reason".into()),
            user_properties: vec![("private-key".into(), String::new())],
        };
        let event = raw_auth(0x18, Some(&properties), None);
        drop(properties);
        let observation = event.protocol_observation().unwrap();
        assert_eq!(observation.connection_generation(), None);
        assert_eq!(
            observation.properties().unwrap().authentication_method(),
            Some("private-method")
        );
        assert_eq!(
            observation.properties().unwrap().authentication_data(),
            Some(b"private-auth-data".as_slice())
        );
        assert_eq!(observation.reason_code(), Some(0x18));
        assert!(!format!("{event:?}").contains("private"));
        assert!(matches!(event, WrapperEvent::ProtocolObservation(_)));
    }

    #[test]
    fn protocol_specific_fields_and_existing_packet_exclusions() {
        let v4 = incoming_v4(
            rumqttc_v4::Packet::SubAck(rumqttc_v4::SubAck::new(
                8,
                vec![
                    rumqttc_v4::SubscribeReasonCode::Success(rumqttc_v4::QoS::ExactlyOnce),
                    rumqttc_v4::SubscribeReasonCode::Failure,
                ],
            )),
            Some(1),
        )
        .unwrap();
        let value = v4.protocol_observation().unwrap();
        assert_eq!(value.filter_reason_codes(), Some([2, 128].as_slice()));
        assert!(!value.properties_available());
        assert!(value.reason_code().is_none());
        let event = incoming_v5(
            rumqttc_v5::Packet::PubRel(rumqttc_v5::PubRel::new(42, None)),
            None,
        )
        .unwrap();
        let value = event.protocol_observation().unwrap();
        assert_eq!(value.kind(), PacketKind::PubRel);
        assert_eq!(value.reason_code(), Some(0));
        assert!(value.properties_available());
        assert!(value.properties().is_none());
        assert_eq!(value.connection_generation(), None);
        for event in [
            incoming_v4(rumqttc_v4::Packet::PingResp, Some(1)).unwrap(),
            incoming_v5(rumqttc_v5::Packet::PingResp, Some(1)).unwrap(),
        ] {
            let value = event.protocol_observation().unwrap();
            assert_eq!(value.kind(), PacketKind::PingResp);
            assert!(value.packet_id().is_none());
            assert!(!value.properties_available());
        }
        assert!(incoming_v4(rumqttc_v4::Packet::PingReq, None).is_none());
        assert!(incoming_v5(rumqttc_v5::Packet::PingReq, None).is_none());
    }
}
