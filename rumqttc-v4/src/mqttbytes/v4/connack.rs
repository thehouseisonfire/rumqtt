use super::{Error, FixedHeader, len_len, read_u8, write_remaining_length};
use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Return code in connack
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ConnectReturnCode {
    Success = 0,
    RefusedProtocolVersion,
    BadClientId,
    ServiceUnavailable,
    BadUserNamePassword,
    NotAuthorized,
}

/// Acknowledgement to connect packet
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnAck {
    pub session_present: bool,
    pub code: ConnectReturnCode,
}

impl ConnAck {
    #[must_use]
    pub const fn new(code: ConnectReturnCode, session_present: bool) -> Self {
        Self {
            session_present,
            code,
        }
    }

    const fn len() -> usize {
        // sesssion present + code

        1 + 1
    }

    pub fn read(fixed_header: FixedHeader, mut bytes: Bytes) -> Result<Self, Error> {
        if fixed_header.remaining_len != Self::len() {
            return Err(Error::PayloadSizeIncorrect);
        }

        let variable_header_index = fixed_header.header_len;
        bytes.advance(variable_header_index);

        let flags = read_u8(&mut bytes)?;
        let return_code = read_u8(&mut bytes)?;

        if (flags & 0xFE) != 0 {
            return Err(Error::IncorrectPacketFormat);
        }

        let session_present = (flags & 0x01) == 1;
        let code = connect_return(return_code)?;
        if code != ConnectReturnCode::Success && session_present {
            return Err(Error::IncorrectPacketFormat);
        }

        let connack = Self {
            session_present,
            code,
        };

        Ok(connack)
    }

    pub fn write(&self, buffer: &mut BytesMut) -> Result<usize, Error> {
        let len = Self::len();
        buffer.put_u8(0x20);

        let count = write_remaining_length(buffer, len)?;
        buffer.put_u8(u8::from(self.session_present));
        buffer.put_u8(self.code as u8);

        Ok(1 + count + len)
    }

    #[must_use]
    pub const fn size(&self) -> usize {
        let len = Self::len();
        let remaining_len_size = len_len(len);

        1 + remaining_len_size + len
    }
}

/// Connection return code type
const fn connect_return(num: u8) -> Result<ConnectReturnCode, Error> {
    match num {
        0 => Ok(ConnectReturnCode::Success),
        1 => Ok(ConnectReturnCode::RefusedProtocolVersion),
        2 => Ok(ConnectReturnCode::BadClientId),
        3 => Ok(ConnectReturnCode::ServiceUnavailable),
        4 => Ok(ConnectReturnCode::BadUserNamePassword),
        5 => Ok(ConnectReturnCode::NotAuthorized),
        num => Err(Error::InvalidConnectReturnCode(num)),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mqttbytes::parse_fixed_header;
    use bytes::BytesMut;
    use pretty_assertions::assert_eq;

    fn read_connack(packetstream: &[u8]) -> Result<ConnAck, Error> {
        let mut stream = bytes::BytesMut::new();
        stream.extend_from_slice(packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connack_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        ConnAck::read(fixed_header, connack_bytes)
    }

    #[test]
    fn connack_parsing_works() {
        let packetstream = &[
            0b0010_0000,
            0x02, // packet type, flags and remaining len
            0x01,
            0x00, // variable header. connack flags, connect return code
            0xDE,
            0xAD,
            0xBE,
            0xEF, // extra packets in the stream
        ];

        let connack = read_connack(packetstream).unwrap();

        assert_eq!(
            connack,
            ConnAck {
                session_present: true,
                code: ConnectReturnCode::Success,
            }
        );
    }

    #[test]
    fn connack_encoding_works() {
        let connack = ConnAck {
            session_present: true,
            code: ConnectReturnCode::Success,
        };

        let mut buf = BytesMut::new();
        connack.write(&mut buf).unwrap();
        assert_eq!(buf, vec![0b0010_0000, 0x02, 0x01, 0x00]);
    }

    #[test]
    fn connack_parsing_rejects_invalid_remaining_len() {
        let packetstream = &[0b0010_0000, 0x03, 0x00, 0x00, 0x00];
        let connack = read_connack(packetstream);
        assert!(matches!(connack, Err(Error::PayloadSizeIncorrect)));
    }

    #[test]
    fn connack_parsing_rejects_reserved_flag_bits() {
        let packetstream = &[0b0010_0000, 0x02, 0x02, 0x00];
        let connack = read_connack(packetstream);
        assert!(matches!(connack, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn connack_parsing_rejects_session_present_on_error() {
        let packetstream = &[0b0010_0000, 0x02, 0x01, 0x01];
        let connack = read_connack(packetstream);
        assert!(matches!(connack, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn connack_parsing_accepts_all_valid_return_codes() {
        let cases = [
            (0x00, ConnectReturnCode::Success),
            (0x01, ConnectReturnCode::RefusedProtocolVersion),
            (0x02, ConnectReturnCode::BadClientId),
            (0x03, ConnectReturnCode::ServiceUnavailable),
            (0x04, ConnectReturnCode::BadUserNamePassword),
            (0x05, ConnectReturnCode::NotAuthorized),
        ];

        for (raw, code) in cases {
            let packetstream = &[0b0010_0000, 0x02, 0x00, raw];
            let connack = read_connack(packetstream).unwrap();
            assert_eq!(connack, ConnAck::new(code, false));
        }
    }

    #[test]
    fn connack_parsing_rejects_invalid_return_code() {
        let packetstream = &[0b0010_0000, 0x02, 0x00, 0x06];
        let connack = read_connack(packetstream);
        assert!(matches!(
            connack,
            Err(Error::InvalidConnectReturnCode(0x06))
        ));
    }
}
