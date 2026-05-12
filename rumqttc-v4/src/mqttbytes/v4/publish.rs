use super::{
    BufMut, BytesMut, Error, FixedHeader, QoS, fmt, len_len, qos, read_mqtt_bytes, read_u16,
    write_mqtt_bytes, write_remaining_length,
};
use bytes::{Buf, Bytes};

/// Publish packet
#[derive(Clone, PartialEq, Eq)]
pub struct Publish {
    pub dup: bool,
    pub qos: QoS,
    pub retain: bool,
    pub topic: Bytes,
    pub pkid: u16,
    pub payload: Bytes,
}

impl Publish {
    pub fn new<S: Into<String>, P: Into<Vec<u8>>>(topic: S, qos: QoS, payload: P) -> Self {
        let topic = topic.into();
        Self {
            dup: false,
            qos,
            retain: false,
            pkid: 0,
            topic: Bytes::from(topic.into_bytes()),
            payload: Bytes::from(payload.into()),
        }
    }

    pub fn from_bytes<S: Into<String>>(topic: S, qos: QoS, payload: Bytes) -> Self {
        let topic = topic.into();
        Self {
            dup: false,
            qos,
            retain: false,
            pkid: 0,
            topic: Bytes::from(topic.into_bytes()),
            payload,
        }
    }

    fn len(&self) -> usize {
        let len = 2 + self.topic.len() + self.payload.len();
        if self.qos != QoS::AtMostOnce && self.pkid != 0 {
            len + 2
        } else {
            len
        }
    }

    pub fn size(&self) -> usize {
        let len = self.len();
        let remaining_len_size = len_len(len);

        1 + remaining_len_size + len
    }

    pub fn read(fixed_header: FixedHeader, mut bytes: Bytes) -> Result<Self, Error> {
        let qos = qos((fixed_header.byte1 & 0b0110) >> 1)?;
        let dup = (fixed_header.byte1 & 0b1000) != 0;
        let retain = (fixed_header.byte1 & 0b0001) != 0;
        if qos == QoS::AtMostOnce && dup {
            return Err(Error::IncorrectPacketFormat);
        }

        let variable_header_index = fixed_header.header_len;
        bytes.advance(variable_header_index);
        let topic = read_mqtt_bytes(&mut bytes)?;
        validate_publish_topic_name(&topic)?;

        // Packet identifier exists where QoS > 0
        let pkid = match qos {
            QoS::AtMostOnce => 0,
            QoS::AtLeastOnce | QoS::ExactlyOnce => read_u16(&mut bytes)?,
        };

        if qos != QoS::AtMostOnce && pkid == 0 {
            return Err(Error::PacketIdZero);
        }

        let publish = Self {
            dup,
            retain,
            qos,
            pkid,
            topic,
            payload: bytes,
        };

        Ok(publish)
    }

    pub fn write(&self, buffer: &mut BytesMut) -> Result<usize, Error> {
        if self.qos == QoS::AtMostOnce && self.dup {
            return Err(Error::IncorrectPacketFormat);
        }

        let len = self.len();

        let dup = u8::from(self.dup);
        let qos = self.qos as u8;
        let retain = u8::from(self.retain);
        buffer.put_u8(0b0011_0000 | retain | (qos << 1) | (dup << 3));

        validate_publish_topic_name(&self.topic)?;

        let count = write_remaining_length(buffer, len)?;
        write_mqtt_bytes(buffer, &self.topic)?;

        if self.qos != QoS::AtMostOnce {
            let pkid = self.pkid;
            if pkid == 0 {
                return Err(Error::PacketIdZero);
            }

            buffer.put_u16(pkid);
        }

        buffer.extend_from_slice(&self.payload);

        Ok(1 + count + len)
    }
}

