use super::{
    BufMut, BytesMut, Error, FixedHeader, PropertyType, QoS, len_len, length, property, qos,
    read_mqtt_bytes, read_mqtt_string, read_u8, read_u16, read_u32, validate_mqtt_string,
    write_mqtt_bytes, write_mqtt_string, write_remaining_length,
};
use bytes::{Buf, Bytes};

type ConnectReadParts = (Connect, Option<LastWill>, ConnectAuth);

/// Connection packet initiated by the client
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connect {
    /// Mqtt keep alive time
    pub keep_alive: u16,
    /// Client Id
    pub client_id: String,
    /// Clean session. Asks the broker to clear previous state
    pub clean_start: bool,
    pub properties: Option<ConnectProperties>,
}

impl Connect {
    pub fn read(fixed_header: FixedHeader, mut bytes: Bytes) -> Result<ConnectReadParts, Error> {
        let variable_header_index = fixed_header.header_len;
        bytes.advance(variable_header_index);

        // Variable header
        let protocol_name = read_mqtt_string(&mut bytes)?;
        let protocol_level = read_u8(&mut bytes)?;
        if protocol_name != "MQTT" {
            return Err(Error::InvalidProtocol);
        }

        if protocol_level != 5 {
            return Err(Error::InvalidProtocolLevel(protocol_level));
        }

        let connect_flags = read_u8(&mut bytes)?;
        validate_connect_flags(connect_flags)?;
        let clean_start = (connect_flags & 0b10) != 0;
        let keep_alive = read_u16(&mut bytes)?;

        let properties = ConnectProperties::read(&mut bytes)?;

        let client_id = read_mqtt_string(&mut bytes)?;
        let will = LastWill::read(connect_flags, &mut bytes)?;
        let auth = ConnectAuth::read(connect_flags, &mut bytes)?;

        if bytes.has_remaining() {
            return Err(Error::IncorrectPacketFormat);
        }

        let connect = Self {
            keep_alive,
            client_id,
            clean_start,
            properties,
        };

        Ok((connect, will, auth))
    }

    fn len(&self, will: Option<&LastWill>, auth: &ConnectAuth) -> usize {
        let mut len = 2 + "MQTT".len() // protocol name
                        + 1            // protocol version
                        + 1            // connect flags
                        + 2; // keep alive

        if let Some(p) = &self.properties {
            let properties_len = p.len();
            let properties_len_len = len_len(properties_len);
            len += properties_len_len + properties_len;
        } else {
            // just 1 byte representing 0 len
            len += 1;
        }

        len += 2 + self.client_id.len();

        // last will len
        if let Some(w) = will {
            len += w.len();
        }

        // username and password len
        len += auth.len();

        len
    }

    pub fn write(
        &self,
        will: &Option<LastWill>,
        auth: &ConnectAuth,
        buffer: &mut BytesMut,
    ) -> Result<usize, Error> {
        let len = self.len(will.as_ref(), auth);

        buffer.put_u8(0b0001_0000);
        let count = write_remaining_length(buffer, len)?;
        write_mqtt_string(buffer, "MQTT")?;

        buffer.put_u8(0x05);
        let flags_index = 1 + count + 2 + 4 + 1;

        let mut connect_flags = 0;
        if self.clean_start {
            connect_flags |= 0x02;
        }

        buffer.put_u8(connect_flags);
        buffer.put_u16(self.keep_alive);

        match &self.properties {
            Some(p) => p.write(buffer)?,
            None => {
                write_remaining_length(buffer, 0)?;
            }
        }

        write_mqtt_string(buffer, &self.client_id)?;

        if let Some(w) = will {
            connect_flags |= w.write(buffer)?;
        }

        connect_flags |= auth.write(buffer)?;

        // update connect flags
        buffer[flags_index] = connect_flags;
        Ok(1 + count + len)
    }

    pub fn size(&self, will: &Option<LastWill>, auth: &ConnectAuth) -> usize {
        let len = self.len(will.as_ref(), auth);
        let remaining_len_size = len_len(len);

        1 + remaining_len_size + len
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectProperties {
    /// Expiry interval property after loosing connection
    pub session_expiry_interval: Option<u32>,
    /// Maximum simultaneous packets
    pub receive_maximum: Option<u16>,
    /// Maximum packet size
    pub max_packet_size: Option<u32>,
    /// Maximum mapping integer for a topic
    pub topic_alias_max: Option<u16>,
    pub request_response_info: Option<u8>,
    pub request_problem_info: Option<u8>,
    /// List of user properties
    pub user_properties: Vec<(String, String)>,
    /// Method of authentication
    pub authentication_method: Option<String>,
    /// Authentication data
    pub authentication_data: Option<Bytes>,
}

impl ConnectProperties {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            session_expiry_interval: None,
            receive_maximum: None,
            max_packet_size: None,
            topic_alias_max: None,
            request_response_info: None,
            request_problem_info: None,
            user_properties: Vec::new(),
            authentication_method: None,
            authentication_data: None,
        }
    }

    pub fn read(bytes: &mut Bytes) -> Result<Option<Self>, Error> {
        let mut session_expiry_interval = None;
        let mut receive_maximum = None;
        let mut max_packet_size = None;
        let mut topic_alias_max = None;
        let mut request_response_info = None;
        let mut request_problem_info = None;
        let mut user_properties = Vec::new();
        let mut authentication_method = None;
        let mut authentication_data = None;

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
                PropertyType::SessionExpiryInterval => {
                    session_expiry_interval = Some(read_u32(bytes)?);
                    cursor += 4;
                }
                PropertyType::ReceiveMaximum => {
                    let receive_max = read_u16(bytes)?;
                    if receive_max == 0 {
                        return Err(Error::ProtocolError);
                    }
                    receive_maximum = Some(receive_max);
                    cursor += 2;
                }
                PropertyType::MaximumPacketSize => {
                    max_packet_size = Some(read_u32(bytes)?);
                    cursor += 4;
                }
                PropertyType::TopicAliasMaximum => {
                    topic_alias_max = Some(read_u16(bytes)?);
                    cursor += 2;
                }
                PropertyType::RequestResponseInformation => {
                    if request_response_info.is_some() {
                        return Err(Error::ProtocolError);
                    }
                    let value = read_u8(bytes)?;
                    // [MQTT-3.1.2-28] Request Response Information is a Byte
                    // Identifier that MUST be 0 or 1.
                    if value > 1 {
                        return Err(Error::ProtocolError);
                    }
                    request_response_info = Some(value);
                    cursor += 1;
                }
                PropertyType::RequestProblemInformation => {
                    if request_problem_info.is_some() {
                        return Err(Error::ProtocolError);
                    }
                    let value = read_u8(bytes)?;
                    // [MQTT-3.1.2-29] Request Problem Information is a Byte
                    // Identifier that MUST be 0 or 1.
                    if value > 1 {
                        return Err(Error::ProtocolError);
                    }
                    request_problem_info = Some(value);
                    cursor += 1;
                }
                PropertyType::UserProperty => {
                    let key = read_mqtt_string(bytes)?;
                    let value = read_mqtt_string(bytes)?;
                    cursor += 2 + key.len() + 2 + value.len();
                    user_properties.push((key, value));
                }
                PropertyType::AuthenticationMethod => {
                    let method = read_mqtt_string(bytes)?;
                    cursor += 2 + method.len();
                    authentication_method = Some(method);
                }
                PropertyType::AuthenticationData => {
                    let data = read_mqtt_bytes(bytes)?;
                    cursor += 2 + data.len();
                    authentication_data = Some(data);
                }
                _ => return Err(Error::InvalidPropertyType(prop)),
            }
        }

        Ok(Some(Self {
            session_expiry_interval,
            receive_maximum,
            max_packet_size,
            topic_alias_max,
            request_response_info,
            request_problem_info,
            user_properties,
            authentication_method,
            authentication_data,
        }))
    }

    fn len(&self) -> usize {
        let mut len = 0;

        if self.session_expiry_interval.is_some() {
            len += 1 + 4;
        }

        if self.receive_maximum.is_some() {
            len += 1 + 2;
        }

        if self.max_packet_size.is_some() {
            len += 1 + 4;
        }

        if self.topic_alias_max.is_some() {
            len += 1 + 2;
        }

        if self.request_response_info.is_some() {
            len += 1 + 1;
        }

        if self.request_problem_info.is_some() {
            len += 1 + 1;
        }

        for (key, value) in &self.user_properties {
            len += 1 + 2 + key.len() + 2 + value.len();
        }

        if let Some(authentication_method) = &self.authentication_method {
            len += 1 + 2 + authentication_method.len();
        }

        if let Some(authentication_data) = &self.authentication_data {
            len += 1 + 2 + authentication_data.len();
        }

        len
    }

    pub fn write(&self, buffer: &mut BytesMut) -> Result<(), Error> {
        let len = self.len();
        write_remaining_length(buffer, len)?;

        if let Some(session_expiry_interval) = self.session_expiry_interval {
            buffer.put_u8(PropertyType::SessionExpiryInterval as u8);
            buffer.put_u32(session_expiry_interval);
        }

        if let Some(receive_maximum) = self.receive_maximum {
            buffer.put_u8(PropertyType::ReceiveMaximum as u8);
            buffer.put_u16(receive_maximum);
        }

        if let Some(max_packet_size) = self.max_packet_size {
            buffer.put_u8(PropertyType::MaximumPacketSize as u8);
            buffer.put_u32(max_packet_size);
        }

        if let Some(topic_alias_max) = self.topic_alias_max {
            buffer.put_u8(PropertyType::TopicAliasMaximum as u8);
            buffer.put_u16(topic_alias_max);
        }

        if let Some(request_response_info) = self.request_response_info {
            // [MQTT-3.1.2-28] Request Response Information is a Byte
            // Identifier that MUST be 0 or 1.
            if request_response_info > 1 {
                return Err(Error::ProtocolError);
            }
            buffer.put_u8(PropertyType::RequestResponseInformation as u8);
            buffer.put_u8(request_response_info);
        }

        if let Some(request_problem_info) = self.request_problem_info {
            // [MQTT-3.1.2-29] Request Problem Information is a Byte
            // Identifier that MUST be 0 or 1.
            if request_problem_info > 1 {
                return Err(Error::ProtocolError);
            }
            buffer.put_u8(PropertyType::RequestProblemInformation as u8);
            buffer.put_u8(request_problem_info);
        }

        for (key, value) in &self.user_properties {
            buffer.put_u8(PropertyType::UserProperty as u8);
            write_mqtt_string(buffer, key)?;
            write_mqtt_string(buffer, value)?;
        }

        if let Some(authentication_method) = &self.authentication_method {
            buffer.put_u8(PropertyType::AuthenticationMethod as u8);
            write_mqtt_string(buffer, authentication_method)?;
        }

        if let Some(authentication_data) = &self.authentication_data {
            buffer.put_u8(PropertyType::AuthenticationData as u8);
            write_mqtt_bytes(buffer, authentication_data)?;
        }

        Ok(())
    }
}

