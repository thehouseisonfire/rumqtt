use std::slice::Iter;

pub use self::{
    auth::{Auth, AuthProperties, AuthReasonCode},
    codec::Codec,
    connack::{ConnAck, ConnAckProperties, ConnectReturnCode},
    connect::{Connect, ConnectProperties, LastWill, LastWillProperties, Login},
    disconnect::{Disconnect, DisconnectProperties, DisconnectReasonCode},
    ping::{PingReq, PingResp},
    puback::{PubAck, PubAckProperties, PubAckReason},
    pubcomp::{PubComp, PubCompProperties, PubCompReason},
    publish::{Publish, PublishProperties},
    pubrec::{PubRec, PubRecProperties, PubRecReason},
    pubrel::{PubRel, PubRelProperties, PubRelReason},
    suback::{SubAck, SubAckProperties, SubscribeReasonCode},
    subscribe::{Filter, RetainForwardRule, Subscribe, SubscribeProperties},
    unsuback::{UnsubAck, UnsubAckProperties, UnsubAckReason},
    unsubscribe::{Unsubscribe, UnsubscribeProperties},
};

use super::*;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use mqttbytes_core::primitives::{self as core_primitives, Error as PrimitiveError};

mod auth;
mod codec;
mod connack;
mod connect;
mod disconnect;
mod ping;
mod puback;
mod pubcomp;
mod publish;
mod pubrec;
mod pubrel;
mod suback;
mod subscribe;
mod unsuback;
mod unsubscribe;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Packet {
    Auth(Auth),
    Connect(Connect, Option<LastWill>, Option<Login>),
    ConnAck(ConnAck),
    Publish(Publish),
    PubAck(PubAck),
    PingReq(PingReq),
    PingResp(PingResp),
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
    /// Reads a stream of bytes and extracts next MQTT packet out of it
    pub fn read(stream: &mut BytesMut, max_size: Option<u32>) -> Result<Packet, Error> {
        let fixed_header = check(stream.iter(), max_size)?;

        // Test with a stream with exactly the size to check border panics
        let packet = stream.split_to(fixed_header.frame_length());
        let packet_type = fixed_header.packet_type()?;

        if fixed_header.remaining_len == 0 {
            // no payload packets, Disconnect still has a bit more info
            return match packet_type {
                PacketType::PingReq => Ok(Packet::PingReq(PingReq)),
                PacketType::PingResp => Ok(Packet::PingResp(PingResp)),
                _ => Err(Error::PayloadRequired),
            };
        }

        let packet = packet.freeze();
        let packet = match packet_type {
            PacketType::Connect => {
                let (connect, will, login) = Connect::read(fixed_header, packet)?;
                Packet::Connect(connect, will, login)
            }
            PacketType::Publish => {
                let publish = Publish::read(fixed_header, packet)?;
                Packet::Publish(publish)
            }
            PacketType::Subscribe => {
                let subscribe = Subscribe::read(fixed_header, packet)?;
                Packet::Subscribe(subscribe)
            }
            PacketType::Unsubscribe => {
                let unsubscribe = Unsubscribe::read(fixed_header, packet)?;
                Packet::Unsubscribe(unsubscribe)
            }
            PacketType::ConnAck => {
                let connack = ConnAck::read(fixed_header, packet)?;
                Packet::ConnAck(connack)
            }
            PacketType::PubAck => {
                let puback = PubAck::read(fixed_header, packet)?;
                Packet::PubAck(puback)
            }
            PacketType::PubRec => {
                let pubrec = PubRec::read(fixed_header, packet)?;
                Packet::PubRec(pubrec)
            }
            PacketType::PubRel => {
                let pubrel = PubRel::read(fixed_header, packet)?;
                Packet::PubRel(pubrel)
            }
            PacketType::PubComp => {
                let pubcomp = PubComp::read(fixed_header, packet)?;
                Packet::PubComp(pubcomp)
            }
            PacketType::SubAck => {
                let suback = SubAck::read(fixed_header, packet)?;
                Packet::SubAck(suback)
            }
            PacketType::UnsubAck => {
                let unsuback = UnsubAck::read(fixed_header, packet)?;
                Packet::UnsubAck(unsuback)
            }
            PacketType::PingReq => Packet::PingReq(PingReq),
            PacketType::PingResp => Packet::PingResp(PingResp),
            PacketType::Disconnect => {
                let disconnect = Disconnect::read(fixed_header, packet)?;
                Packet::Disconnect(disconnect)
            }
            PacketType::Auth => {
                let auth = Auth::read(fixed_header, packet)?;
                Packet::Auth(auth)
            }
        };

        Ok(packet)
    }

