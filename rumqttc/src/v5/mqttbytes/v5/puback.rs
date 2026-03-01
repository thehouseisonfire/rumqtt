use super::*;
use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Return code in puback
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PubAckReason {
    Success = 0,
    NoMatchingSubscribers = 16,
    UnspecifiedError = 128,
    ImplementationSpecificError = 131,
    NotAuthorized = 135,
    TopicNameInvalid = 144,
    PacketIdentifierInUse = 145,
    QuotaExceeded = 151,
    PayloadFormatInvalid = 153,
}

/// Acknowledgement to QoS1 publish
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubAck {
    pub pkid: u16,
    pub reason: PubAckReason,
    pub properties: Option<PubAckProperties>,
}

impl PubAck {
    #[must_use]
    pub fn new(pkid: u16, properties: Option<PubAckProperties>) -> Self {
        Self {
            pkid,
            reason: PubAckReason::Success,
            properties,
        }
    }

    #[must_use]
    pub fn size(&self) -> usize {
        if self.reason == PubAckReason::Success && self.properties.is_none() {
            return 4;
        }
        let len = self.len();
        let remaining_len_size = len_len(len);

        1 + remaining_len_size + len
    }

    fn len(&self) -> usize {
        let mut len = 2 + 1; // pkid + reason

        // If there are no properties, sending reason code is optional
        if self.reason == PubAckReason::Success && self.properties.is_none() {
            return 2;
        }

        if let Some(p) = &self.properties {
            let properties_len = p.len();
            let properties_len_len = len_len(properties_len);
            len += properties_len_len + properties_len;
        } else {
            // just 1 byte representing 0 len properties
            len += 1;
        }

        len
    }

    pub fn read(fixed_header: FixedHeader, mut bytes: Bytes) -> Result<PubAck, Error> {
        let variable_header_index = fixed_header.fixed_header_len;
        bytes.advance(variable_header_index);
        let pkid = read_u16(&mut bytes)?;

        // No reason code or properties if remaining length == 2
        if fixed_header.remaining_len == 2 {
            return Ok(PubAck {
                pkid,
                reason: PubAckReason::Success,
                properties: None,
            });
        }

        // No properties len or properties if remaining len > 2 but < 4
        let ack_reason = read_u8(&mut bytes)?;
        if fixed_header.remaining_len < 4 {
            return Ok(PubAck {
                pkid,
                reason: reason(ack_reason)?,
                properties: None,
            });
        }

        let properties = PubAckProperties::read(&mut bytes)?;
        let puback = PubAck {
            pkid,
            reason: reason(ack_reason)?,
            properties,
        };

        Ok(puback)
    }

    pub fn write(&self, buffer: &mut BytesMut) -> Result<usize, Error> {
        let len = self.len();
        buffer.put_u8(0x40);

        let count = write_remaining_length(buffer, len)?;
        buffer.put_u16(self.pkid);

        // Reason code is optional with success if there are no properties
        if self.reason == PubAckReason::Success && self.properties.is_none() {
            return Ok(4);
        }

        buffer.put_u8(code(self.reason));
        if let Some(p) = &self.properties {
            p.write(buffer)?;
        } else {
            write_remaining_length(buffer, 0)?;
        }

        Ok(1 + count + len)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubAckProperties {
    pub reason_string: Option<String>,
    pub user_properties: Vec<(String, String)>,
}

impl PubAckProperties {
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

    pub fn read(bytes: &mut Bytes) -> Result<Option<PubAckProperties>, Error> {
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

        Ok(Some(PubAckProperties {
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
fn reason(num: u8) -> Result<PubAckReason, Error> {
    num.try_into()
}

fn code(reason: PubAckReason) -> u8 {
    reason as u8
}

impl TryFrom<u8> for PubAckReason {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Success),
            16 => Ok(Self::NoMatchingSubscribers),
            128 => Ok(Self::UnspecifiedError),
            131 => Ok(Self::ImplementationSpecificError),
            135 => Ok(Self::NotAuthorized),
            144 => Ok(Self::TopicNameInvalid),
            145 => Ok(Self::PacketIdentifierInUse),
            151 => Ok(Self::QuotaExceeded),
            153 => Ok(Self::PayloadFormatInvalid),
            _ => Err(Error::InvalidConnectReturnCode(value)),
        }
    }
}

#[cfg(test)]
mod test {
    use super::super::test::{USER_PROP_KEY, USER_PROP_VAL};
    use super::*;
    use bytes::BytesMut;
    use pretty_assertions::assert_eq;

    #[test]
    fn length_calculation() {
        let mut dummy_bytes = BytesMut::new();
        // Use user_properties to pad the size to exceed ~128 bytes to make the
        // remaining_length field in the packet be 2 bytes long.
        let puback_props = PubAckProperties {
            reason_string: None,
            user_properties: vec![(USER_PROP_KEY.into(), USER_PROP_VAL.into())],
        };

        let puback_pkt = PubAck::new(1, Some(puback_props));

        let size_from_size = puback_pkt.size();
        let size_from_write = puback_pkt.write(&mut dummy_bytes).unwrap();
        let size_from_bytes = dummy_bytes.len();

        assert_eq!(size_from_write, size_from_bytes);
        assert_eq!(size_from_size, size_from_bytes);
    }

    #[test]
    fn reason_code_cast_matches_spec() {
        assert_eq!(PubAckReason::Success as u8, 0);
        assert_eq!(PubAckReason::NoMatchingSubscribers as u8, 16);
        assert_eq!(PubAckReason::UnspecifiedError as u8, 128);
        assert_eq!(PubAckReason::ImplementationSpecificError as u8, 131);
        assert_eq!(PubAckReason::NotAuthorized as u8, 135);
        assert_eq!(PubAckReason::TopicNameInvalid as u8, 144);
        assert_eq!(PubAckReason::PacketIdentifierInUse as u8, 145);
        assert_eq!(PubAckReason::QuotaExceeded as u8, 151);
        assert_eq!(PubAckReason::PayloadFormatInvalid as u8, 153);
    }

    #[test]
    fn reason_and_code_round_trip() {
        let values = [
            (0, PubAckReason::Success),
            (16, PubAckReason::NoMatchingSubscribers),
            (128, PubAckReason::UnspecifiedError),
            (131, PubAckReason::ImplementationSpecificError),
            (135, PubAckReason::NotAuthorized),
            (144, PubAckReason::TopicNameInvalid),
            (145, PubAckReason::PacketIdentifierInUse),
            (151, PubAckReason::QuotaExceeded),
            (153, PubAckReason::PayloadFormatInvalid),
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
            Err(Error::InvalidConnectReturnCode(42))
        ));
    }
}