impl Default for ConnectProperties {
    fn default() -> Self {
        Self::new()
    }
}

/// `LastWill` that broker forwards on behalf of the client
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastWill {
    pub topic: Bytes,
    pub message: Bytes,
    pub qos: QoS,
    pub retain: bool,
    pub properties: Option<LastWillProperties>,
}

impl LastWill {
    pub fn new(
        topic: impl Into<String>,
        payload: impl Into<Vec<u8>>,
        qos: QoS,
        retain: bool,
        properties: Option<LastWillProperties>,
    ) -> Self {
        let topic = Bytes::from(topic.into().into_bytes());
        Self {
            topic,
            message: Bytes::from(payload.into()),
            qos,
            retain,
            properties,
        }
    }

    fn len(&self) -> usize {
        let mut len = 0;

        if let Some(p) = &self.properties {
            let properties_len = p.len();
            let properties_len_len = len_len(properties_len);
            len += properties_len_len + properties_len;
        } else {
            // just 1 byte representing 0 len
            len += 1;
        }

        len += 2 + self.topic.len() + 2 + self.message.len();
        len
    }

    pub fn read(connect_flags: u8, bytes: &mut Bytes) -> Result<Option<Self>, Error> {
        let o = match connect_flags & 0b100 {
            0 if (connect_flags & 0b0011_1000) != 0 => {
                return Err(Error::IncorrectPacketFormat);
            }
            0 => None,
            _ => {
                // Properties in variable header
                let properties = LastWillProperties::read(bytes)?;

                let will_topic = read_mqtt_bytes(bytes)?;
                validate_mqtt_string(&will_topic)?;
                let will_message = read_mqtt_bytes(bytes)?;
                let qos_num = (connect_flags & 0b11000) >> 3;
                let will_qos = qos(qos_num).ok_or(Error::InvalidQoS(qos_num))?;
                Some(Self {
                    topic: will_topic,
                    message: will_message,
                    qos: will_qos,
                    retain: (connect_flags & 0b0010_0000) != 0,
                    properties,
                })
            }
        };

        Ok(o)
    }

    pub fn write(&self, buffer: &mut BytesMut) -> Result<u8, Error> {
        let mut connect_flags = 0;

        connect_flags |= 0x04 | ((self.qos as u8) << 3);
        if self.retain {
            connect_flags |= 0x20;
        }

        if let Some(p) = &self.properties {
            p.write(buffer)?;
        } else {
            write_remaining_length(buffer, 0)?;
        }

        validate_mqtt_string(&self.topic)?;
        write_mqtt_bytes(buffer, &self.topic)?;
        write_mqtt_bytes(buffer, &self.message)?;
        Ok(connect_flags)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastWillProperties {
    pub delay_interval: Option<u32>,
    pub payload_format_indicator: Option<u8>,
    pub message_expiry_interval: Option<u32>,
    pub content_type: Option<String>,
    pub response_topic: Option<String>,
    pub correlation_data: Option<Bytes>,
    pub user_properties: Vec<(String, String)>,
}

impl LastWillProperties {
    fn len(&self) -> usize {
        let mut len = 0;

        if self.delay_interval.is_some() {
            len += 1 + 4;
        }

        if self.payload_format_indicator.is_some() {
            len += 1 + 1;
        }

        if self.message_expiry_interval.is_some() {
            len += 1 + 4;
        }

        if let Some(typ) = &self.content_type {
            len += 1 + 2 + typ.len();
        }

        if let Some(topic) = &self.response_topic {
            len += 1 + 2 + topic.len();
        }

        if let Some(data) = &self.correlation_data {
            len += 1 + 2 + data.len();
        }

        for (key, value) in &self.user_properties {
            len += 1 + 2 + key.len() + 2 + value.len();
        }

        len
    }

    pub fn read(bytes: &mut Bytes) -> Result<Option<Self>, Error> {
        let mut delay_interval = None;
        let mut payload_format_indicator = None;
        let mut message_expiry_interval = None;
        let mut content_type = None;
        let mut response_topic = None;
        let mut correlation_data = None;
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
                PropertyType::WillDelayInterval => {
                    delay_interval = Some(read_u32(bytes)?);
                    cursor += 4;
                }
                PropertyType::PayloadFormatIndicator => {
                    payload_format_indicator = Some(read_u8(bytes)?);
                    cursor += 1;
                }
                PropertyType::MessageExpiryInterval => {
                    message_expiry_interval = Some(read_u32(bytes)?);
                    cursor += 4;
                }
                PropertyType::ContentType => {
                    let typ = read_mqtt_string(bytes)?;
                    cursor += 2 + typ.len();
                    content_type = Some(typ);
                }
                PropertyType::ResponseTopic => {
                    let topic = read_mqtt_string(bytes)?;
                    cursor += 2 + topic.len();
                    response_topic = Some(topic);
                }
                PropertyType::CorrelationData => {
                    let data = read_mqtt_bytes(bytes)?;
                    cursor += 2 + data.len();
                    correlation_data = Some(data);
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
            delay_interval,
            payload_format_indicator,
            message_expiry_interval,
            content_type,
            response_topic,
            correlation_data,
            user_properties,
        }))
    }

