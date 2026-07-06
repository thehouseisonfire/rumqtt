pub use self::{
    auth::{Auth, AuthProperties, AuthReasonCode},
    codec::Codec,
    connack::{ConnAck, ConnAckProperties, ConnectReturnCode},
    connect::{Connect, ConnectAuth, ConnectProperties, LastWill, LastWillProperties},
    disconnect::{Disconnect, DisconnectProperties, DisconnectReasonCode},
    ping::{PingReq, PingResp},
    puback::{PubAck, PubAckProperties, PubAckReason},
    pubcomp::{PubComp, PubCompProperties, PubCompReason},
    publish::{Publish, PublishProperties},
    pubrec::{PubRec, PubRecProperties, PubRecReason},
    pubrel::{PubRel, PubRelProperties, PubRelReason},
    suback::{SubAck, SubAckProperties, SubscribeReasonCode},
    subscribe::{RetainForwardRule, Subscribe, SubscribeFilter, SubscribeProperties},
    unsuback::{UnsubAck, UnsubAckProperties, UnsubAckReason},
    unsubscribe::{Unsubscribe, UnsubscribeProperties},
};

use super::{
    Error, FixedHeader, PacketType, QoS, check, len_len, length, qos, read_mqtt_bytes,
    read_mqtt_string, read_u8, read_u16, read_u32, validate_mqtt_string, write_mqtt_bytes,
    write_mqtt_string, write_remaining_length,
};
use bytes::{Buf, BufMut, BytesMut};

#[expect(clippy::missing_errors_doc)]
mod auth;
#[allow(clippy::missing_errors_doc)]
mod codec;
#[expect(clippy::missing_errors_doc)]
mod connack;
#[expect(clippy::missing_errors_doc)]
mod connect;
#[expect(clippy::missing_errors_doc)]
mod disconnect;
#[expect(clippy::missing_errors_doc)]
mod ping;
#[expect(clippy::missing_errors_doc)]
mod puback;
#[expect(clippy::missing_errors_doc)]
mod pubcomp;
#[expect(clippy::missing_errors_doc)]
mod publish;
#[expect(clippy::missing_errors_doc)]
mod pubrec;
#[expect(clippy::missing_errors_doc)]
mod pubrel;
#[expect(clippy::missing_errors_doc)]
mod suback;
#[expect(clippy::missing_errors_doc)]
mod subscribe;
#[expect(clippy::missing_errors_doc)]
mod unsuback;
#[expect(clippy::missing_errors_doc)]
mod unsubscribe;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Packet {
    Auth(Auth),
    Connect(Connect, Option<LastWill>, ConnectAuth),
    ConnAck(ConnAck),
    Publish(Publish),
    PubAck(PubAck),
    PingReq,
    PingResp,
    Subscribe(Subscribe),
    SubAck(SubAck),
    PubRec(PubRec),
    PubRel(PubRel),
    PubComp(PubComp),
    Unsubscribe(Unsubscribe),
    UnsubAck(UnsubAck),
    Disconnect(Disconnect),
}

impl Packet {
    #[must_use]
    pub const fn packet_type(&self) -> PacketType {
        match self {
            Self::Auth(_) => PacketType::Auth,
            Self::Connect(_, _, _) => PacketType::Connect,
            Self::ConnAck(_) => PacketType::ConnAck,
            Self::Publish(_) => PacketType::Publish,
            Self::PubAck(_) => PacketType::PubAck,
            Self::PingReq => PacketType::PingReq,
            Self::PingResp => PacketType::PingResp,
            Self::Subscribe(_) => PacketType::Subscribe,
            Self::SubAck(_) => PacketType::SubAck,
            Self::PubRec(_) => PacketType::PubRec,
            Self::PubRel(_) => PacketType::PubRel,
            Self::PubComp(_) => PacketType::PubComp,
            Self::Unsubscribe(_) => PacketType::Unsubscribe,
            Self::UnsubAck(_) => PacketType::UnsubAck,
            Self::Disconnect(_) => PacketType::Disconnect,
        }
    }