    pub fn write(&self, write: &mut BytesMut, max_size: Option<u32>) -> Result<usize, Error> {
        if let Some(max_size) = max_size {
            if self.size() > max_size as usize {
                return Err(Error::OutgoingPacketTooLarge {
                    pkt_size: u32::try_from(self.size()).unwrap_or(u32::MAX),
                    max: max_size,
                });
            }
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
            Self::Connect(connect, will, login) => connect.write(will, login, write),
            Self::PingReq(_) => PingReq::write(write),
            Self::PingResp(_) => PingResp::write(write),
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
            Self::Connect(connect, will, login) => connect.size(will, login),
            Self::PingReq(req) => req.size(),
            Self::PingResp(resp) => resp.size(),
            Self::Disconnect(disconnect) => disconnect.size(),
        }
    }
}

/// MQTT packet type
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    Connect = 1,
    ConnAck,
    Publish,
    PubAck,
    PubRec,
    PubRel,
    PubComp,
    Subscribe,
    SubAck,
    Unsubscribe,
    UnsubAck,
    PingReq,
    PingResp,
    Disconnect,
    Auth,
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

/// Packet type from a byte
///
/// ```text
///          7                          3                          0
///          +--------------------------+--------------------------+
/// byte 1   | MQTT Control Packet Type | Flags for each type      |
///          +--------------------------+--------------------------+
///          |         Remaining Bytes Len  (1/2/3/4 bytes)        |
///          +-----------------------------------------------------+
///
/// <https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349207>
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
pub struct FixedHeader {
    /// First byte of the stream. Used to identify packet types and
    /// several flags
    byte1: u8,
    /// Length of fixed header. Byte 1 + (1..4) bytes. So fixed header
    /// len can vary from 2 bytes to 5 bytes
    /// 1..4 bytes are variable length encoded to represent remaining length
    fixed_header_len: usize,
    /// Remaining length of the packet. Doesn't include fixed header bytes
    /// Represents variable header + payload size
    remaining_len: usize,
}

impl FixedHeader {
    #[must_use]
    pub fn new(byte1: u8, remaining_len_len: usize, remaining_len: usize) -> FixedHeader {
        FixedHeader {
            byte1,
            fixed_header_len: remaining_len_len + 1,
            remaining_len,
        }
    }

    pub fn packet_type(&self) -> Result<PacketType, Error> {
        let num = self.byte1 >> 4;
        match num {
            1 => Ok(PacketType::Connect),
            2 => Ok(PacketType::ConnAck),
            3 => Ok(PacketType::Publish),
            4 => Ok(PacketType::PubAck),
            5 => Ok(PacketType::PubRec),
            6 => Ok(PacketType::PubRel),
            7 => Ok(PacketType::PubComp),
            8 => Ok(PacketType::Subscribe),
            9 => Ok(PacketType::SubAck),
            10 => Ok(PacketType::Unsubscribe),
            11 => Ok(PacketType::UnsubAck),
            12 => Ok(PacketType::PingReq),
            13 => Ok(PacketType::PingResp),
            14 => Ok(PacketType::Disconnect),
            15 => Ok(PacketType::Auth),
            _ => Err(Error::InvalidPacketType(num)),
        }
    }

    /// Returns the size of full packet (fixed header + variable header + payload)
    /// Fixed header is enough to get the size of a frame in the stream
    #[must_use]
    pub fn frame_length(&self) -> usize {
        self.fixed_header_len + self.remaining_len
    }
}

