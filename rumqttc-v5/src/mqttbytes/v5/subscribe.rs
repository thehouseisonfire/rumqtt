use super::{
    BufMut, BytesMut, Error, FixedHeader, PropertyType, QoS, len_len, length, property, qos,
    read_mqtt_string, read_u8, read_u16, write_mqtt_string, write_remaining_length,
};
use bytes::{Buf, Bytes};

/// Subscription packet
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Subscribe {
    pub pkid: u16,
    pub filters: Vec<Filter>,
    pub properties: Option<SubscribeProperties>,
}

impl Subscribe {
    #[must_use]
    pub fn new(filter: Filter, properties: Option<SubscribeProperties>) -> Self {
        Self {
            filters: vec![filter],
            properties,
            ..Default::default()
        }
    }

    pub fn new_many<F>(filters: F, properties: Option<SubscribeProperties>) -> Self
    where
        F: IntoIterator<Item = Filter>,
    {
        Self {
            filters: filters.into_iter().collect(),
            properties,
            ..Default::default()
        }
    }

    #[must_use]
    pub fn size(&self) -> usize {
        let len = self.len();
        let remaining_len_size = len_len(len);

        1 + remaining_len_size + len
    }

    fn len(&self) -> usize {
        let mut len = 2 + self.filters.iter().fold(0, |s, t| s + t.len());

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

    pub fn read(fixed_header: FixedHeader, mut bytes: Bytes) -> Result<Self, Error> {
        let variable_header_index = fixed_header.header_len;
        bytes.advance(variable_header_index);

        let pkid = read_u16(&mut bytes)?;
        let properties = SubscribeProperties::read(&mut bytes)?;

        // variable header size = 2 (packet identifier)
        let filters = Filter::read(&mut bytes)?;

        match filters.len() {
            0 => Err(Error::EmptySubscription),
            _ => Ok(Self {
                pkid,
                filters,
                properties,
            }),
        }
    }

    pub fn write(&self, buffer: &mut BytesMut) -> Result<usize, Error> {
        // write packet type
        buffer.put_u8(0x82);

        // write remaining length
        let remaining_len = self.len();
        let remaining_len_bytes = write_remaining_length(buffer, remaining_len)?;

        // write packet id
        buffer.put_u16(self.pkid);

        if let Some(p) = &self.properties {
            p.write(buffer)?;
        } else {
            write_remaining_length(buffer, 0)?;
        }

        // write filters
        for f in &self.filters {
            f.write(buffer);
        }

        Ok(1 + remaining_len_bytes + remaining_len)
    }
}

///  Subscription filter
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Filter {
    pub path: String,
    pub qos: QoS,
    pub nolocal: bool,
    pub preserve_retain: bool,
    pub retain_forward_rule: RetainForwardRule,
}

impl Filter {
    pub fn new<T: Into<String>>(topic: T, qos: QoS) -> Self {
        Self {
            path: topic.into(),
            qos,
            ..Default::default()
        }
    }

    fn len(&self) -> usize {
        // filter len + filter + options
        2 + self.path.len() + 1
    }

    pub fn read(bytes: &mut Bytes) -> Result<Vec<Self>, Error> {
        // variable header size = 2 (packet identifier)
        let mut filters = Vec::new();

        while bytes.has_remaining() {
            let path = read_mqtt_string(bytes)?;
            let options = read_u8(bytes)?;
            let requested_qos = options & 0b0000_0011;

            let nolocal = (options >> 2) & 0b0000_0001;
            let nolocal = nolocal != 0;

            let preserve_retain = (options >> 3) & 0b0000_0001;
            let preserve_retain = preserve_retain != 0;

            let retain_forward_rule = (options >> 4) & 0b0000_0011;
            let retain_forward_rule = match retain_forward_rule {
                0 => RetainForwardRule::OnEverySubscribe,
                1 => RetainForwardRule::OnNewSubscribe,
                2 => RetainForwardRule::Never,
                r => return Err(Error::InvalidRetainForwardRule(r)),
            };

            filters.push(Self {
                path,
                qos: qos(requested_qos).ok_or(Error::InvalidQoS(requested_qos))?,
                nolocal,
                preserve_retain,
                retain_forward_rule,
            });
        }

        Ok(filters)
    }