    /// Reads the next MQTT v5 packet from the buffered stream.
    ///
    /// # Errors
    ///
    /// Returns an error if the packet is incomplete, malformed, or exceeds the
    /// configured packet-size limit.
    pub fn read(stream: &mut BytesMut, max_size: Option<u32>) -> Result<Self, Error> {
        let fixed_header = check(stream.iter(), max_size)?;

        // Test with a stream with exactly the size to check border panics
        let packet = stream.split_to(fixed_header.frame_length());
        let packet_type = fixed_header.packet_type()?;
        validate_fixed_header_flags(packet_type, fixed_header.byte1)?;

        if fixed_header.remaining_len == 0 {
            // no payload packets, Disconnect still has a bit more info
            return match packet_type {
                PacketType::PingReq => Ok(Self::PingReq),
                PacketType::PingResp => Ok(Self::PingResp),
                PacketType::Disconnect => {
                    Disconnect::read(fixed_header, packet.freeze()).map(Self::Disconnect)
                }
                _ => Err(Error::PayloadRequired),
            };
        }

        let packet = packet.freeze();
        let packet = match packet_type {
            PacketType::Connect => {
                let (connect, will, auth) = Connect::read(fixed_header, packet)?;
                Self::Connect(connect, will, auth)
            }
            PacketType::Publish => {
                let publish = Publish::read(fixed_header, packet)?;
                Self::Publish(publish)
            }
            PacketType::Subscribe => {
                let subscribe = Subscribe::read(fixed_header, packet)?;
                Self::Subscribe(subscribe)
            }
            PacketType::Unsubscribe => {
                let unsubscribe = Unsubscribe::read(fixed_header, packet)?;
                Self::Unsubscribe(unsubscribe)
            }
            PacketType::ConnAck => {
                let connack = ConnAck::read(fixed_header, packet)?;
                Self::ConnAck(connack)
            }
            PacketType::PubAck => {
                let puback = PubAck::read(fixed_header, packet)?;
                Self::PubAck(puback)
            }
            PacketType::PubRec => {
                let pubrec = PubRec::read(fixed_header, packet)?;
                Self::PubRec(pubrec)
            }
            PacketType::PubRel => {
                let pubrel = PubRel::read(fixed_header, packet)?;
                Self::PubRel(pubrel)
            }
            PacketType::PubComp => {
                let pubcomp = PubComp::read(fixed_header, packet)?;
                Self::PubComp(pubcomp)
            }
            PacketType::SubAck => {
                let suback = SubAck::read(fixed_header, packet)?;
                Self::SubAck(suback)
            }
            PacketType::UnsubAck => {
                let unsuback = UnsubAck::read(fixed_header, packet)?;
                Self::UnsubAck(unsuback)
            }
            PacketType::PingReq => Self::PingReq,
            PacketType::PingResp => Self::PingResp,
            PacketType::Disconnect => {
                let disconnect = Disconnect::read(fixed_header, packet)?;
                Self::Disconnect(disconnect)
            }
            PacketType::Auth => {
                let auth = Auth::read(fixed_header, packet)?;
                Self::Auth(auth)
            }
        };

        Ok(packet)
    }

    /// Serializes this MQTT v5 packet into the output buffer.
    ///
    /// # Errors
    ///
    /// Returns an error if the packet cannot be encoded within the configured
    /// packet-size limit or violates MQTT encoding rules.
    pub fn write(&self, write: &mut BytesMut, max_size: Option<u32>) -> Result<usize, Error> {
        if let Some(max_size) = max_size
            && self.size() > max_size as usize
        {
            return Err(Error::OutgoingPacketTooLarge {
                pkt_size: u32::try_from(self.size()).unwrap_or(u32::MAX),
                max: max_size,
            });
        }

        match self {
            Self::Auth(auth) => auth.write(write),
            Self::Publish(publish) => publish.write(write),
            Self::Subscribe(subscription) => subscription.write(write),
            Self::Unsubscribe(unsubscribe) => unsubscribe.write(write),
            Self::ConnAck(ack) => ack.write(write),
            Self::PubAck(ack) => ack.write(write),
            Self::SubAck(ack) => ack.write(write),
            Self::UnsubAck(unsuback) => unsuback.write(write),
            Self::PubRec(pubrec) => pubrec.write(write),
            Self::PubRel(pubrel) => pubrel.write(write),
            Self::PubComp(pubcomp) => pubcomp.write(write),
            Self::Connect(connect, will, auth) => connect.write(will, auth, write),
            Self::PingReq => PingReq::write(write),
            Self::PingResp => PingResp::write(write),
            Self::Disconnect(disconnect) => disconnect.write(write),
        }
    }

