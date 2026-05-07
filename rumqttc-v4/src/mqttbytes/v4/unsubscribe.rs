use super::{
    BufMut, BytesMut, Error, FixedHeader, len_len, read_mqtt_string, read_u16, write_mqtt_string,
    write_remaining_length,
};
use bytes::{Buf, Bytes};

/// Unsubscribe packet
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsubscribe {
    pub pkid: u16,
    pub topics: Vec<String>,
}

impl Unsubscribe {
    pub fn new<S: Into<String>>(topic: S) -> Self {
        Self {
            pkid: 0,
            topics: vec![topic.into()],
        }
    }

    fn len(&self) -> usize {
        // len of pkid + vec![subscribe topics len]
        2 + self.topics.iter().fold(0, |s, t| s + t.len() + 2)
    }

    #[must_use]
    pub fn size(&self) -> usize {
        let len = self.len();
        let remaining_len_size = len_len(len);

        1 + remaining_len_size + len
    }

    pub fn read(fixed_header: FixedHeader, mut bytes: Bytes) -> Result<Self, Error> {
        let variable_header_index = fixed_header.header_len;
        bytes.advance(variable_header_index);

        let pkid = read_u16(&mut bytes)?;

        if pkid == 0 {
            return Err(Error::PacketIdZero);
        }

        let mut payload_bytes = fixed_header.remaining_len - 2;
        let mut topics = Vec::with_capacity(1);

        while payload_bytes > 0 {
            let topic_filter = read_mqtt_string(&mut bytes)?;
            payload_bytes -= topic_filter.len() + 2;
            topics.push(topic_filter);
        }

        let unsubscribe = Self { pkid, topics };
        Ok(unsubscribe)
    }

    pub fn write(&self, payload: &mut BytesMut) -> Result<usize, Error> {
        if self.pkid == 0 {
            return Err(Error::PacketIdZero);
        }

        let remaining_len = self.len();

        payload.put_u8(0xA2);
        let remaining_len_bytes = write_remaining_length(payload, remaining_len)?;
        payload.put_u16(self.pkid);

        for topic in &self.topics {
            write_mqtt_string(payload, topic.as_str())?;
        }
        Ok(1 + remaining_len_bytes + remaining_len)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mqttbytes::parse_fixed_header;
    use bytes::BytesMut;

    #[test]
    fn unsubscribe_parsing_rejects_zero_packet_identifier() {
        let stream = &[
            0b1010_0010,
            5, // packet type, flags and remaining len
            0x00,
            0x00, // variable header. pkid = 0
            0x00,
            0x01,
            b'a', // payload. topic filter = 'a'
        ];
        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let unsub_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Unsubscribe::read(fixed_header, unsub_bytes);

        assert!(matches!(packet, Err(Error::PacketIdZero)));
    }

    #[test]
    fn unsubscribe_encoding_rejects_zero_packet_identifier() {
        let unsubscribe = Unsubscribe {
            pkid: 0,
            topics: vec!["a/b".to_owned()],
        };

        let mut buf = BytesMut::new();
        let result = unsubscribe.write(&mut buf);

        assert!(matches!(result, Err(Error::PacketIdZero)));
    }

    #[test]
    fn unsubscribe_parsing_rejects_null_character_in_filter() {
        let stream = &[
            0b1010_0010,
            7, // packet type, flags and remaining len
            0x00,
            0x01, // variable header. pkid = 1
            0x00,
            0x03,
            b'a',
            0x00,
            b'b', // payload. topic filter = "a\0b"
        ];
        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let unsub_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Unsubscribe::read(fixed_header, unsub_bytes);

        assert!(matches!(packet, Err(Error::MalformedPacket)));
    }

    #[test]
    fn unsubscribe_encoding_rejects_null_character_in_filter() {
        let unsubscribe = Unsubscribe {
            pkid: 1,
            topics: vec!["a\0b".to_owned()],
        };

        let mut buf = BytesMut::new();
        let result = unsubscribe.write(&mut buf);

        assert!(matches!(result, Err(Error::MalformedPacket)));
    }
}