    pub fn write(&self, buffer: &mut BytesMut) {
        let mut options = 0;
        options |= self.qos as u8;

        if self.nolocal {
            options |= 0b0000_0100;
        }

        if self.preserve_retain {
            options |= 0b0000_1000;
        }

        options |= match self.retain_forward_rule {
            RetainForwardRule::OnEverySubscribe => 0b0000_0000,
            RetainForwardRule::OnNewSubscribe => 0b0001_0000,
            RetainForwardRule::Never => 0b0010_0000,
        };

        write_mqtt_string(buffer, self.path.as_str());
        buffer.put_u8(options);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RetainForwardRule {
    #[default]
    OnEverySubscribe,
    OnNewSubscribe,
    Never,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscribeProperties {
    pub id: Option<usize>,
    pub user_properties: Vec<(String, String)>,
}

impl SubscribeProperties {
    fn len(&self) -> usize {
        let mut len = 0;

        if let Some(id) = &self.id {
            len += 1 + len_len(*id);
        }

        for (key, value) in &self.user_properties {
            len += 1 + 2 + key.len() + 2 + value.len();
        }

        len
    }

    pub fn read(bytes: &mut Bytes) -> Result<Option<Self>, Error> {
        let mut id = None;
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
                PropertyType::SubscriptionIdentifier => {
                    let (id_len, sub_id) = length(bytes.iter())?;
                    if sub_id == 0 {
                        return Err(Error::ProtocolError);
                    }
                    cursor += id_len;
                    bytes.advance(id_len);
                    id = Some(sub_id);
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
            id,
            user_properties,
        }))
    }

    pub fn write(&self, buffer: &mut BytesMut) -> Result<(), Error> {
        let len = self.len();
        write_remaining_length(buffer, len)?;

        if let Some(id) = &self.id {
            if *id == 0 {
                return Err(Error::ProtocolError);
            }
            buffer.put_u8(PropertyType::SubscriptionIdentifier as u8);
            write_remaining_length(buffer, *id)?;
        }

        for (key, value) in &self.user_properties {
            buffer.put_u8(PropertyType::UserProperty as u8);
            write_mqtt_string(buffer, key);
            write_mqtt_string(buffer, value);
        }

        Ok(())
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
        let subscribe_props = SubscribeProperties {
            id: None,
            user_properties: vec![(USER_PROP_KEY.into(), USER_PROP_VAL.into())],
        };

        let subscribe_pkt = Subscribe::new(
            Filter::new("hello/world", QoS::AtMostOnce),
            Some(subscribe_props),
        );

        let size_from_size = subscribe_pkt.size();
        let size_from_write = subscribe_pkt.write(&mut dummy_bytes).unwrap();
        let size_from_bytes = dummy_bytes.len();

        assert_eq!(size_from_write, size_from_bytes);
        assert_eq!(size_from_size, size_from_bytes);
    }

    #[test]
    fn read_rejects_subscription_identifier_zero() {
        let mut bytes = Bytes::from_static(&[0x02, 0x0B, 0x00]);
        let result = SubscribeProperties::read(&mut bytes);

        assert!(matches!(result, Err(Error::ProtocolError)));
    }

    #[test]
    fn write_rejects_subscription_identifier_zero() {
        let props = SubscribeProperties {
            id: Some(0),
            user_properties: vec![],
        };

        let mut bytes = BytesMut::new();
        let result = props.write(&mut bytes);

        assert!(matches!(result, Err(Error::ProtocolError)));
    }

    #[test]
    fn read_subscription_identifier_and_user_property_parses_both() {
        let mut bytes = Bytes::from_static(&[
            0x09, // properties length
            0x0B, // SubscriptionIdentifier property
            0x01, // varint value = 1
            0x26, // UserProperty property
            0x00, 0x01, b'k', // key
            0x00, 0x01, b'v', // value
        ]);

        let properties = SubscribeProperties::read(&mut bytes)
            .unwrap()
            .expect("properties should be present");

        assert_eq!(properties.id, Some(1));
        assert_eq!(
            properties.user_properties,
            vec![("k".to_owned(), "v".to_owned())]
        );
    }
}
