use super::*;
use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Acknowledgement to unsubscribe
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsubAck {
    pub pkid: u16,
    pub reasons: Vec<UnsubAckReason>,
    pub properties: Option<UnsubAckProperties>,
}

impl UnsubAck {
    fn len(&self) -> usize {
        let mut len = 2 + self.reasons.len();

        if let Some(p) = &self.properties {
            let properties_len = p.len();
            let properties_len_len = len_len(properties_len);
            len += properties_len_len + properties_len;
        } else {
            // just 1 byte representing 0 len
            len += 1;
        }

        len
    }

    #[must_use]
    pub fn size(&self) -> usize {
        let len = self.len();
        let remaining_len_size = len_len(len);

        1 + remaining_len_size + len
    }

    pub fn read(fixed_header: FixedHeader, mut bytes: Bytes) -> Result<UnsubAck, Error> {
        let variable_header_index = fixed_header.fixed_header_len;
        bytes.advance(variable_header_index);

        let pkid = read_u16(&mut bytes)?;
        let properties = UnsubAckProperties::read(&mut bytes)?;

        if !bytes.has_remaining() {
            return Err(Error::MalformedPacket);
        }

        let mut reasons = Vec::new();
        while bytes.has_remaining() {
            let r = read_u8(&mut bytes)?;
            reasons.push(reason(r)?);
        }

        let unsuback = UnsubAck {
            pkid,
            reasons,
            properties,
        };

        Ok(unsuback)
    }

    pub fn write(&self, buffer: &mut BytesMut) -> Result<usize, Error> {
        buffer.put_u8(0xB0);
        let remaining_len = self.len();
        let remaining_len_bytes = write_remaining_length(buffer, remaining_len)?;

        buffer.put_u16(self.pkid);

        if let Some(p) = &self.properties {
            p.write(buffer)?;
        } else {
            write_remaining_length(buffer, 0)?;
        }

        for &reason in &self.reasons {
            buffer.put_u8(code(reason));
        }
        Ok(1 + remaining_len_bytes + remaining_len)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UnsubAckReason {
    Success = 0x00,
    NoSubscriptionExisted = 0x11,
    UnspecifiedError = 0x80,
    ImplementationSpecificError = 0x83,
    NotAuthorized = 0x87,
    TopicFilterInvalid = 0x8F,
    PacketIdentifierInUse = 0x91,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsubAckProperties {
    pub reason_string: Option<String>,
    pub user_properties: Vec<(String, String)>,
}

impl UnsubAckProperties {
    fn len(&self) -> usize {
        let mut len = 0;

        if let Some(reason) = &self.reason_string {
            len += 1 + 2 + reason.len();
        }

        for (key, value) in self.user_properties.iter() {
            len += 1 + 2 + key.len() + 2 + value.len();
        }

        len
    }

    pub fn read(bytes: &mut Bytes) -> Result<Option<UnsubAckProperties>, Error> {
        let mut reason_string = None;
        let mut user_properties = Vec::new();

        let (properties_len_len, properties_len) = length(bytes.iter())?;
        bytes.advance(properties_len_len);
        if properties_len == 0 {
            return Ok(None);
        }

        let mut cursor = 0;
        // read until cursor reaches property length. properties_len = 0 will skip this loop
        while cursor < properties_len {
            let prop = read_u8(bytes)?;
            cursor += 1;

            match property(prop)? {
                PropertyType::ReasonString => {
                    let reason = read_mqtt_string(bytes)?;
                    cursor += 2 + reason.len();
                    reason_string = Some(reason);
                }
                PropertyType::UserProperty => {
                    let key = read_mqtt_string(bytes)?;
                    let value = read_mqtt_string(bytes)?;
                    cursor += 2 + key.len() + 2 + value.len();
                    user_properties.push((key, value));
                }
                _ => return Err(Error::InvalidPropertyType(prop)),
            }
        }

        Ok(Some(UnsubAckProperties {
            reason_string,
            user_properties,
        }))
    }

    pub fn write(&self, buffer: &mut BytesMut) -> Result<(), Error> {
        let len = self.len();
        write_remaining_length(buffer, len)?;

        if let Some(reason) = &self.reason_string {
            buffer.put_u8(PropertyType::ReasonString as u8);
            write_mqtt_string(buffer, reason);
        }

        for (key, value) in self.user_properties.iter() {
            buffer.put_u8(PropertyType::UserProperty as u8);
            write_mqtt_string(buffer, key);
            write_mqtt_string(buffer, value);
        }

        Ok(())
    }
}

/// Connection return code type
fn reason(num: u8) -> Result<UnsubAckReason, Error> {
    num.try_into()
}

fn code(reason: UnsubAckReason) -> u8 {
    reason as u8
}

impl TryFrom<u8> for UnsubAckReason {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Self::Success),
            0x11 => Ok(Self::NoSubscriptionExisted),
            0x80 => Ok(Self::UnspecifiedError),
            0x83 => Ok(Self::ImplementationSpecificError),
            0x87 => Ok(Self::NotAuthorized),
            0x8F => Ok(Self::TopicFilterInvalid),
            0x91 => Ok(Self::PacketIdentifierInUse),
            _ => Err(Error::InvalidSubscribeReasonCode(value)),
        }
    }
}

