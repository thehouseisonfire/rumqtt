use super::{
    Error, FixedHeader, PropertyType, QoS, len_len, length, property, read_mqtt_string, read_u8,
    read_u16, write_mqtt_string, write_remaining_length,
};
use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Acknowledgement to subscribe
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubAck {
    pub pkid: u16,
    pub return_codes: Vec<SubscribeReasonCode>,
    pub properties: Option<SubAckProperties>,
}

impl SubAck {
    fn len(&self) -> usize {
        let mut len = 2 + self.return_codes.len();

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

    pub fn read(fixed_header: FixedHeader, mut bytes: Bytes) -> Result<Self, Error> {
        let variable_header_index = fixed_header.header_len;
        bytes.advance(variable_header_index);

        let pkid = read_u16(&mut bytes)?;
        let properties = SubAckProperties::read(&mut bytes)?;

        if !bytes.has_remaining() {
            return Err(Error::MalformedPacket);
        }

        let mut return_codes = Vec::new();
        while bytes.has_remaining() {
            let return_code = read_u8(&mut bytes)?;
            return_codes.push(reason(return_code)?);
        }

        let suback = Self {
            pkid,
            return_codes,
            properties,
        };

        Ok(suback)
    }

    pub fn write(&self, buffer: &mut BytesMut) -> Result<usize, Error> {
        buffer.put_u8(0x90);
        let remaining_len = self.len();
        let remaining_len_bytes = write_remaining_length(buffer, remaining_len)?;

        buffer.put_u16(self.pkid);

        if let Some(p) = &self.properties {
            p.write(buffer)?;
        } else {
            write_remaining_length(buffer, 0)?;
        }

        for &return_code in &self.return_codes {
            buffer.put_u8(code(return_code));
        }
        Ok(1 + remaining_len_bytes + remaining_len)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscribeReasonCode {
    Success(QoS),
    Failure,
    Unspecified,
    ImplementationSpecific,
    NotAuthorized,
    TopicFilterInvalid,
    PkidInUse,
    QuotaExceeded,
    SharedSubscriptionsNotSupported,
    SubscriptionIdNotSupported,
    WildcardSubscriptionsNotSupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubAckProperties {
    pub reason_string: Option<String>,
    pub user_properties: Vec<(String, String)>,
}

impl SubAckProperties {
    fn len(&self) -> usize {
        let mut len = 0;

        if let Some(reason) = &self.reason_string {
            len += 1 + 2 + reason.len();
        }

        for (key, value) in &self.user_properties {
            len += 1 + 2 + key.len() + 2 + value.len();
        }

        len
    }

    pub fn read(bytes: &mut Bytes) -> Result<Option<Self>, Error> {
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

        Ok(Some(Self {
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

        for (key, value) in &self.user_properties {
            buffer.put_u8(PropertyType::UserProperty as u8);
            write_mqtt_string(buffer, key);
            write_mqtt_string(buffer, value);
        }

        Ok(())
    }
}

fn reason(code: u8) -> Result<SubscribeReasonCode, Error> {
    code.try_into()
}

const fn code(value: SubscribeReasonCode) -> u8 {
    match value {
        SubscribeReasonCode::Success(qos) => qos as u8,
        SubscribeReasonCode::Failure | SubscribeReasonCode::Unspecified => 0x80,
        SubscribeReasonCode::ImplementationSpecific => 131,
        SubscribeReasonCode::NotAuthorized => 135,
        SubscribeReasonCode::TopicFilterInvalid => 143,
        SubscribeReasonCode::PkidInUse => 145,
        SubscribeReasonCode::QuotaExceeded => 151,
        SubscribeReasonCode::SharedSubscriptionsNotSupported => 158,
        SubscribeReasonCode::SubscriptionIdNotSupported => 161,
        SubscribeReasonCode::WildcardSubscriptionsNotSupported => 162,
    }
}

impl TryFrom<u8> for SubscribeReasonCode {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Success(QoS::AtMostOnce)),
            1 => Ok(Self::Success(QoS::AtLeastOnce)),
            2 => Ok(Self::Success(QoS::ExactlyOnce)),
            128 => Ok(Self::Unspecified),
            131 => Ok(Self::ImplementationSpecific),
            135 => Ok(Self::NotAuthorized),
            143 => Ok(Self::TopicFilterInvalid),
            145 => Ok(Self::PkidInUse),
            151 => Ok(Self::QuotaExceeded),
            158 => Ok(Self::SharedSubscriptionsNotSupported),
            161 => Ok(Self::SubscriptionIdNotSupported),
            162 => Ok(Self::WildcardSubscriptionsNotSupported),
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
        let suback_props = SubAckProperties {
            reason_string: None,
            user_properties: vec![(USER_PROP_KEY.into(), USER_PROP_VAL.into())],
        };

        let suback_pkt = SubAck {
            pkid: 1,
            return_codes: vec![SubscribeReasonCode::Success(QoS::ExactlyOnce)],
            properties: Some(suback_props),
        };

        let size_from_size = suback_pkt.size();
        let size_from_write = suback_pkt.write(&mut dummy_bytes).unwrap();
        let size_from_bytes = dummy_bytes.len();

        assert_eq!(size_from_write, size_from_bytes);
        assert_eq!(size_from_size, size_from_bytes);
    }

    #[test]
    fn reason_and_code_round_trip() {
        let values = [
            (0, SubscribeReasonCode::Success(QoS::AtMostOnce)),
            (1, SubscribeReasonCode::Success(QoS::AtLeastOnce)),
            (2, SubscribeReasonCode::Success(QoS::ExactlyOnce)),
            (128, SubscribeReasonCode::Unspecified),
            (131, SubscribeReasonCode::ImplementationSpecific),
            (135, SubscribeReasonCode::NotAuthorized),
            (143, SubscribeReasonCode::TopicFilterInvalid),
            (145, SubscribeReasonCode::PkidInUse),
            (151, SubscribeReasonCode::QuotaExceeded),
            (158, SubscribeReasonCode::SharedSubscriptionsNotSupported),
            (161, SubscribeReasonCode::SubscriptionIdNotSupported),
            (162, SubscribeReasonCode::WildcardSubscriptionsNotSupported),
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
    fn failure_encodes_like_unspecified() {
        assert_eq!(code(SubscribeReasonCode::Failure), 0x80);
        assert_eq!(code(SubscribeReasonCode::Unspecified), 0x80);
    }

    #[test]
    fn write_multiple_reasons_bytes() {
        let mut buffer = BytesMut::new();
        let suback = SubAck {
            pkid: 10,
            return_codes: vec![
                SubscribeReasonCode::Success(QoS::AtMostOnce),
                SubscribeReasonCode::ImplementationSpecific,
                SubscribeReasonCode::WildcardSubscriptionsNotSupported,
            ],
            properties: None,
        };

        suback.write(&mut buffer).unwrap();

        let expected = [
            0x90, // Packet type
            0x06, // Remaining length
            0x00, 0x0A, // pkid
            0x00, // properties length
            0x00, // Success QoS0
            0x83, // ImplementationSpecific
            0xA2, // WildcardSubscriptionsNotSupported
        ];
        assert_eq!(&buffer[..], &expected);
    }

    #[test]
    fn read_errors_on_invalid_first_reason_code() {
        let packet = Bytes::from_static(&[
            0x90, 0x0C, // packet type + remaining length
            0x00, 0x0A, // pkid
            0x00, // properties length
            0xFF, // invalid first reason code
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // trailing reason bytes
        ]);

        let fixed_header = FixedHeader::new(0x90, 1, 0x0C);
        assert!(matches!(
            SubAck::read(fixed_header, packet),
            Err(Error::InvalidSubscribeReasonCode(0xFF))
        ));
    }
}
