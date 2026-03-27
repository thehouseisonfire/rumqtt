use super::{Error, FixedHeader, len_len, read_u16, write_remaining_length};
use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Acknowledgement to `QoS1` publish
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubAck {
    pub pkid: u16,
}

impl PubAck {
    #[must_use]
    pub const fn new(pkid: u16) -> Self {
        Self { pkid }
    }

    const fn len() -> usize {
        // pkid
        2
    }

    #[must_use]
    pub fn size(&self) -> usize {
        let len = Self::len();
        let remaining_len_size = len_len(len);

        1 + remaining_len_size + len
    }

    pub fn read(fixed_header: FixedHeader, mut bytes: Bytes) -> Result<Self, Error> {
        if fixed_header.remaining_len != Self::len() {
            return Err(Error::PayloadSizeIncorrect);
        }

        let variable_header_index = fixed_header.header_len;
        bytes.advance(variable_header_index);
        let pkid = read_u16(&mut bytes)?;

        if pkid == 0 {
            return Err(Error::PacketIdZero);
        }

        Ok(Self { pkid })
    }

    pub fn write(&self, buffer: &mut BytesMut) -> Result<usize, Error> {
        if self.pkid == 0 {
            return Err(Error::PacketIdZero);
        }

        let len = Self::len();
        buffer.put_u8(0x40);
        let count = write_remaining_length(buffer, len)?;
        buffer.put_u16(self.pkid);
        Ok(1 + count + len)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mqttbytes::parse_fixed_header;
    use bytes::BytesMut;
    use pretty_assertions::assert_eq;

    #[test]
    fn puback_encoding_works() {
        let stream = &[
            0b0100_0000,
            0x02, // packet type, flags and remaining len
            0x00,
            0x0A, // fixed header. packet identifier = 10
            0xDE,
            0xAD,
            0xBE,
            0xEF, // extra packets in the stream
        ];
        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let ack_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = PubAck::read(fixed_header, ack_bytes).unwrap();

        assert_eq!(packet, PubAck { pkid: 10 });
    }

    #[test]
    fn puback_parsing_rejects_invalid_remaining_length() {
        let stream = &[0b0100_0000, 0x03, 0x00, 0x0A, 0x00];
        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let ack_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = PubAck::read(fixed_header, ack_bytes);

        assert!(matches!(packet, Err(Error::PayloadSizeIncorrect)));
    }

    #[test]
    fn puback_parsing_rejects_zero_packet_identifier() {
        let stream = &[0b0100_0000, 0x02, 0x00, 0x00];
        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let ack_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = PubAck::read(fixed_header, ack_bytes);

        assert!(matches!(packet, Err(Error::PacketIdZero)));
    }

    #[test]
    fn puback_encoding_rejects_zero_packet_identifier() {
        let packet = PubAck { pkid: 0 };
        let mut buf = BytesMut::new();
        let result = packet.write(&mut buf);

        assert!(matches!(result, Err(Error::PacketIdZero)));
    }
}
