use bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder};

use super::{Error, Packet, Utf8ComplianceMode};

/// MQTT v4 codec
#[derive(Debug, Clone)]
pub struct Codec {
    /// Maximum packet size allowed by client
    pub max_incoming_size: usize,
    /// Maximum packet size allowed by broker
    pub max_outgoing_size: usize,
    /// UTF-8 string compliance mode for discouraged code points
    pub utf8_compliance_mode: Utf8ComplianceMode,
}

impl Decoder for Codec {
    type Item = Packet;
    type Error = Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match Packet::read_with_mode(src, self.max_incoming_size, self.utf8_compliance_mode) {
            Ok(packet) => Ok(Some(packet)),
            Err(Error::InsufficientBytes(b)) => {
                // Get more packets to construct the incomplete packet
                src.reserve(b);
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}

impl Encoder<Packet> for Codec {
    type Error = Error;

    fn encode(&mut self, item: Packet, dst: &mut BytesMut) -> Result<(), Self::Error> {
        item.write_with_mode(dst, self.max_outgoing_size, self.utf8_compliance_mode)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bytes::BytesMut;
    use tokio_util::codec::{Decoder, Encoder};

    use super::Codec;
    use crate::{Packet, Publish, QoS, Subscribe, Utf8ComplianceMode, mqttbytes::Error};

    #[test]
    fn outgoing_max_packet_size_check() {
        let mut buf = BytesMut::new();
        let mut codec = Codec {
            max_incoming_size: 100,
            max_outgoing_size: 200,
            utf8_compliance_mode: Utf8ComplianceMode::Permissive,
        };

        let mut small_publish = Publish::new("hello/world", QoS::AtLeastOnce, vec![1; 100]);
        small_publish.pkid = 1;
        codec
            .encode(Packet::Publish(small_publish), &mut buf)
            .unwrap();

        let large_publish = Publish::new("hello/world", QoS::AtLeastOnce, vec![1; 265]);
        match codec.encode(Packet::Publish(large_publish), &mut buf) {
            Err(Error::OutgoingPacketTooLarge {
                pkt_size: 281,
                max: 200,
            }) => {}
            _ => unreachable!(),
        }
    }

    #[test]
    fn incoming_max_packet_size_check_happens_on_partial_frame() {
        let mut buf = BytesMut::from(&[0x30, 0x14][..]);
        let mut codec = Codec {
            max_incoming_size: 10,
            max_outgoing_size: 200,
            utf8_compliance_mode: Utf8ComplianceMode::Permissive,
        };

        match codec.decode(&mut buf) {
            Err(Error::PayloadSizeLimitExceeded(20)) => {}
            _ => unreachable!(),
        }
    }

    #[test]
    fn strict_mode_rejects_discouraged_utf8_on_decode() {
        let mut buf = BytesMut::from(
            &[
                0x82, // subscribe
                0x08, // remaining len
                0x00, 0x01, // pkid
                0x00, 0x03, b'a', 0x00, b'b', // filter with U+0000
                0x00, // qos0 options
            ][..],
        );
        let mut codec = Codec {
            max_incoming_size: 64,
            max_outgoing_size: 200,
            utf8_compliance_mode: Utf8ComplianceMode::Strict,
        };

        let packet = codec.decode(&mut buf);
        assert!(matches!(packet, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn permissive_and_warn_modes_accept_discouraged_utf8_on_decode() {
        for mode in [Utf8ComplianceMode::Permissive, Utf8ComplianceMode::Warn] {
            let mut buf = BytesMut::from(
                &[
                    0x82, // subscribe
                    0x08, // remaining len
                    0x00, 0x01, // pkid
                    0x00, 0x03, b'a', 0x00, b'b', // filter with U+0000
                    0x00, // qos0 options
                ][..],
            );
            let mut codec = Codec {
                max_incoming_size: 64,
                max_outgoing_size: 200,
                utf8_compliance_mode: mode,
            };

            let packet = codec.decode(&mut buf);
            assert!(matches!(packet, Ok(Some(Packet::Subscribe(_)))));
        }
    }

    #[test]
    fn strict_mode_rejects_discouraged_utf8_on_encode() {
        let mut buf = BytesMut::new();
        let mut codec = Codec {
            max_incoming_size: 100,
            max_outgoing_size: 200,
            utf8_compliance_mode: Utf8ComplianceMode::Strict,
        };
        let subscribe = Subscribe::new("a\u{0000}b", QoS::AtMostOnce);

        let result = codec.encode(Packet::Subscribe(subscribe), &mut buf);
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn permissive_and_warn_modes_accept_discouraged_utf8_on_encode() {
        for mode in [Utf8ComplianceMode::Permissive, Utf8ComplianceMode::Warn] {
            let mut buf = BytesMut::new();
            let mut codec = Codec {
                max_incoming_size: 100,
                max_outgoing_size: 200,
                utf8_compliance_mode: mode,
            };
            let subscribe = Subscribe::new("a\u{0000}b", QoS::AtMostOnce);

            let result = codec.encode(Packet::Subscribe(subscribe), &mut buf);
            assert!(result.is_ok());
        }
    }
}