#[cfg(test)]
mod test {
    use super::super::test::{USER_PROP_KEY, USER_PROP_VAL};
    use super::*;
    use bytes::{Bytes, BytesMut};
    use pretty_assertions::assert_eq;

    #[test]
    fn length_calculation() {
        let mut dummy_bytes = BytesMut::new();
        // Use user_properties to pad the size to exceed ~128 bytes to make the
        // remaining_length field in the packet be 2 bytes long.
        let unsuback_props = UnsubAckProperties {
            reason_string: None,
            user_properties: vec![(USER_PROP_KEY.into(), USER_PROP_VAL.into())],
        };

        let unsuback_pkt = UnsubAck {
            pkid: 1,
            reasons: vec![UnsubAckReason::Success],
            properties: Some(unsuback_props),
        };

        let size_from_size = unsuback_pkt.size();
        let size_from_write = unsuback_pkt.write(&mut dummy_bytes).unwrap();
        let size_from_bytes = dummy_bytes.len();

        assert_eq!(size_from_write, size_from_bytes);
        assert_eq!(size_from_size, size_from_bytes);
    }

    #[test]
    fn reason_and_code_round_trip() {
        let values = [
            (0x00, UnsubAckReason::Success),
            (0x11, UnsubAckReason::NoSubscriptionExisted),
            (0x80, UnsubAckReason::UnspecifiedError),
            (0x83, UnsubAckReason::ImplementationSpecificError),
            (0x87, UnsubAckReason::NotAuthorized),
            (0x8F, UnsubAckReason::TopicFilterInvalid),
            (0x91, UnsubAckReason::PacketIdentifierInUse),
        ];

        for (raw, parsed) in values {
            assert_eq!(reason(raw).unwrap(), parsed);
            assert_eq!(code(parsed), raw);
        }
    }

    #[test]
    fn reason_invalid_code_errors() {
        assert!(matches!(
            reason(42),
            Err(Error::InvalidSubscribeReasonCode(42))
        ));
    }

    #[test]
    fn write_multiple_reasons_bytes() {
        let mut buffer = BytesMut::new();
        let unsuback = UnsubAck {
            pkid: 10,
            reasons: vec![
                UnsubAckReason::Success,
                UnsubAckReason::NoSubscriptionExisted,
                UnsubAckReason::PacketIdentifierInUse,
            ],
            properties: None,
        };

        unsuback.write(&mut buffer).unwrap();

        let expected = [
            0xB0, // Packet type
            0x06, // Remaining length
            0x00, 0x0A, // pkid
            0x00, // properties length
            0x00, // Success
            0x11, // NoSubscriptionExisted
            0x91, // PacketIdentifierInUse
        ];
        assert_eq!(&buffer[..], &expected);
    }

    #[test]
    fn read_errors_on_invalid_first_reason_code() {
        let packet = Bytes::from_static(&[
            0xB0, 0x0C, // packet type + remaining length
            0x00, 0x0A, // pkid
            0x00, // properties length
            0xFF, // invalid first reason code
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // trailing reason bytes
        ]);

        let fixed_header = FixedHeader::new(0xB0, 1, 0x0C);
        assert!(matches!(
            UnsubAck::read(fixed_header, packet),
            Err(Error::InvalidSubscribeReasonCode(0xFF))
        ));
    }
}