fn validate_publish_topic_name(topic: &[u8]) -> Result<(), Error> {
    if topic.is_empty() {
        return Err(Error::IncorrectPacketFormat);
    }

    if topic.iter().any(|&b| b == b'+' || b == b'#') {
        return Err(Error::IncorrectPacketFormat);
    }

    let topic = std::str::from_utf8(topic).map_err(|_| Error::TopicNotUtf8)?;
    if topic.contains('\0') {
        return Err(Error::MalformedPacket);
    }

    Ok(())
}

impl fmt::Debug for Publish {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let topic = std::str::from_utf8(&self.topic).unwrap_or("<invalid utf-8>");
        write!(
            f,
            "Topic = {}, Qos = {:?}, Retain = {}, Pkid = {:?}, Payload Size = {}",
            topic,
            self.qos,
            self.retain,
            self.pkid,
            self.payload.len()
        )
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mqttbytes::parse_fixed_header;
    use bytes::{Bytes, BytesMut};
    use pretty_assertions::assert_eq;

    #[test]
    fn qos1_publish_parsing_works() {
        let stream = &[
            0b0011_0010,
            11, // packet type, flags and remaining len
            0x00,
            0x03,
            b'a',
            b'/',
            b'b', // variable header. topic name = 'a/b'
            0x00,
            0x0a, // variable header. pkid = 10
            0xF1,
            0xF2,
            0xF3,
            0xF4, // publish payload
            0xDE,
            0xAD,
            0xBE,
            0xEF, // extra packets in the stream
        ];

        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let publish_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Publish::read(fixed_header, publish_bytes).unwrap();

        let payload = &[0xF1, 0xF2, 0xF3, 0xF4];
        assert_eq!(
            packet,
            Publish {
                dup: false,
                qos: QoS::AtLeastOnce,
                retain: false,
                topic: Bytes::from_static(b"a/b"),
                pkid: 10,
                payload: Bytes::from(&payload[..]),
            }
        );
    }

    #[test]
    fn qos0_publish_parsing_works() {
        let stream = &[
            0b0011_0000,
            7, // packet type, flags and remaining len
            0x00,
            0x03,
            b'a',
            b'/',
            b'b', // variable header. topic name = 'a/b'
            0x01,
            0x02, // payload
            0xDE,
            0xAD,
            0xBE,
            0xEF, // extra packets in the stream
        ];

        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let publish_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Publish::read(fixed_header, publish_bytes).unwrap();

        assert_eq!(
            packet,
            Publish {
                dup: false,
                qos: QoS::AtMostOnce,
                retain: false,
                topic: Bytes::from_static(b"a/b"),
                pkid: 0,
                payload: Bytes::from(&[0x01, 0x02][..]),
            }
        );
    }

    #[test]
    fn publish_parsing_preserves_retain_flag() {
        let stream = &[
            0b0011_0001,
            7, // packet type, flags and remaining len
            0x00,
            0x03,
            b'a',
            b'/',
            b'b', // variable header. topic name = 'a/b'
            0x01,
            0x02, // payload
        ];

        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let publish_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Publish::read(fixed_header, publish_bytes).unwrap();

        assert_eq!(
            packet,
            Publish {
                dup: false,
                qos: QoS::AtMostOnce,
                retain: true,
                topic: Bytes::from_static(b"a/b"),
                pkid: 0,
                payload: Bytes::from(&[0x01, 0x02][..]),
            }
        );
    }

    #[test]
    fn publish_parsing_accepts_retained_empty_payload() {
        let stream = &[
            0b0011_0001,
            5, // packet type, flags and remaining len
            0x00,
            0x03,
            b'a',
            b'/',
            b'b', // variable header. topic name = 'a/b'
        ];

        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let publish_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Publish::read(fixed_header, publish_bytes).unwrap();

        assert_eq!(
            packet,
            Publish {
                dup: false,
                qos: QoS::AtMostOnce,
                retain: true,
                topic: Bytes::from_static(b"a/b"),
                pkid: 0,
                payload: Bytes::new(),
            }
        );
    }