    pub fn size(&self) -> usize {
        match self {
            Self::Auth(auth) => auth.size(),
            Self::Publish(publish) => publish.size(),
            Self::Subscribe(subscription) => subscription.size(),
            Self::Unsubscribe(unsubscribe) => unsubscribe.size(),
            Self::ConnAck(ack) => ack.size(),
            Self::PubAck(ack) => ack.size(),
            Self::SubAck(ack) => ack.size(),
            Self::UnsubAck(unsuback) => unsuback.size(),
            Self::PubRec(pubrec) => pubrec.size(),
            Self::PubRel(pubrel) => pubrel.size(),
            Self::PubComp(pubcomp) => pubcomp.size(),
            Self::Connect(connect, will, auth) => connect.size(will, auth),
            Self::PingReq => PingReq.size(),
            Self::PingResp => PingResp.size(),
            Self::Disconnect(disconnect) => disconnect.size(),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PropertyType {
    PayloadFormatIndicator = 1,
    MessageExpiryInterval = 2,
    ContentType = 3,
    ResponseTopic = 8,
    CorrelationData = 9,
    SubscriptionIdentifier = 11,
    SessionExpiryInterval = 17,
    AssignedClientIdentifier = 18,
    ServerKeepAlive = 19,
    AuthenticationMethod = 21,
    AuthenticationData = 22,
    RequestProblemInformation = 23,
    WillDelayInterval = 24,
    RequestResponseInformation = 25,
    ResponseInformation = 26,
    ServerReference = 28,
    ReasonString = 31,
    ReceiveMaximum = 33,
    TopicAliasMaximum = 34,
    TopicAlias = 35,
    MaximumQos = 36,
    RetainAvailable = 37,
    UserProperty = 38,
    MaximumPacketSize = 39,
    WildcardSubscriptionAvailable = 40,
    SubscriptionIdentifierAvailable = 41,
    SharedSubscriptionAvailable = 42,
}

const fn property(num: u8) -> Result<PropertyType, Error> {
    let property = match num {
        1 => PropertyType::PayloadFormatIndicator,
        2 => PropertyType::MessageExpiryInterval,
        3 => PropertyType::ContentType,
        8 => PropertyType::ResponseTopic,
        9 => PropertyType::CorrelationData,
        11 => PropertyType::SubscriptionIdentifier,
        17 => PropertyType::SessionExpiryInterval,
        18 => PropertyType::AssignedClientIdentifier,
        19 => PropertyType::ServerKeepAlive,
        21 => PropertyType::AuthenticationMethod,
        22 => PropertyType::AuthenticationData,
        23 => PropertyType::RequestProblemInformation,
        24 => PropertyType::WillDelayInterval,
        25 => PropertyType::RequestResponseInformation,
        26 => PropertyType::ResponseInformation,
        28 => PropertyType::ServerReference,
        31 => PropertyType::ReasonString,
        33 => PropertyType::ReceiveMaximum,
        34 => PropertyType::TopicAliasMaximum,
        35 => PropertyType::TopicAlias,
        36 => PropertyType::MaximumQos,
        37 => PropertyType::RetainAvailable,
        38 => PropertyType::UserProperty,
        39 => PropertyType::MaximumPacketSize,
        40 => PropertyType::WildcardSubscriptionAvailable,
        41 => PropertyType::SubscriptionIdentifierAvailable,
        42 => PropertyType::SharedSubscriptionAvailable,
        num => return Err(Error::InvalidPropertyType(num)),
    };

    Ok(property)
}

/// Validates that the fixed-header flag bits match the expected values for
/// the given packet type, as required by [MQTT-2.1.3-1].
///
/// | Packet type        | Expected flags |
/// |-------------------|---------------|
/// | PUBLISH           | any (flags carry meaning) |
/// | PUBREL            | 0b0010 |
/// | SUBSCRIBE         | 0b0010 |
/// | UNSUBSCRIBE       | 0b0010 |
/// | All others        | 0b0000 |
///
/// [MQTT-2.1.3-1]: https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc353481062
const fn validate_fixed_header_flags(packet_type: PacketType, byte1: u8) -> Result<(), Error> {
    let flags = byte1 & 0x0F;
    let valid = match packet_type {
        PacketType::Publish => true,
        PacketType::PubRel | PacketType::Subscribe | PacketType::Unsubscribe => flags == 0b0010,
        PacketType::Connect
        | PacketType::ConnAck
        | PacketType::PubAck
        | PacketType::PubRec
        | PacketType::PubComp
        | PacketType::SubAck
        | PacketType::UnsubAck
        | PacketType::PingReq
        | PacketType::PingResp
        | PacketType::Disconnect
        | PacketType::Auth => flags == 0,
    };

    if valid {
        Ok(())
    } else {
        Err(Error::IncorrectPacketFormat)
    }
}

mod test {
    // These are used in tests by packets
    #[expect(dead_code)]
    pub const USER_PROP_KEY: &str = "property";
    #[expect(dead_code)]
    pub const USER_PROP_VAL: &str = "a value thats really long............................................................................................................";
}

#[cfg(test)]
mod tests {
    use bytes::{Bytes, BytesMut};

    use super::{
        Auth, AuthReasonCode, ConnAck, Connect, ConnectAuth, ConnectProperties, ConnectReturnCode,
        Disconnect, DisconnectReasonCode, Error, Packet, PacketType, PubAck, PubAckReason, PubComp,
        PubCompReason, PubRec, PubRecReason, PubRel, PubRelReason, Publish, QoS, SubAck, Subscribe,
        SubscribeFilter, SubscribeReasonCode, UnsubAck, UnsubAckReason, Unsubscribe,
        validate_fixed_header_flags,
    };

    fn assert_packet_bytes_round_trip(packet: Packet, expected: &[u8]) {
        let mut encoded = BytesMut::new();
        packet.write(&mut encoded, None).unwrap();

        assert_eq!(&encoded[..], expected);

        let decoded = Packet::read(&mut encoded, None).unwrap();
        assert_eq!(decoded, packet);
        assert!(encoded.is_empty());
    }

    #[test]
    fn property_bearing_packets_emit_and_read_explicit_zero_property_lengths() {
        assert_packet_bytes_round_trip(
            Packet::Connect(
                Connect {
                    keep_alive: 60,
                    client_id: "c".into(),
                    clean_start: true,
                    properties: None,
                },
                None,
                ConnectAuth::None,
            ),
            &[
                0x10, 0x0E, 0x00, 0x04, b'M', b'Q', b'T', b'T', 0x05, 0x02, 0x00, 0x3C, 0x00, 0x00,
                0x01, b'c',
            ],
        );
        assert_packet_bytes_round_trip(
            Packet::ConnAck(ConnAck {
                session_present: false,
                code: ConnectReturnCode::Success,
                properties: None,
            }),
            &[0x20, 0x03, 0x00, 0x00, 0x00],
        );
        assert_packet_bytes_round_trip(
            Packet::Publish(Publish::new("a", QoS::AtMostOnce, Bytes::new(), None)),
            &[0x30, 0x04, 0x00, 0x01, b'a', 0x00],
        );

        let mut puback = PubAck::new(1, None);
        puback.reason = PubAckReason::NoMatchingSubscribers;
        assert_packet_bytes_round_trip(
            Packet::PubAck(puback),
            &[0x40, 0x04, 0x00, 0x01, 0x10, 0x00],
        );

        let mut pubrec = PubRec::new(1, None);
        pubrec.reason = PubRecReason::NoMatchingSubscribers;
        assert_packet_bytes_round_trip(
            Packet::PubRec(pubrec),
            &[0x50, 0x04, 0x00, 0x01, 0x10, 0x00],
        );

        let mut pubrel = PubRel::new(1, None);
        pubrel.reason = PubRelReason::PacketIdentifierNotFound;
        assert_packet_bytes_round_trip(
            Packet::PubRel(pubrel),
            &[0x62, 0x04, 0x00, 0x01, 0x92, 0x00],
        );

        let mut pubcomp = PubComp::new(1, None);
        pubcomp.reason = PubCompReason::PacketIdentifierNotFound;
        assert_packet_bytes_round_trip(
            Packet::PubComp(pubcomp),
            &[0x70, 0x04, 0x00, 0x01, 0x92, 0x00],
        );

        assert_packet_bytes_round_trip(
            Packet::Subscribe(Subscribe {
                pkid: 1,
                filters: vec![SubscribeFilter::new("a", QoS::AtMostOnce)],
                properties: None,
            }),
            &[0x82, 0x07, 0x00, 0x01, 0x00, 0x00, 0x01, b'a', 0x00],
        );
        assert_packet_bytes_round_trip(
            Packet::SubAck(SubAck {
                pkid: 1,
                return_codes: vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
                properties: None,
            }),
            &[0x90, 0x04, 0x00, 0x01, 0x00, 0x00],
        );
        assert_packet_bytes_round_trip(
            Packet::Unsubscribe(Unsubscribe {
                pkid: 1,
                ..Unsubscribe::new("a", None)
            }),
            &[0xA2, 0x06, 0x00, 0x01, 0x00, 0x00, 0x01, b'a'],
        );
        assert_packet_bytes_round_trip(
            Packet::UnsubAck(UnsubAck {
                pkid: 1,
                reasons: vec![UnsubAckReason::Success],
                properties: None,
            }),
            &[0xB0, 0x04, 0x00, 0x01, 0x00, 0x00],
        );

        let mut disconnect = Disconnect::new(DisconnectReasonCode::UnspecifiedError);
        disconnect.properties = None;
        assert_packet_bytes_round_trip(Packet::Disconnect(disconnect), &[0xE0, 0x02, 0x80, 0x00]);
        assert_packet_bytes_round_trip(
            Packet::Auth(Auth::new(AuthReasonCode::Continue, None)),
            &[0xF0, 0x02, 0x18, 0x00],
        );
    }

    #[test]
    fn non_zero_property_length_is_vbi_encoded_and_excludes_its_own_bytes() {
        let props = ConnectProperties {
            session_expiry_interval: Some(300),
            ..ConnectProperties::new()
        };
        let packet = Packet::Connect(
            Connect {
                keep_alive: 60,
                client_id: "c".into(),
                clean_start: true,
                properties: Some(props),
            },
            None,
            ConnectAuth::None,
        );

        let mut encoded = BytesMut::new();
        packet.write(&mut encoded, None).unwrap();

        // CONNECT byte layout: fixed header (1) + remaining length VBI (1) + protocol name
        // length (2) + "MQTT" (4) + protocol version (1) + connect flags (1) + keep alive (2)
        // = Property Length VBI at offset 12.
        let prop_len_offset = 12;
        assert_eq!(
            encoded[prop_len_offset], 5,
            "Property Length VBI should be 5"
        );

        let prop_data_start = prop_len_offset + 1;
        assert_eq!(
            encoded[prop_data_start], 0x11,
            "first property byte should be SessionExpiryInterval identifier (0x11)"
        );

        let expected: &[u8] = &[
            0x10, 0x13, 0x00, 0x04, b'M', b'Q', b'T', b'T', 0x05, 0x02, 0x00, 0x3C, 0x05, 0x11,
            0x00, 0x00, 0x01, 0x2C, 0x00, 0x01, b'c',
        ];
        assert_eq!(&encoded[..], expected);

        let decoded = Packet::read(&mut encoded, None).unwrap();
        assert_eq!(
            decoded,
            Packet::Connect(
                Connect {
                    keep_alive: 60,
                    client_id: "c".into(),
                    clean_start: true,
                    properties: Some(ConnectProperties {
                        session_expiry_interval: Some(300),
                        ..ConnectProperties::new()
                    }),
                },
                None,
                ConnectAuth::None,
            )
        );
    }

    #[test]
    fn property_length_uses_multi_byte_vbi_when_property_data_exceeds_127_bytes() {
        // Each UserProperty("012345678901234", "012345678901234") contributes
        // 1 + 2 + 15 + 2 + 15 = 35 bytes. Four of them total 140 bytes.
        // Add session_expiry_interval (1 + 4 = 5) and receive_maximum (1 + 2 = 3)
        // for a grand total of 140 + 5 + 3 = 148 bytes of property data.
        // 148 >= 128 requires a 2-byte VBI: (148 & 0x7F) | 0x80 = 0x94, 148 >> 7 = 0x01.
        let k = "012345678901234";
        let v = "012345678901234";
        let props = ConnectProperties {
            session_expiry_interval: Some(300),
            receive_maximum: Some(20),
            user_properties: vec![
                (k.to_owned(), v.to_owned()),
                (k.to_owned(), v.to_owned()),
                (k.to_owned(), v.to_owned()),
                (k.to_owned(), v.to_owned()),
            ],
            ..ConnectProperties::new()
        };
        let packet = Packet::Connect(
            Connect {
                keep_alive: 60,
                client_id: "c".into(),
                clean_start: true,
                properties: Some(props),
            },
            None,
            ConnectAuth::None,
        );

        let mut encoded = BytesMut::new();
        packet.write(&mut encoded, None).unwrap();

        // Remaining length is 163, which needs 2-byte VBI encoding.
        // CONNECT byte layout: fixed header (1) + remaining length VBI (2) + protocol name
        // length (2) + "MQTT" (4) + protocol version (1) + connect flags (1) + keep alive (2)
        // = Property Length VBI at offset 13.
        let prop_len_offset = 13;
        let vbi_byte_0 = encoded[prop_len_offset] as usize;
        let vbi_byte_1 = encoded[prop_len_offset + 1] as usize;
        assert_ne!(
            vbi_byte_0 & 0x80,
            0,
            "first VBI byte should have continuation bit set for property data >= 128"
        );
        let decoded_prop_len = (vbi_byte_0 & 0x7F) | ((vbi_byte_1 & 0x7F) << 7);
        assert_eq!(decoded_prop_len, 148, "Property Length should be 148");

        let vbi_len = 2;
        let prop_data_start = prop_len_offset + vbi_len;
        assert_eq!(
            encoded[prop_data_start], 0x11,
            "first property byte should be SessionExpiryInterval identifier (0x11)"
        );

        let expected: &[u8] = &[
            // fixed header + remaining length VBI (163 = [0xA3, 0x01])
            0x10, 0xA3, 0x01, // protocol name: length + "MQTT"
            0x00, 0x04, b'M', b'Q', b'T', b'T',
            // protocol version + connect flags + keep alive
            0x05, 0x02, 0x00, 0x3C, // Property Length VBI (148 = 0x94, 0x01)
            0x94, 0x01, // SessionExpiryInterval: id(0x11) + u32(300 = 0x0000012C)
            0x11, 0x00, 0x00, 0x01, 0x2C, // ReceiveMaximum: id(0x21) + u16(20 = 0x0014)
            0x21, 0x00, 0x14, // UserProperty 1: id(0x26) + key + value
            0x26, 0x00, 0x0F, b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0',
            b'1', b'2', b'3', b'4', 0x00, 0x0F, b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7',
            b'8', b'9', b'0', b'1', b'2', b'3', b'4', // UserProperty 2
            0x26, 0x00, 0x0F, b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0',
            b'1', b'2', b'3', b'4', 0x00, 0x0F, b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7',
            b'8', b'9', b'0', b'1', b'2', b'3', b'4', // UserProperty 3
            0x26, 0x00, 0x0F, b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0',
            b'1', b'2', b'3', b'4', 0x00, 0x0F, b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7',
            b'8', b'9', b'0', b'1', b'2', b'3', b'4', // UserProperty 4
            0x26, 0x00, 0x0F, b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0',
            b'1', b'2', b'3', b'4', 0x00, 0x0F, b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7',
            b'8', b'9', b'0', b'1', b'2', b'3', b'4', // client ID: length + "c"
            0x00, 0x01, b'c',
        ];
        assert_eq!(&encoded[..], expected);

        let decoded = Packet::read(&mut encoded, None).unwrap();
        assert_eq!(
            decoded,
            Packet::Connect(
                Connect {
                    keep_alive: 60,
                    client_id: "c".into(),
                    clean_start: true,
                    properties: Some(ConnectProperties {
                        session_expiry_interval: Some(300),
                        receive_maximum: Some(20),
                        user_properties: vec![
                            (k.to_owned(), v.to_owned()),
                            (k.to_owned(), v.to_owned()),
                            (k.to_owned(), v.to_owned()),
                            (k.to_owned(), v.to_owned()),
                        ],
                        ..ConnectProperties::new()
                    }),
                },
                None,
                ConnectAuth::None,
            )
        );
    }

    #[test]
    fn check_rejects_oversized_packet_on_partial_frame() {
        let stream = [0x30, 0x14];
        let result = super::check(stream.iter(), Some(10));

        assert!(matches!(
            result,
            Err(Error::PayloadSizeLimitExceeded {
                pkt_size: 22,
                max: 10,
            })
        ));
    }

    #[test]
    fn check_rejects_when_total_packet_size_exceeds_limit() {
        let stream = [0x30, 0x09];
        let result = super::check(stream.iter(), Some(10));

        assert!(matches!(
            result,
            Err(Error::PayloadSizeLimitExceeded {
                pkt_size: 11,
                max: 10,
            })
        ));
    }

    // MQTT-3.1.2-24: a packet whose total size equals the Maximum Packet Size
    // (fixed-header bytes included) is legal. The limit is exclusive, so the
    // exact boundary MUST be accepted to avoid silently dropping valid frames.
    #[test]
    fn check_accepts_packet_exactly_at_limit() {
        // PUBLISH QoS0: fixed header 0x30 + remaining length 0x08 = total size 10.
        let stream = [0x30, 0x08, 0x00, 0x01, b'a', 0x00, 0x42, 0x00, 0x00, 0x00];
        let result = super::check(stream.iter(), Some(10));

        assert!(result.is_ok());
        assert_eq!(result.unwrap().frame_length(), 10);
    }

    // MQTT-2.1.3-1: Reserved flag bits MUST be set to the required value.
    // For most packet types the lower 4 bits of byte 1 must be 0.
    // SUBSCRIBE, UNSUBSCRIBE, and PUBREL require flags == 0b0010.
    // PUBLISH uses the flag bits for DUP/QoS/RETAIN, so any value is valid.

    #[test]
    fn read_rejects_connect_with_nonzero_reserved_flags() {
        // CONNECT is type 1 (0x10), flags must be 0. 0x11 has flags=1.
        let mut stream = BytesMut::from(&[0x11, 0x00][..]);
        let result = Packet::read(&mut stream, Some(1024));
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_rejects_connack_with_nonzero_reserved_flags() {
        // CONNACK is type 2 (0x20), flags must be 0. 0x2F has flags=0xF.
        let mut stream = BytesMut::from(&[0x2F, 0x03, 0x00, 0x00, 0x00][..]);
        let result = Packet::read(&mut stream, Some(1024));
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_rejects_puback_with_nonzero_reserved_flags() {
        // PUBACK is type 4 (0x40), flags must be 0. 0x41 has flags=1.
        let mut stream = BytesMut::from(&[0x41, 0x02, 0x00, 0x01][..]);
        let result = Packet::read(&mut stream, Some(1024));
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_rejects_pubrec_with_nonzero_reserved_flags() {
        // PUBREC is type 5 (0x50), flags must be 0. 0x51 has flags=1.
        let mut stream = BytesMut::from(&[0x51, 0x02, 0x00, 0x01][..]);
        let result = Packet::read(&mut stream, Some(1024));
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_rejects_pubrel_with_wrong_reserved_flags() {
        // PUBREL is type 6 (0x62), flags must be 0b0010. 0x60 has flags=0.
        let mut stream = BytesMut::from(&[0x60, 0x02, 0x00, 0x01][..]);
        let result = Packet::read(&mut stream, Some(1024));
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_rejects_pubcomp_with_nonzero_reserved_flags() {
        // PUBCOMP is type 7 (0x70), flags must be 0. 0x71 has flags=1.
        let mut stream = BytesMut::from(&[0x71, 0x02, 0x00, 0x01][..]);
        let result = Packet::read(&mut stream, Some(1024));
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_rejects_subscribe_with_wrong_reserved_flags() {
        // SUBSCRIBE is type 8 (0x82), flags must be 0b0010. 0x80 has flags=0.
        let mut stream = BytesMut::from(&[0x80, 0x02, 0x00, 0x01][..]);
        let result = Packet::read(&mut stream, Some(1024));
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_rejects_suback_with_nonzero_reserved_flags() {
        // SUBACK is type 9 (0x90), flags must be 0. 0x91 has flags=1.
        let mut stream = BytesMut::from(&[0x91, 0x03, 0x00, 0x01, 0x00][..]);
        let result = Packet::read(&mut stream, Some(1024));
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_rejects_unsubscribe_with_wrong_reserved_flags() {
        // UNSUBSCRIBE is type 10 (0xA2), flags must be 0b0010. 0xA0 has flags=0.
        let mut stream = BytesMut::from(&[0xA0, 0x02, 0x00, 0x01][..]);
        let result = Packet::read(&mut stream, Some(1024));
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_rejects_unsuback_with_nonzero_reserved_flags() {
        // UNSUBACK is type 11 (0xB0), flags must be 0. 0xB1 has flags=1.
        let mut stream = BytesMut::from(&[0xB1, 0x03, 0x00, 0x01, 0x00][..]);
        let result = Packet::read(&mut stream, Some(1024));
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_rejects_pingreq_with_nonzero_reserved_flags() {
        // PINGREQ is type 12 (0xC0), flags must be 0. 0xC1 has flags=1.
        let mut stream = BytesMut::from(&[0xC1, 0x00][..]);
        let result = Packet::read(&mut stream, Some(1024));
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_rejects_pingresp_with_nonzero_reserved_flags() {
        // PINGRESP is type 13 (0xD0), flags must be 0. 0xD1 has flags=1.
        let mut stream = BytesMut::from(&[0xD1, 0x00][..]);
        let result = Packet::read(&mut stream, Some(1024));
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_rejects_disconnect_with_nonzero_reserved_flags() {
        // DISCONNECT is type 14 (0xE0), flags must be 0. 0xE1 has flags=1.
        let mut stream = BytesMut::from(&[0xE1, 0x00][..]);
        let result = Packet::read(&mut stream, Some(1024));
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_rejects_auth_with_nonzero_reserved_flags() {
        // AUTH is type 15 (0xF0), flags must be 0. 0xF1 has flags=1.
        let mut stream = BytesMut::from(&[0xF1, 0x00][..]);
        let result = Packet::read(&mut stream, Some(1024));
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_accepts_publish_with_any_flag_combination() {
        // PUBLISH uses flag bits for DUP/QoS/RETAIN, so all combinations are valid
        // at the fixed-header level. Only QoS 3 is rejected later by Publish::read.
        // 0x3F = PUBLISH with all flag bits set (QoS 3, DUP=1, RETAIN=1).
        // This should pass flag validation but fail in Publish::read with MalformedPacket.
        let mut stream = BytesMut::from(&[0x3F, 0x02, 0x00, 0x01][..]);
        let result = Packet::read(&mut stream, Some(1024));
        // Flag validation passes; Publish::read rejects QoS 3
        assert!(matches!(result, Err(Error::MalformedPacket)));
    }

    #[test]
    fn validate_flags_const_fn_works_for_all_types() {
        // Smoke test that the const fn compiles and returns correct results
        assert!(validate_fixed_header_flags(PacketType::Publish, 0x3F).is_ok());
        assert!(validate_fixed_header_flags(PacketType::PubRel, 0x62).is_ok());
        assert!(validate_fixed_header_flags(PacketType::Subscribe, 0x82).is_ok());
        assert!(validate_fixed_header_flags(PacketType::Unsubscribe, 0xA2).is_ok());
        assert!(validate_fixed_header_flags(PacketType::Connect, 0x10).is_ok());
        assert!(validate_fixed_header_flags(PacketType::ConnAck, 0x20).is_ok());
        assert!(validate_fixed_header_flags(PacketType::Disconnect, 0xE0).is_ok());
        assert!(validate_fixed_header_flags(PacketType::Auth, 0xF0).is_ok());

        assert!(validate_fixed_header_flags(PacketType::PubRel, 0x60).is_err());
        assert!(validate_fixed_header_flags(PacketType::Subscribe, 0x80).is_err());
        assert!(validate_fixed_header_flags(PacketType::ConnAck, 0x2F).is_err());
        assert!(validate_fixed_header_flags(PacketType::Disconnect, 0xE1).is_err());
    }
}
