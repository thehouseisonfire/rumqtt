use super::{
    BufMut, BytesMut, Error, FixedHeader, PacketType, Protocol, QoS, check, fmt, qos,
    read_mqtt_bytes, read_mqtt_string, read_u8, read_u16, write_mqtt_bytes, write_mqtt_string,
    write_remaining_length,
};

#[allow(clippy::missing_errors_doc)]
mod codec;
#[allow(clippy::missing_errors_doc)]
mod connack;
#[allow(clippy::missing_errors_doc)]
mod connect;
#[allow(clippy::missing_errors_doc)]
mod disconnect;
#[allow(clippy::missing_errors_doc)]
mod ping;
#[allow(clippy::missing_errors_doc)]
mod puback;
#[allow(clippy::missing_errors_doc)]
mod pubcomp;
#[allow(clippy::missing_errors_doc)]
mod publish;
#[allow(clippy::missing_errors_doc)]
mod pubrec;
#[allow(clippy::missing_errors_doc)]
mod pubrel;
#[allow(clippy::missing_errors_doc)]
mod suback;
#[allow(clippy::missing_errors_doc)]
mod subscribe;
#[allow(clippy::missing_errors_doc)]
mod unsuback;
#[allow(clippy::missing_errors_doc)]
mod unsubscribe;

pub use codec::*;
pub use connack::*;
pub use connect::*;
pub use disconnect::*;
pub use ping::*;
pub use puback::*;
pub use pubcomp::*;
pub use publish::*;
pub use pubrec::*;
pub use pubrel::*;
pub use suback::*;
pub use subscribe::*;
pub use unsuback::*;
pub use unsubscribe::*;

/// Encapsulates all MQTT packet types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    Connect(Connect),
    ConnAck(ConnAck),
    Publish(Publish),
    PubAck(PubAck),
    PubRec(PubRec),
    PubRel(PubRel),
    PubComp(PubComp),
    Subscribe(Subscribe),
    SubAck(SubAck),
    Unsubscribe(Unsubscribe),
    UnsubAck(UnsubAck),
    PingReq,
    PingResp,
    Disconnect,
}

impl Packet {
    pub fn size(&self) -> usize {
        match self {
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
            Self::Connect(connect) => connect.size(),
            Self::PingReq => PingReq.size(),
            Self::PingResp => PingResp.size(),
            Self::Disconnect => Disconnect.size(),
        }
    }

    /// Reads the next MQTT v3.1.1 packet from the buffered stream.
    ///
    /// # Errors
    ///
    /// Returns an error if the packet is incomplete, malformed, or exceeds
    /// `max_size`.
    pub fn read(stream: &mut BytesMut, max_size: usize) -> Result<Self, Error> {
        let fixed_header = check(stream.iter(), max_size)?;

        // Test with a stream with exactly the size to check border panics
        let packet = stream.split_to(fixed_header.frame_length());
        let packet_type = fixed_header.packet_type()?;
        validate_fixed_header_flags(packet_type, fixed_header.byte1)?;

        if fixed_header.remaining_len == 0 {
            // no payload packets
            return match packet_type {
                PacketType::PingReq => Ok(Self::PingReq),
                PacketType::PingResp => Ok(Self::PingResp),
                PacketType::Disconnect => Ok(Self::Disconnect),
                _ => Err(Error::PayloadRequired),
            };
        }

        let packet = packet.freeze();
        let packet = match packet_type {
            PacketType::Connect => Self::Connect(Connect::read(fixed_header, packet)?),
            PacketType::ConnAck => Self::ConnAck(ConnAck::read(fixed_header, packet)?),
            PacketType::Publish => Self::Publish(Publish::read(fixed_header, packet)?),
            PacketType::PubAck => Self::PubAck(PubAck::read(fixed_header, packet)?),
            PacketType::PubRec => Self::PubRec(PubRec::read(fixed_header, packet)?),
            PacketType::PubRel => Self::PubRel(PubRel::read(fixed_header, packet)?),
            PacketType::PubComp => Self::PubComp(PubComp::read(fixed_header, packet)?),
            PacketType::Subscribe => Self::Subscribe(Subscribe::read(fixed_header, packet)?),
            PacketType::SubAck => Self::SubAck(SubAck::read(fixed_header, packet)?),
            PacketType::Unsubscribe => Self::Unsubscribe(Unsubscribe::read(fixed_header, packet)?),
            PacketType::UnsubAck => Self::UnsubAck(UnsubAck::read(fixed_header, packet)?),
            PacketType::PingReq => Self::PingReq,
            PacketType::PingResp => Self::PingResp,
            PacketType::Disconnect => Self::Disconnect,
        };

        Ok(packet)
    }

    /// Serializes this MQTT v3.1.1 packet into the output buffer.
    ///
    /// # Errors
    ///
    /// Returns an error if the packet cannot be encoded within `max_size` or
    /// violates MQTT encoding limits.
    pub fn write(&self, stream: &mut BytesMut, max_size: usize) -> Result<usize, Error> {
        if self.size() > max_size {
            return Err(Error::OutgoingPacketTooLarge {
                pkt_size: self.size(),
                max: max_size,
            });
        }

        match self {
            Self::Connect(c) => c.write(stream),
            Self::ConnAck(c) => c.write(stream),
            Self::Publish(p) => p.write(stream),
            Self::PubAck(p) => p.write(stream),
            Self::PubRec(p) => p.write(stream),
            Self::PubRel(p) => p.write(stream),
            Self::PubComp(p) => p.write(stream),
            Self::Subscribe(s) => s.write(stream),
            Self::SubAck(s) => s.write(stream),
            Self::Unsubscribe(u) => u.write(stream),
            Self::UnsubAck(u) => u.write(stream),
            Self::PingReq => PingReq.write(stream),
            Self::PingResp => PingResp.write(stream),
            Self::Disconnect => Disconnect.write(stream),
        }
    }
}

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
        | PacketType::Disconnect => flags == 0,
    };

    if valid {
        Ok(())
    } else {
        Err(Error::IncorrectPacketFormat)
    }
}

/// Return number of remaining length bytes required for encoding length
fn len_len(len: usize) -> usize {
    mqttbytes_core::primitives::len_len(len)
}

#[cfg(test)]
mod tests {
    use bytes::BytesMut;

    use super::{Error, Packet};

    #[test]
    fn read_rejects_malformed_pubrel_flags() {
        let mut stream = BytesMut::from(&[0x60, 0x02, 0x00, 0x01][..]);
        let packet = Packet::read(&mut stream, 10);
        assert!(matches!(packet, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_rejects_malformed_subscribe_flags() {
        let mut stream = BytesMut::from(&[0x80, 0x05, 0x00, 0x01, b'a', 0x00, 0x00][..]);
        let packet = Packet::read(&mut stream, 10);
        assert!(matches!(packet, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn read_rejects_malformed_disconnect_flags() {
        let mut stream = BytesMut::from(&[0xE1, 0x00][..]);
        let packet = Packet::read(&mut stream, 10);
        assert!(matches!(packet, Err(Error::IncorrectPacketFormat)));
    }
}
