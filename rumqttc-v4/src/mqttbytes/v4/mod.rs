use super::*;

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
                PacketType::PingReq => Ok(Packet::PingReq),
                PacketType::PingResp => Ok(Packet::PingResp),
                PacketType::Disconnect => Ok(Packet::Disconnect),
                _ => Err(Error::PayloadRequired),
            };
        }

        let packet = packet.freeze();
        let packet = match packet_type {
            PacketType::Connect => Packet::Connect(Connect::read(fixed_header, packet)?),
            PacketType::ConnAck => Packet::ConnAck(ConnAck::read(fixed_header, packet)?),
            PacketType::Publish => Packet::Publish(Publish::read(fixed_header, packet)?),
            PacketType::PubAck => Packet::PubAck(PubAck::read(fixed_header, packet)?),
            PacketType::PubRec => Packet::PubRec(PubRec::read(fixed_header, packet)?),
            PacketType::PubRel => Packet::PubRel(PubRel::read(fixed_header, packet)?),
            PacketType::PubComp => Packet::PubComp(PubComp::read(fixed_header, packet)?),
            PacketType::Subscribe => Packet::Subscribe(Subscribe::read(fixed_header, packet)?),
            PacketType::SubAck => Packet::SubAck(SubAck::read(fixed_header, packet)?),
            PacketType::Unsubscribe => {
                Packet::Unsubscribe(Unsubscribe::read(fixed_header, packet)?)
            }
            PacketType::UnsubAck => Packet::UnsubAck(UnsubAck::read(fixed_header, packet)?),
            PacketType::PingReq => Packet::PingReq,
            PacketType::PingResp => Packet::PingResp,
            PacketType::Disconnect => Packet::Disconnect,
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
            Packet::Connect(c) => c.write(stream),
            Packet::ConnAck(c) => c.write(stream),
            Packet::Publish(p) => p.write(stream),
            Packet::PubAck(p) => p.write(stream),
            Packet::PubRec(p) => p.write(stream),
            Packet::PubRel(p) => p.write(stream),
            Packet::PubComp(p) => p.write(stream),
            Packet::Subscribe(s) => s.write(stream),
            Packet::SubAck(s) => s.write(stream),
            Packet::Unsubscribe(u) => u.write(stream),
            Packet::UnsubAck(u) => u.write(stream),
            Packet::PingReq => PingReq.write(stream),
            Packet::PingResp => PingResp.write(stream),
            Packet::Disconnect => Disconnect.write(stream),
        }
    }
}

fn validate_fixed_header_flags(packet_type: PacketType, byte1: u8) -> Result<(), Error> {
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