    #[test]
    fn qos1_publish_encoding_works() {
        let publish = Publish {
            dup: false,
            qos: QoS::AtLeastOnce,
            retain: false,
            topic: Bytes::from_static(b"a/b"),
            pkid: 10,
            payload: Bytes::from(vec![0xF1, 0xF2, 0xF3, 0xF4]),
        };

        let mut buf = BytesMut::new();
        publish.write(&mut buf).unwrap();

        assert_eq!(
            buf,
            vec![
                0b0011_0010,
                11,
                0x00,
                0x03,
                b'a',
                b'/',
                b'b',
                0x00,
                0x0a,
                0xF1,
                0xF2,
                0xF3,
                0xF4
            ]
        );
    }

    #[test]
    fn qos0_publish_encoding_works() {
        let publish = Publish {
            dup: false,
            qos: QoS::AtMostOnce,
            retain: false,
            topic: Bytes::from_static(b"a/b"),
            pkid: 0,
            payload: Bytes::from(vec![0xE1, 0xE2, 0xE3, 0xE4]),
        };

        let mut buf = BytesMut::new();
        publish.write(&mut buf).unwrap();

        assert_eq!(
            buf,
            vec![
                0b0011_0000,
                9,
                0x00,
                0x03,
                b'a',
                b'/',
                b'b',
                0xE1,
                0xE2,
                0xE3,
                0xE4
            ]
        );
    }

    #[test]
    fn publish_encoding_accepts_retained_empty_payload() {
        let publish = Publish {
            dup: false,
            qos: QoS::AtMostOnce,
            retain: true,
            topic: Bytes::from_static(b"a/b"),
            pkid: 0,
            payload: Bytes::new(),
        };

        let mut buf = BytesMut::new();
        publish.write(&mut buf).unwrap();

        assert_eq!(buf, vec![0b0011_0001, 5, 0x00, 0x03, b'a', b'/', b'b']);
    }

    #[test]
    fn qos0_publish_encoding_omits_packet_identifier_even_if_struct_has_pkid() {
        let publish = Publish {
            dup: false,
            qos: QoS::AtMostOnce,
            retain: false,
            topic: Bytes::from_static(b"a/b"),
            pkid: 10,
            payload: Bytes::from(vec![0xE1, 0xE2]),
        };

        let mut buf = BytesMut::new();
        publish.write(&mut buf).unwrap();

        assert_eq!(
            buf,
            vec![0b0011_0000, 7, 0x00, 0x03, b'a', b'/', b'b', 0xE1, 0xE2]
        );
    }