fn property(num: u8) -> Result<PropertyType, Error> {
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

/// Checks if the stream has enough bytes to frame a packet and returns fixed header
/// only if a packet can be framed with existing bytes in the `stream`.
/// The passed stream doesn't modify parent stream's cursor. If this function
/// returned an error, next `check` on the same parent stream is forced start
/// with cursor at 0 again (Iter is owned. Only Iter's cursor is changed internally)
pub fn check(stream: Iter<u8>, max_packet_size: Option<u32>) -> Result<FixedHeader, Error> {
    let stream_len = stream.len();
    let fixed_header = parse_fixed_header(stream)?;

    // Don't let rogue connections attack with huge payloads.
    // Disconnect them before reading all that data
    if let Some(max_size) = max_packet_size {
        if fixed_header.remaining_len > max_size as usize {
            return Err(Error::PayloadSizeLimitExceeded {
                pkt_size: fixed_header.remaining_len,
                max: max_size,
            });
        }
    }

    let frame_length = fixed_header.frame_length();
    if stream_len < frame_length {
        return Err(Error::InsufficientBytes(frame_length - stream_len));
    }

    Ok(fixed_header)
}

fn parse_fixed_header(stream: Iter<u8>) -> Result<FixedHeader, Error> {
    let fixed_header = core_primitives::parse_fixed_header(stream).map_err(map_primitive_error)?;
    Ok(FixedHeader::new(
        fixed_header.byte1,
        fixed_header.remaining_len_len,
        fixed_header.remaining_len,
    ))
}

/// Parses variable byte integer in the stream and returns the length
/// and number of bytes that make it. Used for remaining length calculation
/// as well as for calculating property lengths
fn length(stream: Iter<u8>) -> Result<(usize, usize), Error> {
    core_primitives::length(stream).map_err(map_primitive_error)
}

/// Reads a series of bytes with a length from a byte stream
fn read_mqtt_bytes(stream: &mut Bytes) -> Result<Bytes, Error> {
    core_primitives::read_mqtt_bytes(stream).map_err(map_primitive_error)
}

/// Reads a string from bytes stream
fn read_mqtt_string(stream: &mut Bytes) -> Result<String, Error> {
    core_primitives::read_mqtt_string(stream).map_err(map_primitive_error)
}

/// Serializes bytes to stream (including length)
fn write_mqtt_bytes(stream: &mut BytesMut, bytes: &[u8]) {
    core_primitives::write_mqtt_bytes(stream, bytes);
}

/// Serializes a string to stream
fn write_mqtt_string(stream: &mut BytesMut, string: &str) {
    core_primitives::write_mqtt_string(stream, string);
}

/// Writes remaining length to stream and returns number of bytes for remaining length
fn write_remaining_length(stream: &mut BytesMut, len: usize) -> Result<usize, Error> {
    core_primitives::write_remaining_length(stream, len).map_err(map_primitive_error)
}

/// Return number of remaining length bytes required for encoding length
fn len_len(len: usize) -> usize {
    core_primitives::len_len(len)
}

/// After collecting enough bytes to frame a packet (packet's frame())
/// , It's possible that content itself in the stream is wrong. Like expected
/// packet id or qos not being present. In cases where `read_mqtt_string` or
/// `read_mqtt_bytes` exhausted remaining length but packet framing expects to
/// parse qos next, these pre checks will prevent `bytes` crashes
fn read_u16(stream: &mut Bytes) -> Result<u16, Error> {
    core_primitives::read_u16(stream).map_err(map_primitive_error)
}

fn read_u8(stream: &mut Bytes) -> Result<u8, Error> {
    core_primitives::read_u8(stream).map_err(map_primitive_error)
}

fn read_u32(stream: &mut Bytes) -> Result<u32, Error> {
    core_primitives::read_u32(stream).map_err(map_primitive_error)
}

fn map_primitive_error(error: PrimitiveError) -> Error {
    match error {
        PrimitiveError::PayloadTooLong => Error::PayloadTooLong,
        PrimitiveError::BoundaryCrossed(len) => Error::BoundaryCrossed(len),
        PrimitiveError::MalformedPacket => Error::MalformedPacket,
        PrimitiveError::MalformedRemainingLength => Error::MalformedRemainingLength,
        PrimitiveError::TopicNotUtf8 => Error::TopicNotUtf8,
        PrimitiveError::InsufficientBytes(required) => Error::InsufficientBytes(required),
    }
}

mod test {
    // These are used in tests by packets
    #[allow(dead_code)]
    pub const USER_PROP_KEY: &str = "property";
    #[allow(dead_code)]
    pub const USER_PROP_VAL: &str = "a value thats really long............................................................................................................";
}

#[cfg(test)]
mod tests {
    use super::{Error, check};

    #[test]
    fn check_rejects_oversized_packet_on_partial_frame() {
        let stream = [0x30, 0x14];
        let result = check(stream.iter(), Some(10));

        assert!(matches!(
            result,
            Err(Error::PayloadSizeLimitExceeded {
                pkt_size: 20,
                max: 10,
            })
        ));
    }
}
