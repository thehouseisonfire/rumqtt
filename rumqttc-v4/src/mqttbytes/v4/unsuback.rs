use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::mqttbytes::{Error, FixedHeader, read_u16};

/// Acknowledgement to unsubscribe
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsubAck {
    pub pkid: u16,
}

impl UnsubAck {
    #[must_use]
    pub const fn new(pkid: u16) -> Self {
        Self { pkid }
    }

    #[must_use]
    pub const fn size(&self) -> usize {
        4
    }

    pub fn read(fixed_header: FixedHeader, mut bytes: Bytes) -> Result<Self, Error> {
        if fixed_header.remaining_len != 2 {
            return Err(Error::PayloadSizeIncorrect);
        }

        let variable_header_index = fixed_header.header_len;
        bytes.advance(variable_header_index);
        let pkid = read_u16(&mut bytes)?;

        if pkid == 0 {
            return Err(Error::PacketIdZero);
        }

        let unsuback = Self { pkid };

        Ok(unsuback)
    }

    pub fn write(&self, payload: &mut BytesMut) -> Result<usize, Error> {
        payload.put_slice(&[0xB0, 0x02]);
        payload.put_u16(self.pkid);
        Ok(4)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mqttbytes::parse_fixed_header;
    use bytes::BytesMut;

    #[test]
    fn unsuback_parsing_rejects_zero_packet_identifier() {
        let stream = &[0xB0, 0x02, 0x00, 0x00];
        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let ack_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = UnsubAck::read(fixed_header, ack_bytes);

        assert!(matches!(packet, Err(Error::PacketIdZero)));
    }
}