    #[test]
    fn publish_parsing_rejects_empty_topic() {
        let stream = &[0b0011_0000, 2, 0x00, 0x00];
        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let publish_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Publish::read(fixed_header, publish_bytes);
        assert!(matches!(packet, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn publish_parsing_rejects_wildcards_in_topic_name() {
        let stream = &[0b0011_0000, 5, 0x00, 0x03, b'a', b'/', b'#'];
        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let publish_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Publish::read(fixed_header, publish_bytes);
        assert!(matches!(packet, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn publish_encoding_rejects_empty_topic() {
        let publish = Publish {
            dup: false,
            qos: QoS::AtMostOnce,
            retain: false,
            topic: Bytes::from_static(b""),
            pkid: 0,
            payload: Bytes::from(vec![0xE1, 0xE2]),
        };

        let mut buf = BytesMut::new();
        let result = publish.write(&mut buf);
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn publish_parsing_rejects_dup_set_for_qos0() {
        let stream = &[
            0b0011_1000,
            7, // packet type, flags and remaining len
            0x00,
            0x03,
            b'a',
            b'/',
            b'b', // variable header. topic name = 'a/b'
            0x01,
            0x02, // payload
        ];
        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let publish_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Publish::read(fixed_header, publish_bytes);
        assert!(matches!(packet, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn publish_parsing_rejects_qos_bits_11() {
        let stream = &[
            0b0011_0110,
            9, // packet type, flags and remaining len
            0x00,
            0x03,
            b'a',
            b'/',
            b'b', // topic name = 'a/b'
            0x00,
            0x0a, // packet identifier
            0x01,
            0x02, // payload
        ];
        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let publish_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Publish::read(fixed_header, publish_bytes);
        assert!(matches!(packet, Err(Error::InvalidQoS(3))));
    }

    #[test]
    fn publish_encoding_rejects_dup_set_for_qos0() {
        let publish = Publish {
            dup: true,
            qos: QoS::AtMostOnce,
            retain: false,
            topic: Bytes::from_static(b"a/b"),
            pkid: 0,
            payload: Bytes::from(vec![0xE1, 0xE2]),
        };

        let mut buf = BytesMut::new();
        let result = publish.write(&mut buf);
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    /// MQTT-1.5.3-1: Publish topic name MUST be well-formed
    /// UTF-8 and MUST NOT include surrogate code points (U+D800..U+DFFF).
    #[test]
    fn publish_parsing_rejects_surrogate_in_topic_name() {
        // QoS 0 PUBLISH with topic containing U+D800 (CESU-8: 0xED 0xA0 0x80)
        let stream = &[
            0b0011_0000,
            5, // packet type, flags and remaining len
            0x00,
            0x03, // topic length = 3
            0xED,
            0xA0,
            0x80, // topic = U+D800 (surrogate)
            0x01,
            0x02, // payload
        ];
        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let publish_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Publish::read(fixed_header, publish_bytes);
        assert!(matches!(packet, Err(Error::TopicNotUtf8)));
    }

    #[test]
    fn publish_encoding_rejects_surrogate_in_topic_name() {
        let publish = Publish {
            dup: false,
            qos: QoS::AtMostOnce,
            retain: false,
            topic: Bytes::from_static(&[0xED, 0xA0, 0x80]), // U+D800
            pkid: 0,
            payload: Bytes::from(vec![0xE1, 0xE2]),
        };

        let mut buf = BytesMut::new();
        let result = publish.write(&mut buf);
        assert!(matches!(result, Err(Error::TopicNotUtf8)));
    }

    #[test]
    fn publish_parsing_rejects_overlong_utf8_in_topic_name() {
        // QoS 0 PUBLISH with topic containing overlong encoding of NUL (0xC0 0x80)
        let stream = &[
            0b0011_0000,
            4, // packet type, flags and remaining len
            0x00,
            0x02, // topic length = 2
            0xC0,
            0x80, // topic = overlong U+0000
            0x01, // payload
        ];
        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let publish_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Publish::read(fixed_header, publish_bytes);
        assert!(matches!(packet, Err(Error::TopicNotUtf8)));
    }

    #[test]
    fn publish_encoding_rejects_overlong_utf8_in_topic_name() {
        let publish = Publish {
            dup: false,
            qos: QoS::AtMostOnce,
            retain: false,
            topic: Bytes::from_static(&[0xC0, 0x80]), // overlong U+0000
            pkid: 0,
            payload: Bytes::from(vec![0xE1]),
        };

        let mut buf = BytesMut::new();
        let result = publish.write(&mut buf);
        assert!(matches!(result, Err(Error::TopicNotUtf8)));
    }

    #[test]
    fn publish_parsing_rejects_null_character_in_topic_name() {
        let stream = &[
            0b0011_0000,
            5, // packet type, flags and remaining len
            0x00,
            0x03, // topic length = 3
            b'a',
            0x00,
            b'b',
        ];
        let mut stream = BytesMut::from(&stream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let publish_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Publish::read(fixed_header, publish_bytes);

        assert!(matches!(packet, Err(Error::MalformedPacket)));
    }

    #[test]
    fn publish_encoding_rejects_null_character_in_topic_name() {
        let publish = Publish {
            dup: false,
            qos: QoS::AtMostOnce,
            retain: false,
            topic: Bytes::from_static(b"a\0b"),
            pkid: 0,
            payload: Bytes::from(vec![0xE1]),
        };

        let mut buf = BytesMut::new();
        let result = publish.write(&mut buf);

        assert!(matches!(result, Err(Error::MalformedPacket)));
    }
}