    pub fn write(&self, buffer: &mut BytesMut) -> Result<(), Error> {
        let len = self.len();
        write_remaining_length(buffer, len)?;

        if let Some(delay_interval) = self.delay_interval {
            buffer.put_u8(PropertyType::WillDelayInterval as u8);
            buffer.put_u32(delay_interval);
        }

        if let Some(payload_format_indicator) = self.payload_format_indicator {
            buffer.put_u8(PropertyType::PayloadFormatIndicator as u8);
            buffer.put_u8(payload_format_indicator);
        }

        if let Some(message_expiry_interval) = self.message_expiry_interval {
            buffer.put_u8(PropertyType::MessageExpiryInterval as u8);
            buffer.put_u32(message_expiry_interval);
        }

        if let Some(typ) = &self.content_type {
            buffer.put_u8(PropertyType::ContentType as u8);
            write_mqtt_string(buffer, typ)?;
        }

        if let Some(topic) = &self.response_topic {
            buffer.put_u8(PropertyType::ResponseTopic as u8);
            write_mqtt_string(buffer, topic)?;
        }

        if let Some(data) = &self.correlation_data {
            buffer.put_u8(PropertyType::CorrelationData as u8);
            write_mqtt_bytes(buffer, data)?;
        }

        for (key, value) in &self.user_properties {
            buffer.put_u8(PropertyType::UserProperty as u8);
            write_mqtt_string(buffer, key)?;
            write_mqtt_string(buffer, value)?;
        }

        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnectAuth {
    #[default]
    None,
    Username {
        username: String,
    },
    Password {
        password: Bytes,
    },
    UsernamePassword {
        username: String,
        password: Bytes,
    },
}

impl ConnectAuth {
    pub fn read(connect_flags: u8, bytes: &mut Bytes) -> Result<Self, Error> {
        let username_flag = (connect_flags & 0b1000_0000) != 0;
        let password_flag = (connect_flags & 0b0100_0000) != 0;

        match (username_flag, password_flag) {
            (false, false) => Ok(Self::None),
            (true, false) => Ok(Self::Username {
                username: read_mqtt_string(bytes)?,
            }),
            (false, true) => Ok(Self::Password {
                password: read_mqtt_bytes(bytes)?,
            }),
            (true, true) => Ok(Self::UsernamePassword {
                username: read_mqtt_string(bytes)?,
                password: read_mqtt_bytes(bytes)?,
            }),
        }
    }

    const fn len(&self) -> usize {
        match self {
            Self::None => 0,
            Self::Username { username } => 2 + username.len(),
            Self::Password { password } => 2 + password.len(),
            Self::UsernamePassword { username, password } => {
                2 + username.len() + 2 + password.len()
            }
        }
    }

    pub fn write(&self, buffer: &mut BytesMut) -> Result<u8, Error> {
        let flags = match self {
            Self::None => 0,
            Self::Username { username } => {
                write_mqtt_string(buffer, username)?;
                0x80
            }
            Self::Password { password } => {
                write_mqtt_bytes(buffer, password.as_ref())?;
                0x40
            }
            Self::UsernamePassword { username, password } => {
                write_mqtt_string(buffer, username)?;
                write_mqtt_bytes(buffer, password.as_ref())?;
                0xC0
            }
        };

        Ok(flags)
    }
}

const fn validate_connect_flags(connect_flags: u8) -> Result<(), Error> {
    if (connect_flags & 0x01) != 0 {
        return Err(Error::IncorrectPacketFormat);
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use super::super::test::{USER_PROP_KEY, USER_PROP_VAL};
    use super::*;
    use crate::mqttbytes::v5::parse_fixed_header;
    use bytes::Bytes;
    use bytes::BytesMut;
    use pretty_assertions::assert_eq;

    #[test]
    fn length_calculation() {
        let mut dummy_bytes = BytesMut::new();
        let mut connect_props = ConnectProperties::new();
        // Use user_properties to pad the size to exceed ~128 bytes to make the
        // remaining_length field in the packet be 2 bytes long.
        connect_props.user_properties = vec![(USER_PROP_KEY.into(), USER_PROP_VAL.into())];
        let connect_pkt = Connect {
            keep_alive: 5,
            client_id: "client".into(),
            clean_start: true,
            properties: Some(connect_props),
        };

        let reported_size = connect_pkt
            .write(&None, &ConnectAuth::None, &mut dummy_bytes)
            .unwrap();
        let size_from_bytes = dummy_bytes.len();

        assert_eq!(reported_size, size_from_bytes);
    }

    #[test]
    fn write_uses_mqtt_protocol_name() {
        let connect_pkt = Connect {
            keep_alive: 5,
            client_id: "client".into(),
            clean_start: true,
            properties: None,
        };

        let mut bytes = BytesMut::new();
        connect_pkt
            .write(&None, &ConnectAuth::None, &mut bytes)
            .unwrap();

        assert_eq!(&bytes[2..8], &[0x00, 0x04, b'M', b'Q', b'T', b'T']);
    }

    /// MQTT-3.1.2-2: Protocol Version for v5 MUST be 5 (0x05).
    #[test]
    fn connect_parsing_rejects_invalid_protocol_level() {
        let packetstream: Vec<u8> = [
            0x10, 0x10, // packet type, flags and remaining len
            0x00, 0x04, b'M', b'Q', b'T', b'T',
            0x04, // protocol name "MQTT" + invalid level 4
            0x02, // connect flags: clean start only
            0x00, 0x0a, // keep alive = 10
            0x00, // properties length = 0
            0x00, 0x04, b't', b'e', b's', b't', // client_id = "test"
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::InvalidProtocolLevel(4))));
    }

    /// MQTT-3.1.2-2: Protocol Version for v5 MUST be 5 (0x05).
    #[test]
    fn connect_encoding_emits_protocol_level_5() {
        let connect_pkt = Connect {
            keep_alive: 5,
            client_id: "client".into(),
            clean_start: true,
            properties: None,
        };

        let mut bytes = BytesMut::new();
        connect_pkt
            .write(&None, &ConnectAuth::None, &mut bytes)
            .unwrap();

        // After fixed header (1 byte type + 1 byte remaining length) and
        // protocol name (2 byte length + 4 byte "MQTT"), the protocol level
        // byte sits at index 8.
        assert_eq!(bytes[8], 0x05);
    }

    #[test]
    fn connect_roundtrips_max_keep_alive() {
        let connect_pkt = Connect {
            keep_alive: u16::MAX,
            client_id: "client".into(),
            clean_start: true,
            properties: None,
        };

        let mut bytes = BytesMut::new();
        connect_pkt
            .write(&None, &ConnectAuth::None, &mut bytes)
            .unwrap();

        let fixed_header = parse_fixed_header(bytes.iter()).unwrap();
        let (connect, will, auth) = Connect::read(fixed_header, bytes.freeze()).unwrap();

        assert_eq!(connect.keep_alive, u16::MAX);
        assert_eq!(will, None);
        assert_eq!(auth, ConnectAuth::None);
    }

    #[test]
    fn connect_roundtrips_multibyte_utf8_client_id() {
        let client_id = "cl-日本語-🔑".to_owned();
        let connect_pkt = Connect {
            keep_alive: 10,
            client_id: client_id.clone(),
            clean_start: true,
            properties: None,
        };

        let mut bytes = BytesMut::new();
        connect_pkt
            .write(&None, &ConnectAuth::None, &mut bytes)
            .unwrap();

        let fixed_header = parse_fixed_header(bytes.iter()).unwrap();
        let (connect, will, auth) = Connect::read(fixed_header, bytes.freeze()).unwrap();

        assert_eq!(connect.client_id, client_id);
        assert_eq!(will, None);
        assert_eq!(auth, ConnectAuth::None);
    }

    /// MQTT-3.1.3-5: the server-required acceptance profile (1-23 UTF-8 bytes,
    /// ASCII alphanumeric only) is a server-side obligation, not a client-side
    /// outbound restriction. The codec must not reject a ClientID merely for
    /// being longer than 23 bytes or for containing characters outside that
    /// 62-character set.
    #[test]
    fn connect_encoding_allows_broad_client_ids() {
        let client_id = "client-id_with.symbols/and/unicode-é".to_owned();
        assert!(client_id.len() > 23);
        let connect_pkt = Connect {
            keep_alive: 10,
            client_id: client_id.clone(),
            clean_start: true,
            properties: None,
        };

        let mut bytes = BytesMut::new();
        connect_pkt
            .write(&None, &ConnectAuth::None, &mut bytes)
            .unwrap();

        let fixed_header = parse_fixed_header(bytes.iter()).unwrap();
        let (connect, will, auth) = Connect::read(fixed_header, bytes.freeze()).unwrap();

        assert_eq!(connect.client_id, client_id);
        assert_eq!(will, None);
        assert_eq!(auth, ConnectAuth::None);
    }

    #[test]
    fn connect_encoding_rejects_client_id_larger_than_u16() {
        let connect_pkt = Connect {
            keep_alive: 5,
            client_id: "a".repeat(usize::from(u16::MAX) + 1),
            clean_start: true,
            properties: None,
        };

        let mut bytes = BytesMut::new();
        let result = connect_pkt.write(&None, &ConnectAuth::None, &mut bytes);

        assert!(matches!(result, Err(Error::PayloadTooLong)));
        assert!(
            !bytes.is_empty(),
            "CONNECT encoding reports the protocol error after writing the packet prefix"
        );
    }

    /// MQTT-3.1.2-3: CONNECT flags bit 0 is reserved and must encode as zero.
    #[test]
    fn connect_encoding_emits_zero_reserved_connect_flag_bit() {
        let connect_pkt = Connect {
            keep_alive: 5,
            client_id: "client".into(),
            clean_start: true,
            properties: None,
        };
        let will = Some(LastWill {
            topic: "/a".into(),
            message: Bytes::from_static(b"offline"),
            qos: QoS::AtLeastOnce,
            retain: true,
            properties: None,
        });
        let auth = ConnectAuth::UsernamePassword {
            username: "rust".to_owned(),
            password: Bytes::from_static(b"mq"),
        };

        let mut bytes = BytesMut::new();
        connect_pkt.write(&will, &auth, &mut bytes).unwrap();

        assert_eq!(bytes[9] & 0x01, 0);
    }

    /// MQTT-3.1.2-4: Clean Start=1 MUST encode into bit 1 (0x02) of the CONNECT flags.
    #[test]
    fn connect_encoding_sets_clean_start_bit_when_true() {
        let connect_pkt = Connect {
            keep_alive: 5,
            client_id: "client".into(),
            clean_start: true,
            properties: None,
        };

        let mut bytes = BytesMut::new();
        connect_pkt
            .write(&None, &ConnectAuth::None, &mut bytes)
            .unwrap();

        // CONNECT flags byte sits at index 9 for this packet. Clean Start is bit 1 (0x02).
        assert_eq!(bytes[9] & 0x02, 0x02);
    }

    /// MQTT-3.1.2-4: Clean Start=0 MUST leave bit 1 (0x02) clear and survive a round-trip.
    #[test]
    fn connect_encoding_clears_clean_start_bit_when_false() {
        let connect_pkt = Connect {
            keep_alive: 5,
            client_id: "client".into(),
            clean_start: false,
            properties: None,
        };

        let mut bytes = BytesMut::new();
        connect_pkt
            .write(&None, &ConnectAuth::None, &mut bytes)
            .unwrap();

        // CONNECT flags byte sits at index 9 for this packet. Clean Start is bit 1 (0x02).
        assert_eq!(bytes[9] & 0x02, 0);

        let fixed_header = parse_fixed_header(bytes.iter()).unwrap();
        let (connect, will, auth) = Connect::read(fixed_header, bytes.freeze()).unwrap();

        assert_eq!(connect.clean_start, false);
        assert_eq!(will, None);
        assert_eq!(auth, ConnectAuth::None);
    }

    #[test]
    fn connect_roundtrips_last_will() {
        let connect_pkt = Connect {
            keep_alive: 5,
            client_id: "client".into(),
            clean_start: true,
            properties: None,
        };
        let will = Some(LastWill {
            topic: "/a".into(),
            message: Bytes::from_static(b"offline"),
            qos: QoS::AtLeastOnce,
            retain: true,
            properties: None,
        });

        let mut bytes = BytesMut::new();
        connect_pkt
            .write(&will, &ConnectAuth::None, &mut bytes)
            .unwrap();

        let fixed_header = parse_fixed_header(bytes.iter()).unwrap();
        let connect_bytes = bytes.split_to(fixed_header.frame_length()).freeze();
        let (_, decoded_will, decoded_auth) = Connect::read(fixed_header, connect_bytes).unwrap();

        assert_eq!(decoded_will, will);
        assert_eq!(decoded_auth, ConnectAuth::None);
    }

    #[test]
    fn read_rejects_receive_maximum_zero() {
        let mut bytes = Bytes::from_static(&[
            0x03, // properties length
            0x21, // ReceiveMaximum property
            0x00, 0x00, // value = 0
        ]);
        let result = ConnectProperties::read(&mut bytes);

        assert!(matches!(result, Err(Error::ProtocolError)));
    }

    /// [MQTT-3.1.2-28] Request Response Information MUST be 0 or 1.
    #[test]
    fn read_rejects_request_response_info_greater_than_one() {
        let mut bytes = Bytes::from_static(&[
            0x02, // properties length
            PropertyType::RequestResponseInformation as u8,
            0x02, // value = 2 (invalid)
        ]);
        let result = ConnectProperties::read(&mut bytes);

        assert!(matches!(result, Err(Error::ProtocolError)));
    }

    /// [MQTT-3.1.2-28] Request Response Information MUST NOT appear more than once.
    #[test]
    fn read_rejects_duplicate_request_response_info() {
        let mut bytes = Bytes::from_static(&[
            0x04, // properties length
            PropertyType::RequestResponseInformation as u8,
            0x00,
            PropertyType::RequestResponseInformation as u8,
            0x01,
        ]);
        let result = ConnectProperties::read(&mut bytes);

        assert!(matches!(result, Err(Error::ProtocolError)));
    }

    /// [MQTT-3.1.2-28] Request Response Information MUST be 0 or 1.
    #[test]
    fn write_rejects_request_response_info_greater_than_one() {
        let mut props = ConnectProperties::new();
        props.request_response_info = Some(2);
        let mut buffer = BytesMut::new();
        let result = props.write(&mut buffer);

        assert!(matches!(result, Err(Error::ProtocolError)));
    }

    /// [MQTT-3.1.2-28] Request Response Information MUST be 0 or 1.
    #[test]
    fn read_accepts_request_response_info_zero() {
        let mut bytes = Bytes::from_static(&[
            0x02, // properties length
            PropertyType::RequestResponseInformation as u8,
            0x00, // value = 0
        ]);
        let result = ConnectProperties::read(&mut bytes);

        let props = result.unwrap().unwrap();
        assert_eq!(props.request_response_info, Some(0));
    }

    /// [MQTT-3.1.2-28] Request Response Information MUST be 0 or 1.
    #[test]
    fn read_accepts_request_response_info_one() {
        let mut bytes = Bytes::from_static(&[
            0x02, // properties length
            PropertyType::RequestResponseInformation as u8,
            0x01, // value = 1
        ]);
        let result = ConnectProperties::read(&mut bytes);

        let props = result.unwrap().unwrap();
        assert_eq!(props.request_response_info, Some(1));
    }

    /// [MQTT-3.1.2-29] Request Problem Information MUST be 0 or 1.
    #[test]
    fn read_rejects_request_problem_info_greater_than_one() {
        let mut bytes = Bytes::from_static(&[
            0x02, // properties length
            PropertyType::RequestProblemInformation as u8,
            0x02, // value = 2 (invalid)
        ]);
        let result = ConnectProperties::read(&mut bytes);

        assert!(matches!(result, Err(Error::ProtocolError)));
    }

    /// [MQTT-3.1.2-29] Request Problem Information MUST NOT appear more than once.
    #[test]
    fn read_rejects_duplicate_request_problem_info() {
        let mut bytes = Bytes::from_static(&[
            0x04, // properties length
            PropertyType::RequestProblemInformation as u8,
            0x00,
            PropertyType::RequestProblemInformation as u8,
            0x01,
        ]);
        let result = ConnectProperties::read(&mut bytes);

        assert!(matches!(result, Err(Error::ProtocolError)));
    }

    /// [MQTT-3.1.2-29] Request Problem Information MUST be 0 or 1.
    #[test]
    fn write_rejects_request_problem_info_greater_than_one() {
        let mut props = ConnectProperties::new();
        props.request_problem_info = Some(2);
        let mut buffer = BytesMut::new();
        let result = props.write(&mut buffer);

        assert!(matches!(result, Err(Error::ProtocolError)));
    }

    /// [MQTT-3.1.2-29] Request Problem Information MUST be 0 or 1.
    #[test]
    fn read_accepts_request_problem_info_zero() {
        let mut bytes = Bytes::from_static(&[
            0x02, // properties length
            PropertyType::RequestProblemInformation as u8,
            0x00, // value = 0
        ]);
        let result = ConnectProperties::read(&mut bytes);

        let props = result.unwrap().unwrap();
        assert_eq!(props.request_problem_info, Some(0));
    }

    /// [MQTT-3.1.2-29] Request Problem Information MUST be 0 or 1.
    #[test]
    fn read_accepts_request_problem_info_one() {
        let mut bytes = Bytes::from_static(&[
            0x02, // properties length
            PropertyType::RequestProblemInformation as u8,
            0x01, // value = 1
        ]);
        let result = ConnectProperties::read(&mut bytes);

        let props = result.unwrap().unwrap();
        assert_eq!(props.request_problem_info, Some(1));
    }

    #[test]
    fn last_will_read_rejects_topic_with_invalid_utf8() {
        let mut bytes = Bytes::from_static(&[
            0x00, // properties length
            0x00, 0x01, // topic length
            0xff, // invalid UTF-8
            0x00, 0x00, // payload length
        ]);
        let result = LastWill::read(0x04, &mut bytes);

        assert!(matches!(result, Err(Error::TopicNotUtf8 { .. })));
    }

    #[test]
    fn last_will_read_rejects_topic_with_null_character() {
        let mut bytes = Bytes::from_static(&[
            0x00, // properties length
            0x00, 0x03, // topic length
            b'a', 0x00, b'b', // topic
            0x00, 0x00, // payload length
        ]);
        let result = LastWill::read(0x04, &mut bytes);

        assert!(matches!(result, Err(Error::MalformedPacket)));
    }

    #[test]
    fn last_will_write_rejects_topic_with_null_character() {
        let will = LastWill {
            topic: Bytes::from_static(b"a\0b"),
            message: Bytes::new(),
            qos: QoS::AtMostOnce,
            retain: false,
            properties: None,
        };
        let mut buffer = BytesMut::new();
        let result = will.write(&mut buffer);

        assert!(matches!(result, Err(Error::MalformedPacket)));
    }

    #[test]
    fn last_will_write_rejects_topic_with_invalid_utf8() {
        let will = LastWill {
            topic: Bytes::from_static(&[0xff]),
            message: Bytes::new(),
            qos: QoS::AtMostOnce,
            retain: false,
            properties: None,
        };
        let mut buffer = BytesMut::new();
        let result = will.write(&mut buffer);

        assert!(matches!(result, Err(Error::TopicNotUtf8 { .. })));
    }

    #[test]
    fn last_will_properties_roundtrips_delay_interval() {
        let properties = LastWillProperties {
            delay_interval: Some(30),
            payload_format_indicator: None,
            message_expiry_interval: None,
            content_type: None,
            response_topic: None,
            correlation_data: None,
            user_properties: Vec::new(),
        };
        let mut buffer = BytesMut::new();

        properties.write(&mut buffer).unwrap();

        assert_eq!(
            &buffer[..],
            &[
                0x05,
                PropertyType::WillDelayInterval as u8,
                0x00,
                0x00,
                0x00,
                0x1e
            ]
        );

        let mut bytes = buffer.freeze();
        let decoded = LastWillProperties::read(&mut bytes).unwrap().unwrap();

        assert_eq!(decoded.delay_interval, Some(30));
        assert!(decoded.user_properties.is_empty());
    }

    #[test]
    fn last_will_properties_roundtrips_user_properties_in_order() {
        let properties = LastWillProperties {
            delay_interval: None,
            payload_format_indicator: None,
            message_expiry_interval: None,
            content_type: None,
            response_topic: None,
            correlation_data: None,
            user_properties: vec![
                ("first".to_owned(), "1".to_owned()),
                ("second".to_owned(), "2".to_owned()),
            ],
        };
        let mut buffer = BytesMut::new();

        properties.write(&mut buffer).unwrap();

        let expected = [
            0x17,
            PropertyType::UserProperty as u8,
            0x00,
            0x05,
            b'f',
            b'i',
            b'r',
            b's',
            b't',
            0x00,
            0x01,
            b'1',
            PropertyType::UserProperty as u8,
            0x00,
            0x06,
            b's',
            b'e',
            b'c',
            b'o',
            b'n',
            b'd',
            0x00,
            0x01,
            b'2',
        ];
        assert_eq!(&buffer[..], &expected);

        let mut bytes = buffer.freeze();
        let decoded = LastWillProperties::read(&mut bytes).unwrap().unwrap();

        assert_eq!(decoded.user_properties, properties.user_properties);
    }

    /// [MQTT-3.1.3-4] / [MQTT-1.5.4-2]: The ClientID MUST be a UTF-8 Encoded
    /// String. Verify that decoding a CONNECT with U+0000 in the client_id
    /// is rejected.
    #[test]
    fn connect_parsing_rejects_null_character_in_client_id() {
        // CONNECT with client_id containing U+0000
        // remaining = var_header(10) + props_len(1) + client_id(2+3) = 16
        let packetstream: Vec<u8> = [
            0x10, 16, // packet type, flags and remaining len
            0x00, 0x04, b'M', b'Q', b'T', b'T', 0x05, // protocol name + level
            0x02, // connect flags: clean start only
            0x00, 0x0a, // keep alive = 10
            0x00, // properties length = 0
            0x00, 0x03, // client_id length = 3
            b'a', 0x00, b'b', // client_id = "a\0b"
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::MalformedPacket)));
    }

    /// [MQTT-3.1.3-4] / [MQTT-1.5.4-1]: The ClientID MUST be a UTF-8 Encoded
    /// String. Verify that encoding a CONNECT with U+0000 in the client_id
    /// is rejected.
    #[test]
    fn connect_encoding_rejects_null_character_in_client_id() {
        let connect_pkt = Connect {
            keep_alive: 10,
            client_id: "a\0b".to_owned(),
            clean_start: true,
            properties: None,
        };

        let mut buf = BytesMut::new();
        let result = connect_pkt.write(&None, &ConnectAuth::None, &mut buf);

        assert!(matches!(result, Err(Error::MalformedPacket)));
    }

    /// [MQTT-3.1.3-4] / [MQTT-1.5.4-1]: The ClientID MUST be a UTF-8 Encoded
    /// String. Verify that decoding a CONNECT with a surrogate code point
    /// (U+D800) in the client_id is rejected.
    #[test]
    fn connect_parsing_rejects_surrogate_in_client_id() {
        // CONNECT with client_id containing U+D800 (CESU-8: 0xED 0xA0 0x80)
        // remaining = var_header(10) + props_len(1) + client_id(2+3) = 16
        let packetstream: Vec<u8> = [
            0x10, 16, // packet type, flags and remaining len
            0x00, 0x04, b'M', b'Q', b'T', b'T', 0x05, // protocol name + level
            0x02, // connect flags: clean start only
            0x00, 0x0a, // keep alive = 10
            0x00, // properties length = 0
            0x00, 0x03, // client_id length = 3
            0xED, 0xA0, 0x80, // client_id = U+D800 (surrogate)
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::TopicNotUtf8 { .. })));
    }

    /// [MQTT-3.1.3-12] / [MQTT-1.5.4-2]: The User Name MUST be a UTF-8 Encoded
    /// String. Verify that decoding a CONNECT with U+0000 in the username
    /// is rejected.
    #[test]
    fn connect_parsing_rejects_null_character_in_username() {
        // CONNECT with username containing U+0000
        // remaining = var_header(10) + props_len(1) + client_id(2+1) + username(2+3) = 19
        let packetstream: Vec<u8> = [
            0x10,
            19, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x05,        // protocol name + level
            0b1000_0010, // connect flags: username + clean start
            0x00,
            0x0a, // keep alive = 10
            0x00, // properties length = 0
            0x00,
            0x01, // client_id length = 1
            b'a', // client_id = "a"
            0x00,
            0x03, // username length = 3
            b'a',
            0x00,
            b'b', // username = "a\0b"
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::MalformedPacket)));
    }

    /// [MQTT-3.1.3-12] / [MQTT-1.5.4-2]: The User Name MUST be a UTF-8 Encoded
    /// String. Verify that encoding a CONNECT with U+0000 in the username
    /// is rejected.
    #[test]
    fn connect_encoding_rejects_null_character_in_username() {
        let connect_pkt = Connect {
            keep_alive: 10,
            client_id: "test".into(),
            clean_start: true,
            properties: None,
        };
        let auth = ConnectAuth::Username {
            username: "a\0b".to_owned(),
        };

        let mut buf = BytesMut::new();
        let result = connect_pkt.write(&None, &auth, &mut buf);

        assert!(matches!(result, Err(Error::MalformedPacket)));
    }

    /// [MQTT-3.1.3-12] / [MQTT-1.5.4-1]: The User Name MUST be a UTF-8 Encoded
    /// String. Verify that decoding a CONNECT with a surrogate code point
    /// (U+D801) in the username is rejected.
    #[test]
    fn connect_parsing_rejects_surrogate_in_username() {
        // CONNECT with username containing U+D801 (CESU-8: 0xED 0xA0 0x81)
        // remaining = var_header(10) + props_len(1) + client_id(2+1) + username(2+3) = 19
        let packetstream: Vec<u8> = [
            0x10,
            19, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x05,        // protocol name + level
            0b1000_0010, // connect flags: username + clean start
            0x00,
            0x0a, // keep alive = 10
            0x00, // properties length = 0
            0x00,
            0x01, // client_id length = 1
            b'a', // client_id = "a"
            0x00,
            0x03, // username length = 3
            0xED,
            0xA0,
            0x81, // username = U+D801 (surrogate)
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::TopicNotUtf8 { .. })));
    }

    /// [MQTT-3.1.3-12]: If the User Name Flag is set to 1, a User Name MUST be
    /// present in the payload. Verify that decoding a CONNECT where the
    /// username flag is set but the payload is truncated (length prefix claims
    /// more bytes than remain) fails.
    #[test]
    fn connect_parsing_rejects_truncated_username_when_username_flag_set() {
        // CONNECT with username flag set but payload ends after length prefix
        // remaining = var_header(10) + props_len(1) + client_id(2+1) + username_len(2) = 16
        // but the username length prefix says 5 bytes and only 1 is present
        let packetstream: Vec<u8> = [
            0x10,
            16, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x05,        // protocol name + level
            0b1000_0010, // connect flags: username + clean start
            0x00,
            0x0a, // keep alive = 10
            0x00, // properties length = 0
            0x00,
            0x01, // client_id length = 1
            b'a', // client_id = "a"
            0x00,
            0x05, // username length = 5 (but only 1 byte follows)
            b'x', // truncated username
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(packet.is_err());
    }

    #[test]
    fn connect_roundtrips_binary_password() {
        let connect_pkt = Connect {
            keep_alive: 5,
            client_id: "client".into(),
            clean_start: true,
            properties: None,
        };
        let login = ConnectAuth::UsernamePassword {
            username: "binary".to_owned(),
            password: Bytes::from_static(b"\x00\xffproto\0buf"),
        };

        let mut buf = BytesMut::new();
        connect_pkt.write(&None, &login, &mut buf).unwrap();

        let fixed_header = parse_fixed_header(buf.iter()).unwrap();
        let connect_bytes = buf.split_to(fixed_header.frame_length()).freeze();
        let (_, _, decoded_login) = Connect::read(fixed_header, connect_bytes).unwrap();

        assert_eq!(
            decoded_login,
            ConnectAuth::UsernamePassword {
                username: "binary".to_owned(),
                password: Bytes::from_static(b"\x00\xffproto\0buf"),
            }
        );
    }

    #[test]
    fn connect_roundtrips_multibyte_utf8_username() {
        let connect_pkt = Connect {
            keep_alive: 5,
            client_id: "client".into(),
            clean_start: true,
            properties: None,
        };
        let username = "user-日本語-🔑".to_owned();
        let login = ConnectAuth::UsernamePassword {
            username: username.clone(),
            password: Bytes::from_static(b"pw"),
        };

        let mut buf = BytesMut::new();
        connect_pkt.write(&None, &login, &mut buf).unwrap();

        let fixed_header = parse_fixed_header(buf.iter()).unwrap();
        let connect_bytes = buf.split_to(fixed_header.frame_length()).freeze();
        let (_, _, decoded_login) = Connect::read(fixed_header, connect_bytes).unwrap();

        assert_eq!(
            decoded_login,
            ConnectAuth::UsernamePassword {
                username,
                password: Bytes::from_static(b"pw"),
            }
        );
    }

    #[test]
    fn connect_encoding_with_password_and_empty_username_writes_zero_len_username() {
        let connect_pkt = Connect {
            keep_alive: 5,
            client_id: "client".into(),
            clean_start: true,
            properties: None,
        };
        let login = ConnectAuth::UsernamePassword {
            username: String::new(),
            password: Bytes::from_static(b"pw"),
        };

        let mut buf = BytesMut::new();
        connect_pkt.write(&None, &login, &mut buf).unwrap();

        assert_eq!(buf[9], 0b1100_0010);

        let fixed_header = parse_fixed_header(buf.iter()).unwrap();
        let connect_bytes = buf.split_to(fixed_header.frame_length()).freeze();
        let (_, _, decoded_login) = Connect::read(fixed_header, connect_bytes).unwrap();
        assert_eq!(
            decoded_login,
            ConnectAuth::UsernamePassword {
                username: String::new(),
                password: Bytes::from_static(b"pw"),
            }
        );
    }

    #[test]
    fn connect_encoding_with_username_only_omits_password_and_flag() {
        let connect_pkt = Connect {
            keep_alive: 5,
            client_id: "client".into(),
            clean_start: true,
            properties: None,
        };
        let auth = ConnectAuth::Username {
            username: "user".to_owned(),
        };

        let mut buf = BytesMut::new();
        connect_pkt.write(&None, &auth, &mut buf).unwrap();

        assert_eq!(buf[9] & 0x40, 0);
        assert_eq!(
            &buf[..],
            b"\x10\x19\x00\x04MQTT\x05\x82\x00\x05\x00\x00\x06client\x00\x04user"
        );
    }

    #[test]
    fn connect_parsing_accepts_password_without_username_flag() {
        let mut stream = bytes::BytesMut::new();
        let packetstream = &[
            0x10, 0x15, 0x00, 0x04, b'M', b'Q', b'T', b'T', 0x05, 0x42, 0x00, 0x0a, 0x00, 0x00,
            0x04, b't', b'e', b's', b't', 0x00, 0x02, 0xff, 0x00,
        ];
        stream.extend_from_slice(packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let (_, will, auth) = Connect::read(fixed_header, connect_bytes).unwrap();

        assert_eq!(will, None);
        assert_eq!(
            auth,
            ConnectAuth::Password {
                password: Bytes::from_static(b"\xff\0"),
            }
        );
    }

    #[test]
    fn connect_parsing_rejects_reserved_connect_flag_bit() {
        let mut stream = bytes::BytesMut::new();
        let packetstream = &[
            0x10, 0x10, 0x00, 0x04, b'M', b'Q', b'T', b'T', 0x05, 0x03, 0x00, 0x0a, 0x00, 0x00,
            0x00, 0x04, b't', b'e', b's', b't',
        ];
        stream.extend_from_slice(packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn connect_roundtrips_explicitly_empty_password() {
        let connect_pkt = Connect {
            keep_alive: 5,
            client_id: "client".into(),
            clean_start: true,
            properties: None,
        };
        let auth = ConnectAuth::UsernamePassword {
            username: "user".to_owned(),
            password: Bytes::new(),
        };

        let mut buf = BytesMut::new();
        connect_pkt.write(&None, &auth, &mut buf).unwrap();

        assert_eq!(buf[9], 0b1100_0010);

        let fixed_header = parse_fixed_header(buf.iter()).unwrap();
        let connect_bytes = buf.split_to(fixed_header.frame_length()).freeze();
        let (_, _, decoded_auth) = Connect::read(fixed_header, connect_bytes).unwrap();
        assert_eq!(
            decoded_auth,
            ConnectAuth::UsernamePassword {
                username: "user".to_owned(),
                password: Bytes::new(),
            }
        );
    }

    #[test]
    fn connect_roundtrips_password_only_auth() {
        let connect_pkt = Connect {
            keep_alive: 5,
            client_id: "client".into(),
            clean_start: true,
            properties: None,
        };
        let auth = ConnectAuth::Password {
            password: Bytes::from_static(b"\x00\xffproto\0buf"),
        };

        let mut buf = BytesMut::new();
        connect_pkt.write(&None, &auth, &mut buf).unwrap();

        assert_eq!(buf[9], 0b0100_0010);

        let fixed_header = parse_fixed_header(buf.iter()).unwrap();
        let connect_bytes = buf.split_to(fixed_header.frame_length()).freeze();
        let (_, _, decoded_auth) = Connect::read(fixed_header, connect_bytes).unwrap();
        assert_eq!(
            decoded_auth,
            ConnectAuth::Password {
                password: Bytes::from_static(b"\x00\xffproto\0buf"),
            }
        );
    }

    /// MQTT-3.1.2-9 / MQTT-3.1.2-11 / MQTT-3.1.2-13: When no last will is set,
    /// the Will Flag, Will QoS, and Will Retain bits in the CONNECT flags
    /// MUST all be zero.
    #[test]
    fn connect_encoding_without_last_will_emits_zero_will_bits() {
        let connect_pkt = Connect {
            keep_alive: 5,
            client_id: "client".into(),
            clean_start: true,
            properties: None,
        };

        let mut buf = BytesMut::new();
        connect_pkt
            .write(&None, &ConnectAuth::None, &mut buf)
            .unwrap();

        // CONNECT flags byte sits at index 9 for this packet.
        // Will Flag is bit 2 (0x04), Will QoS is bits 3-4 (0x18),
        // Will Retain is bit 5 (0x20). All must be zero when will is None.
        assert_eq!(buf[9] & 0x3C, 0);
    }

    /// MQTT-3.1.2-9: If the Will Flag is set to 1, the Will Properties, Will
    /// Topic, and Will Payload fields MUST be present in the Payload. Verify
    /// that decoding a CONNECT with Will Flag=1 but no Will data is rejected.
    #[test]
    fn connect_parsing_rejects_will_flag_set_without_will_topic() {
        // CONNECT with Will Flag=1 but payload truncated right after client_id
        // (no Will Properties, Will Topic, or Will Payload bytes follow).
        let packetstream: Vec<u8> = [
            0x10,
            15, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x05,        // protocol name + level
            0b0000_0110, // connect flags: clean start + will flag, will qos 0
            0x00,
            0x0a, // keep alive = 10
            0x00, // properties length = 0
            0x00,
            0x04,
            b't',
            b'e',
            b's',
            b't', // client_id = "test"
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(packet.is_err());
    }

    /// MQTT-3.1.2-9: Will Properties MUST be present when Will Flag=1. When
    /// there are no Will properties, the spec requires a 0-length property
    /// section (a single 0x00 byte). Verify this encoding is correct and
    /// round-trips through read.
    #[test]
    fn last_will_encoding_writes_zero_length_properties_when_none() {
        let will = LastWill {
            topic: Bytes::from_static(b"goodbye/topic"),
            message: Bytes::from_static(b"offline"),
            qos: QoS::AtMostOnce,
            retain: false,
            properties: None,
        };

        let mut buf = BytesMut::new();
        let flags = will.write(&mut buf).unwrap();

        // Will Flag must be set
        assert_eq!(flags & 0x04, 0x04);
        // First byte of Will data is the property length; must be 0x00
        assert_eq!(buf[0], 0x00);

        // Verify round-trip: reconstruct from the written bytes
        let mut read_bytes = buf.copy_to_bytes(buf.len());
        let decoded = LastWill::read(flags, &mut read_bytes).unwrap().unwrap();
        assert_eq!(decoded.topic, will.topic);
        assert_eq!(decoded.message, will.message);
        assert_eq!(decoded.qos, will.qos);
        assert_eq!(decoded.retain, will.retain);
        assert_eq!(decoded.properties, None);
    }

    /// MQTT-3.1.2-11: If the Will Flag is set to 0, the Will QoS fields in
    /// the Connect Flags MUST be set to zero.
    #[test]
    fn connect_parsing_rejects_will_flag_zero_with_will_qos_1() {
        // CONNECT with Will Flag=0 but Will QoS=1 (bits 3-4 = 0b01)
        // remaining = var_header(10) + props_len(1) + client_id(2+4) = 17
        let packetstream: Vec<u8> = [
            0x10,
            17, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x05,        // protocol name + level
            0b0001_0010, // connect flags: clean start + will qos 1, will flag=0
            0x00,
            0x0a, // keep alive = 10
            0x00, // properties length = 0
            0x00,
            0x04,
            b't',
            b'e',
            b's',
            b't', // client_id = "test"
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::IncorrectPacketFormat)));
    }

    /// MQTT-3.1.2-13: If the Will Flag is set to 0, the Will Retain field in
    /// the Connect Flags MUST be set to zero.
    #[test]
    fn connect_parsing_rejects_will_flag_zero_with_will_retain() {
        // CONNECT with Will Flag=0 but Will Retain=1 (bit 5)
        // remaining = var_header(10) + props_len(1) + client_id(2+4) = 17
        let packetstream: Vec<u8> = [
            0x10,
            17, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x05,        // protocol name + level
            0b0010_0010, // connect flags: clean start + will retain, will flag=0
            0x00,
            0x0a, // keep alive = 10
            0x00, // properties length = 0
            0x00,
            0x04,
            b't',
            b'e',
            b's',
            b't', // client_id = "test"
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::IncorrectPacketFormat)));
    }

    /// MQTT-3.1.2-12: Will QoS values 0, 1, and 2 MUST encode into the correct
    /// bit positions in the CONNECT flags (bit 3 = LSB, bit 4 = MSB).
    #[test]
    fn connect_encoding_emits_correct_will_qos_bits() {
        let connect = Connect {
            keep_alive: 5,
            client_id: "c".into(),
            clean_start: true,
            properties: None,
        };

        // Will Flag is bit 2 (0x04). Will QoS is bits 3-4.
        // connect_flags = 0x04 | (qos as u8) << 3
        // QoS 0: 0x04 | 0x00 = 0x04  (bits: ...0 0 1 0 0)
        // QoS 1: 0x04 | 0x08 = 0x0C  (bits: ...0 1 1 0 0)
        // QoS 2: 0x04 | 0x10 = 0x14  (bits: ...1 0 1 0 0)
        for (qos, expected_will_bits) in [
            (QoS::AtMostOnce, 0x04u8),
            (QoS::AtLeastOnce, 0x0Cu8),
            (QoS::ExactlyOnce, 0x14u8),
        ] {
            let will = Some(LastWill {
                topic: Bytes::from_static(b"t"),
                message: Bytes::from_static(b"m"),
                qos,
                retain: false,
                properties: None,
            });

            let mut buf = BytesMut::new();
            connect.write(&will, &ConnectAuth::None, &mut buf).unwrap();

            let flags = buf[9];
            // Mask bits 2-4 (Will Flag + Will QoS)
            assert_eq!(
                flags & 0x1C,
                expected_will_bits & 0x1C,
                "QoS {qos:?}: expected Will bits {:#010b}, got {:#010b}",
                expected_will_bits & 0x1C,
                flags & 0x1C
            );
        }
    }

    /// MQTT-3.1.2-12: Will QoS 0, 1, and 2 MUST round-trip through
    /// encode and decode without alteration.
    #[test]
    fn connect_roundtrips_all_valid_will_qos_levels() {
        let connect = Connect {
            keep_alive: 5,
            client_id: "c".into(),
            clean_start: true,
            properties: None,
        };

        for qos in [QoS::AtMostOnce, QoS::AtLeastOnce, QoS::ExactlyOnce] {
            let will = Some(LastWill {
                topic: Bytes::from_static(b"t"),
                message: Bytes::from_static(b"m"),
                qos,
                retain: false,
                properties: None,
            });

            let mut buf = BytesMut::new();
            connect.write(&will, &ConnectAuth::None, &mut buf).unwrap();

            let fixed_header = parse_fixed_header(buf.iter()).unwrap();
            let connect_bytes = buf.split_to(fixed_header.frame_length()).freeze();
            let (_, decoded_will, _) = Connect::read(fixed_header, connect_bytes).unwrap();

            assert_eq!(decoded_will.as_ref().unwrap().qos, qos);
        }
    }

    /// MQTT-3.1.2-12: A Will QoS value of 3 (0x03) is a Malformed Packet.
    #[test]
    fn connect_parsing_rejects_will_qos_3() {
        // CONNECT with Will Flag=1 and Will QoS=3 (bits 3-4 = 0b11)
        // remaining = var_header(10) + props_len(1) + client_id(6)
        //           + will_props_len(1) + will_topic(3) + will_message(3) = 24
        let packetstream: Vec<u8> = [
            0x10,
            24, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x05,        // protocol name + level
            0b0001_1110, // connect flags: clean start + will flag + will qos 3
            0x00,
            0x0a, // keep alive = 10
            0x00, // properties length = 0
            0x00,
            0x04,
            b't',
            b'e',
            b's',
            b't', // client_id = "test"
            0x00, // will properties length = 0
            0x00,
            0x01,
            b'a', // will topic = "a"
            0x00,
            0x01,
            b'x', // will message = "x"
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::InvalidQoS(3))));
    }

    /// MQTT-3.1.3-1: CONNECT payload fields are selected by the flags and must
    /// appear in the specified order. With Will Flag=0, Will Properties, Will
    /// Topic, and Will Payload MUST NOT be present.
    #[test]
    fn connect_parsing_rejects_will_fields_when_will_flag_zero() {
        // CONNECT with Will Flag=0 and all Will bits zero, but with Will data
        // bytes after the Client Identifier.
        // remaining = var_header(10) + props_len(1) + client_id(2+4)
        //           + will_props_len(1) + will_topic(2+1) + will_payload(2+1) = 24
        let packetstream: Vec<u8> = [
            0x10,
            24, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x05,        // protocol name + level
            0b0000_0010, // connect flags: clean start only, will flag=0
            0x00,
            0x0a, // keep alive = 10
            0x00, // properties length = 0
            0x00,
            0x04,
            b't',
            b'e',
            b's',
            b't', // client_id = "test"
            0x00, // forbidden will properties length = 0
            0x00,
            0x01,
            b'a', // forbidden will topic = "a"
            0x00,
            0x01,
            b'x', // forbidden will payload = "x"
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::IncorrectPacketFormat)));
    }

    /// MQTT-3.1.2-16: If the User Name Flag is set to 0, a User Name MUST NOT
    /// be present in the Payload.
    #[test]
    fn connect_parsing_rejects_username_when_username_flag_zero() {
        // CONNECT with username_flag=0 but spurious username bytes in the payload.
        // The has_remaining() check in Connect::read() should reject this.
        // remaining = var_header(10) + props_len(1) + client_id(2+4) + spurious_username(2+4) = 23
        let packetstream: Vec<u8> = [
            0x10,
            23, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x05,        // protocol name + level
            0b0000_0010, // connect flags: clean start only, no username flag
            0x00,
            0x0a, // keep alive = 10
            0x00, // properties length = 0
            0x00,
            0x04,
            b't',
            b'e',
            b's',
            b't', // client_id = "test"
            0x00,
            0x04,
            b'u',
            b's',
            b'e',
            b'r', // spurious username (flag says it shouldn't be here)
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::IncorrectPacketFormat)));
    }

    /// MQTT-3.1.2-19: If the Password Flag is set to 1, a Password MUST be
    /// present in the Payload. Verify that decoding a CONNECT with
    /// password_flag=1 but no password data is rejected.
    #[test]
    fn connect_parsing_rejects_password_flag_set_without_password() {
        // CONNECT with username_flag=1, password_flag=1, but payload truncated
        // right after the username (no password bytes follow).
        // remaining = var_header(10) + props_len(1) + client_id(2+4) + username(2+4) = 23
        let packetstream: Vec<u8> = [
            0x10,
            23, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x05,        // protocol name + level
            0b1100_0010, // connect flags: username + password + clean start
            0x00,
            0x0a, // keep alive = 10
            0x00, // properties length = 0
            0x00,
            0x04,
            b't',
            b'e',
            b's',
            b't', // client_id = "test"
            0x00,
            0x04,
            b'u',
            b's',
            b'e',
            b'r', // username = "user"
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(packet.is_err());
    }

    /// MQTT-3.1.2-18: If the Password Flag is set to 0, a Password MUST NOT
    /// be present in the Payload.
    #[test]
    fn connect_parsing_rejects_password_when_password_flag_zero() {
        // CONNECT with password_flag=0 but spurious password bytes in the payload.
        // The has_remaining() check in Connect::read() should reject this.
        // remaining = var_header(10) + props_len(1) + client_id(2+4) + spurious_password(2+2) = 21
        let packetstream: Vec<u8> = [
            0x10,
            21, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x05,        // protocol name + level
            0b0000_0010, // connect flags: clean start only, no password flag
            0x00,
            0x0a, // keep alive = 10
            0x00, // properties length = 0
            0x00,
            0x04,
            b't',
            b'e',
            b's',
            b't', // client_id = "test"
            0x00,
            0x02,
            b'p',
            b'w', // spurious password (flag says it shouldn't be here)
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::IncorrectPacketFormat)));
    }

    /// MQTT-3.1.3-1: CONNECT payload fields MUST appear in the order
    /// Client Identifier, Will Properties, Will Topic, Will Payload,
    /// User Name, Password. Verify byte-level positions of each field
    /// in the encoded output.
    #[test]
    fn connect_encoding_emits_payload_fields_in_spec_order() {
        let connect = Connect {
            keep_alive: 10,
            client_id: "cid".to_owned(),
            clean_start: true,
            properties: None,
        };
        let will = Some(LastWill {
            topic: Bytes::from_static(b"wt"),
            message: Bytes::from_static(b"wm"),
            qos: QoS::AtLeastOnce,
            retain: false,
            properties: None,
        });
        let auth = ConnectAuth::UsernamePassword {
            username: "un".to_owned(),
            password: Bytes::from_static(b"pw"),
        };

        let mut buf = BytesMut::new();
        connect.write(&will, &auth, &mut buf).unwrap();

        // Fixed header: 0x10, remaining_length (0x21 = 33)
        // Variable header: 0x00 0x04 M Q T T 0x05 connect_flags 0x00 0x0a
        // Connect Properties Length: 0x00
        // Payload starts after variable header (10 bytes) + connect props length (1 byte)
        // + fixed header (2 bytes) = offset 13.
        let payload_start = 13;

        // Client Identifier: 0x00 0x03 c i d
        let client_id_start = payload_start;
        assert_eq!(&buf[client_id_start..client_id_start + 5], b"\x00\x03cid");

        // Will Properties Length: 0x00
        let will_props_start = client_id_start + 5;
        assert_eq!(buf[will_props_start], 0x00);

        // Will Topic: 0x00 0x02 w t
        let will_topic_start = will_props_start + 1;
        assert_eq!(&buf[will_topic_start..will_topic_start + 4], b"\x00\x02wt");

        // Will Payload: 0x00 0x02 w m
        let will_payload_start = will_topic_start + 4;
        assert_eq!(
            &buf[will_payload_start..will_payload_start + 4],
            b"\x00\x02wm"
        );

        // Username: 0x00 0x02 u n
        let username_start = will_payload_start + 4;
        assert_eq!(&buf[username_start..username_start + 4], b"\x00\x02un");

        // Password: 0x00 0x02 p w
        let password_start = username_start + 4;
        assert_eq!(&buf[password_start..password_start + 4], b"\x00\x02pw");

        // Nothing after password.
        assert_eq!(buf.len(), password_start + 4);
    }

    /// MQTT-3.1.3-3: zero-byte ClientID is still serialized as the first payload field.
    #[test]
    fn connect_encoding_emits_zero_byte_client_id_first_in_payload() {
        let connect = Connect {
            keep_alive: 10,
            client_id: String::new(),
            clean_start: true,
            properties: None,
        };
        let will = Some(LastWill {
            topic: Bytes::from_static(b"wt"),
            message: Bytes::from_static(b"wm"),
            qos: QoS::AtLeastOnce,
            retain: false,
            properties: None,
        });
        let auth = ConnectAuth::UsernamePassword {
            username: "un".to_owned(),
            password: Bytes::from_static(b"pw"),
        };

        let mut buf = BytesMut::new();
        connect.write(&will, &auth, &mut buf).unwrap();

        let payload_start = 13;

        // Zero-byte Client Identifier: 0x00 0x00
        assert_eq!(&buf[payload_start..payload_start + 2], b"\x00\x00");

        // Will Properties Length: 0x00
        let will_props_start = payload_start + 2;
        assert_eq!(buf[will_props_start], 0x00);

        // Will Topic follows: 0x00 0x02 w t
        let will_topic_start = will_props_start + 1;
        assert_eq!(&buf[will_topic_start..will_topic_start + 4], b"\x00\x02wt");
    }
}
