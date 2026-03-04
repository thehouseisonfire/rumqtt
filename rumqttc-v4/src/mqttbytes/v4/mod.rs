use super::*;

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

    /// Reads a stream of bytes and extracts next MQTT packet out of it
    pub fn read(stream: &mut BytesMut, max_size: usize) -> Result<Self, Error> {
        Self::read_with_mode(stream, max_size, Utf8ComplianceMode::Permissive)
    }

    pub(crate) fn read_with_mode(
        stream: &mut BytesMut,
        max_size: usize,
        mode: Utf8ComplianceMode,
    ) -> Result<Self, Error> {
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
            PacketType::Connect => {
                Packet::Connect(Connect::read_with_mode(fixed_header, packet, mode)?)
            }
            PacketType::ConnAck => Packet::ConnAck(ConnAck::read(fixed_header, packet)?),
            PacketType::Publish => {
                Packet::Publish(Publish::read_with_mode(fixed_header, packet, mode)?)
            }
            PacketType::PubAck => Packet::PubAck(PubAck::read(fixed_header, packet)?),
            PacketType::PubRec => Packet::PubRec(PubRec::read(fixed_header, packet)?),
            PacketType::PubRel => Packet::PubRel(PubRel::read(fixed_header, packet)?),
            PacketType::PubComp => Packet::PubComp(PubComp::read(fixed_header, packet)?),
            PacketType::Subscribe => {
                Packet::Subscribe(Subscribe::read_with_mode(fixed_header, packet, mode)?)
            }
            PacketType::SubAck => Packet::SubAck(SubAck::read(fixed_header, packet)?),
            PacketType::Unsubscribe => {
                Packet::Unsubscribe(Unsubscribe::read_with_mode(fixed_header, packet, mode)?)
            }
            PacketType::UnsubAck => Packet::UnsubAck(UnsubAck::read(fixed_header, packet)?),
            PacketType::PingReq => Packet::PingReq,
            PacketType::PingResp => Packet::PingResp,
            PacketType::Disconnect => Packet::Disconnect,
        };

        Ok(packet)
    }

    /// Serializes the MQTT packet into a stream of bytes
    pub fn write(&self, stream: &mut BytesMut, max_size: usize) -> Result<usize, Error> {
        self.write_with_mode(stream, max_size, Utf8ComplianceMode::Permissive)
    }

    pub(crate) fn write_with_mode(
        &self,
        stream: &mut BytesMut,
        max_size: usize,
        mode: Utf8ComplianceMode,
    ) -> Result<usize, Error> {
        if self.size() > max_size {
            return Err(Error::OutgoingPacketTooLarge {
                pkt_size: self.size(),
                max: max_size,
            });
        }

        match self {
            Packet::Connect(c) => c.write_with_mode(stream, mode),
            Packet::ConnAck(c) => c.write(stream),
            Packet::Publish(p) => p.write_with_mode(stream, mode),
            Packet::PubAck(p) => p.write(stream),
            Packet::PubRec(p) => p.write(stream),
            Packet::PubRel(p) => p.write(stream),
            Packet::PubComp(p) => p.write(stream),
            Packet::Subscribe(s) => s.write_with_mode(stream, mode),
            Packet::SubAck(s) => s.write(stream),
            Packet::Unsubscribe(u) => u.write_with_mode(stream, mode),
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
