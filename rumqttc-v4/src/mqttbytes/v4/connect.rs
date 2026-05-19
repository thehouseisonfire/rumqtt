use super::{
    BufMut, BytesMut, Error, FixedHeader, QoS, len_len, qos, read_mqtt_bytes, read_mqtt_string,
    read_u8, read_u16, write_mqtt_bytes, write_mqtt_string, write_remaining_length,
};
use bytes::{Buf, Bytes};

/// Connection packet initiated by the client
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connect {
    /// Mqtt keep alive time
    pub keep_alive: u16,
    /// Client Id
    pub client_id: String,
    /// Clean session. Asks the broker to clear previous state
    pub clean_session: bool,
    /// Will that broker needs to publish when the client disconnects
    pub last_will: Option<LastWill>,
    /// CONNECT authentication fields
    pub auth: ConnectAuth,
}

impl Connect {
    pub fn new<S: Into<String>>(id: S) -> Self {
        Self {
            keep_alive: 10,
            client_id: id.into(),
            clean_session: true,
            last_will: None,
            auth: ConnectAuth::None,
        }
    }

    pub fn set_auth(&mut self, auth: ConnectAuth) -> &mut Self {
        self.auth = auth;
        self
    }

    pub fn clear_auth(&mut self) -> &mut Self {
        self.auth = ConnectAuth::None;
        self
    }

    pub fn set_username<U: Into<String>>(&mut self, username: U) -> &mut Self {
        self.auth = ConnectAuth::Username {
            username: username.into(),
        };
        self
    }

    pub fn set_credentials<U: Into<String>, P: Into<Bytes>>(
        &mut self,
        username: U,
        password: P,
    ) -> &mut Self {
        self.auth = ConnectAuth::UsernamePassword {
            username: username.into(),
            password: password.into(),
        };
        self
    }

    const fn len(&self) -> usize {
        let mut len = 2 + "MQTT".len() // protocol name
                              + 1            // protocol version
                              + 1            // connect flags
                              + 2; // keep alive

        len += 2 + self.client_id.len();

        // last will len
        if let Some(last_will) = &self.last_will {
            len += last_will.len();
        }

        // username and password len
        len += self.auth.len();

        len
    }

    pub fn read(fixed_header: FixedHeader, mut bytes: Bytes) -> Result<Self, Error> {
        let variable_header_index = fixed_header.header_len;
        bytes.advance(variable_header_index);

        // Variable header
        let protocol_name = read_mqtt_string(&mut bytes)?;
        let protocol_level = read_u8(&mut bytes)?;
        if protocol_name != "MQTT" {
            return Err(Error::InvalidProtocol);
        }

        if protocol_level != 4 {
            return Err(Error::InvalidProtocolLevel(protocol_level));
        }

        let connect_flags = read_u8(&mut bytes)?;
        validate_connect_flags(connect_flags)?;
        let clean_session = (connect_flags & 0b10) != 0;
        let keep_alive = read_u16(&mut bytes)?;

        let client_id = read_mqtt_string(&mut bytes)?;
        let last_will = LastWill::read(connect_flags, &mut bytes)?;
        let auth = ConnectAuth::read(connect_flags, &mut bytes)?;

        if bytes.has_remaining() {
            return Err(Error::IncorrectPacketFormat);
        }

        let connect = Self {
            keep_alive,
            client_id,
            clean_session,
            last_will,
            auth,
        };

        Ok(connect)
    }

    pub fn write(&self, buffer: &mut BytesMut) -> Result<usize, Error> {
        // MQTT-3.1.3-7: zero-byte ClientId requires CleanSession=1
        if self.client_id.is_empty() && !self.clean_session {
            return Err(Error::IncorrectPacketFormat);
        }

        let len = self.len();
        buffer.put_u8(0b0001_0000);
        let count = write_remaining_length(buffer, len)?;
        write_mqtt_string(buffer, "MQTT")?;

        buffer.put_u8(0x04);

        let flags_index = 1 + count + 2 + 4 + 1;

        let mut connect_flags = 0;
        if self.clean_session {
            connect_flags |= 0x02;
        }

        buffer.put_u8(connect_flags);
        buffer.put_u16(self.keep_alive);
        write_mqtt_string(buffer, &self.client_id)?;

        if let Some(last_will) = &self.last_will {
            connect_flags |= last_will.write(buffer)?;
        }

        connect_flags |= self.auth.write(buffer)?;

        // update connect flags
        buffer[flags_index] = connect_flags;
        Ok(1 + count + len)
    }

    pub const fn size(&self) -> usize {
        let len = self.len();
        let remaining_len_size = len_len(len);

        1 + remaining_len_size + len
    }
}

/// `LastWill` that broker forwards on behalf of the client
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastWill {
    pub topic: String,
    pub message: Bytes,
    pub qos: QoS,
    pub retain: bool,
}

impl LastWill {
    pub fn new(
        topic: impl Into<String>,
        payload: impl Into<Vec<u8>>,
        qos: QoS,
        retain: bool,
    ) -> Self {
        Self {
            topic: topic.into(),
            message: Bytes::from(payload.into()),
            qos,
            retain,
        }
    }

    const fn len(&self) -> usize {
        let mut len = 0;
        len += 2 + self.topic.len() + 2 + self.message.len();
        len
    }

    fn read(connect_flags: u8, bytes: &mut Bytes) -> Result<Option<Self>, Error> {
        let last_will = match connect_flags & 0b100 {
            0 if (connect_flags & 0b0011_1000) != 0 => {
                return Err(Error::IncorrectPacketFormat);
            }
            0 => None,
            _ => {
                let will_topic = read_mqtt_string(bytes)?;
                let will_message = read_mqtt_bytes(bytes)?;
                let will_qos = qos((connect_flags & 0b11000) >> 3)?;
                Some(Self {
                    topic: will_topic,
                    message: will_message,
                    qos: will_qos,
                    retain: (connect_flags & 0b0010_0000) != 0,
                })
            }
        };

        Ok(last_will)
    }

    fn write(&self, buffer: &mut BytesMut) -> Result<u8, Error> {
        let mut connect_flags = 0;

        connect_flags |= 0x04 | ((self.qos as u8) << 3);
        if self.retain {
            connect_flags |= 0x20;
        }

        write_mqtt_string(buffer, &self.topic)?;
        write_mqtt_bytes(buffer, &self.message)?;
        Ok(connect_flags)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnectAuth {
    #[default]
    None,
    Username {
        username: String,
    },
    UsernamePassword {
        username: String,
        password: Bytes,
    },
}

impl ConnectAuth {
    fn read(connect_flags: u8, bytes: &mut Bytes) -> Result<Self, Error> {
        let username_flag = (connect_flags & 0b1000_0000) != 0;
        let password_flag = (connect_flags & 0b0100_0000) != 0;

        match (username_flag, password_flag) {
            (false, false) => Ok(Self::None),
            (true, false) => Ok(Self::Username {
                username: read_mqtt_string(bytes)?,
            }),
            (true, true) => Ok(Self::UsernamePassword {
                username: read_mqtt_string(bytes)?,
                password: read_mqtt_bytes(bytes)?,
            }),
            (false, true) => Err(Error::IncorrectPacketFormat),
        }
    }

    const fn len(&self) -> usize {
        match self {
            Self::None => 0,
            Self::Username { username } => 2 + username.len(),
            Self::UsernamePassword { username, password } => {
                2 + username.len() + 2 + password.len()
            }
        }
    }

    fn write(&self, buffer: &mut BytesMut) -> Result<u8, Error> {
        let flags = match self {
            Self::None => 0,
            Self::Username { username } => {
                write_mqtt_string(buffer, username)?;
                0x80
            }
            Self::UsernamePassword { username, password } => {
                write_mqtt_string(buffer, username)?;
                write_mqtt_bytes(buffer, password.as_ref())?;
                0xC0
            }
        };

        Ok(flags)
    }

    #[must_use]
    pub fn validate<P: AsRef<[u8]>>(&self, username: &str, password: P) -> bool {
        matches!(
            self,
            Self::UsernamePassword {
                username: actual_username,
                password: actual_password,
            } if actual_username == username && actual_password.as_ref() == password.as_ref()
        )
    }
}

const fn validate_connect_flags(connect_flags: u8) -> Result<(), Error> {
    // CONNECT reserved bit must be zero.
    if (connect_flags & 0x01) != 0 {
        return Err(Error::IncorrectPacketFormat);
    }

    let username_flag = (connect_flags & 0x80) != 0;
    let password_flag = (connect_flags & 0x40) != 0;
    // Password flag implies username flag.
    if password_flag && !username_flag {
        return Err(Error::IncorrectPacketFormat);
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mqttbytes::parse_fixed_header;
    use bytes::BytesMut;
    use pretty_assertions::assert_eq;

    #[test]
    fn connect_parsing_works() {
        let mut stream = bytes::BytesMut::new();
        let packetstream = &[
            0x10,
            39, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x04,        // variable header
            0b1100_1110, // variable header. +username, +password, -will retain, will qos=1, +last_will, +clean_session
            0x00,
            0x0a, // variable header. keep alive = 10 sec
            0x00,
            0x04,
            b't',
            b'e',
            b's',
            b't', // payload. client_id
            0x00,
            0x02,
            b'/',
            b'a', // payload. will topic = '/a'
            0x00,
            0x07,
            b'o',
            b'f',
            b'f',
            b'l',
            b'i',
            b'n',
            b'e', // payload. variable header. will msg = 'offline'
            0x00,
            0x04,
            b'r',
            b'u',
            b'm',
            b'q', // payload. username = 'rumq'
            0x00,
            0x02,
            b'm',
            b'q', // payload. password = 'mq'
            0xDE,
            0xAD,
            0xBE,
            0xEF, // extra packets in the stream
        ];

        stream.extend_from_slice(&packetstream[..]);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes).unwrap();

        assert_eq!(
            packet,
            Connect {
                keep_alive: 10,
                client_id: "test".to_owned(),
                clean_session: true,
                last_will: Some(LastWill::new("/a", "offline", QoS::AtLeastOnce, false)),
                auth: ConnectAuth::UsernamePassword {
                    username: "rumq".to_owned(),
                    password: Bytes::from_static(b"mq"),
                },
            }
        );
    }

    fn sample_bytes() -> Vec<u8> {
        vec![
            0x10,
            39,
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x04,
            0b1100_1110, // +username, +password, -will retain, will qos=1, +last_will, +clean_session
            0x00,
            0x0a, // 10 sec
            0x00,
            0x04,
            b't',
            b'e',
            b's',
            b't', // client_id
            0x00,
            0x02,
            b'/',
            b'a', // will topic = '/a'
            0x00,
            0x07,
            b'o',
            b'f',
            b'f',
            b'l',
            b'i',
            b'n',
            b'e', // will msg = 'offline'
            0x00,
            0x04,
            b'r',
            b'u',
            b's',
            b't', // username = 'rust'
            0x00,
            0x02,
            b'm',
            b'q', // password = 'mq'
        ]
    }

    #[test]
    fn connect_encoding_works() {
        let connect = Connect {
            keep_alive: 10,
            client_id: "test".to_owned(),
            clean_session: true,
            last_will: Some(LastWill::new("/a", "offline", QoS::AtLeastOnce, false)),
            auth: ConnectAuth::UsernamePassword {
                username: "rust".to_owned(),
                password: Bytes::from_static(b"mq"),
            },
        };

        let mut buf = BytesMut::new();
        connect.write(&mut buf).unwrap();

        // println!("{:?}", &buf[..]);
        // println!("{:?}", sample_bytes());

        assert_eq!(buf, sample_bytes());
    }

    #[test]
    fn connect_roundtrips_max_keep_alive() {
        let connect = Connect {
            keep_alive: u16::MAX,
            client_id: "test".to_owned(),
            clean_session: true,
            last_will: None,
            auth: ConnectAuth::None,
        };

        let mut buf = BytesMut::new();
        connect.write(&mut buf).unwrap();

        let fixed_header = parse_fixed_header(buf.iter()).unwrap();
        let packet = Connect::read(fixed_header, buf.freeze()).unwrap();

        assert_eq!(packet.keep_alive, u16::MAX);
    }

    #[test]
    fn connect_parsing_rejects_reserved_connect_flag() {
        let mut stream = bytes::BytesMut::new();
        let packetstream = &[
            0x10, 0x10, 0x00, 0x04, b'M', b'Q', b'T', b'T', 0x04, 0x03, 0x00, 0x0a, 0x00, 0x04,
            b't', b'e', b's', b't',
        ];
        stream.extend_from_slice(packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::IncorrectPacketFormat)));
    }

    #[test]
    fn connect_parsing_rejects_password_flag_without_username_flag() {
        let mut stream = bytes::BytesMut::new();
        let packetstream = &[
            0x10, 0x14, 0x00, 0x04, b'M', b'Q', b'T', b'T', 0x04, 0x42, 0x00, 0x0a, 0x00, 0x04,
            b't', b'e', b's', b't', 0x00, 0x02, b'p', b'w',
        ];
        stream.extend_from_slice(packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::IncorrectPacketFormat)));
    }

    /// MQTT-3.1.2-2: Protocol Level for v3.1.1 MUST be 4 (0x04).
    #[test]
    fn connect_parsing_rejects_invalid_protocol_level() {
        let packetstream: Vec<u8> = [
            0x10, 14, // packet type, flags and remaining len
            0x00, 0x04, b'M', b'Q', b'T', b'T',
            0x03, // protocol name "MQTT" + invalid level 3
            0x02, // connect flags: clean session only
            0x00, 0x0a, // keep alive = 10
            0x00, 0x04, b't', b'e', b's', b't', // client_id = "test"
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::InvalidProtocolLevel(3))));
    }

    /// MQTT-3.1.2-2: The v3.1.1 codec must reject MQTT 5 CONNECT packets.
    #[test]
    fn connect_parsing_rejects_protocol_level_5() {
        let packetstream: Vec<u8> = [
            0x10, 14, // packet type, flags and remaining len
            0x00, 0x04, b'M', b'Q', b'T', b'T', 0x05, // protocol name "MQTT" + level 5
            0x02, // connect flags: clean session only
            0x00, 0x0a, // keep alive = 10
            0x00, 0x04, b't', b'e', b's', b't', // client_id = "test"
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::InvalidProtocolLevel(5))));
    }

    /// MQTT-3.1.2-2: Protocol Level for v3.1.1 MUST be 4 (0x04).
    #[test]
    fn connect_encoding_emits_protocol_level_4() {
        let connect = Connect {
            keep_alive: 10,
            client_id: "test".to_owned(),
            clean_session: true,
            last_will: None,
            auth: ConnectAuth::None,
        };

        let mut buf = BytesMut::new();
        connect.write(&mut buf).unwrap();

        // After fixed header (1 byte type + 1 byte remaining length) and
        // protocol name (2 byte length + 4 byte "MQTT"), the protocol level
        // byte sits at index 8.
        assert_eq!(buf[8], 0x04);
    }

    /// MQTT-3.1.2-3: CONNECT flags bit 0 is reserved and must encode as zero.
    #[test]
    fn connect_encoding_emits_zero_reserved_connect_flag_bit() {
        let connect = Connect {
            keep_alive: 10,
            client_id: "test".to_owned(),
            clean_session: true,
            last_will: Some(LastWill::new("/a", "offline", QoS::AtLeastOnce, true)),
            auth: ConnectAuth::UsernamePassword {
                username: "rust".to_owned(),
                password: Bytes::from_static(b"mq"),
            },
        };

        let mut buf = BytesMut::new();
        connect.write(&mut buf).unwrap();

        assert_eq!(buf[9] & 0x01, 0);
    }

    #[test]
    fn connect_encoding_with_password_and_empty_username_writes_zero_len_username() {
        let connect = Connect {
            keep_alive: 10,
            client_id: "test".to_owned(),
            clean_session: true,
            last_will: None,
            auth: ConnectAuth::UsernamePassword {
                username: String::new(),
                password: Bytes::from_static(b"pw"),
            },
        };

        let mut buf = BytesMut::new();
        connect.write(&mut buf).unwrap();

        // CONNECT flags byte sits at index 9 for this packet.
        assert_eq!(buf[9], 0b1100_0010);

        let fixed_header = parse_fixed_header(buf.iter()).unwrap();
        let connect_bytes = buf.split_to(fixed_header.frame_length()).freeze();
        let decoded = Connect::read(fixed_header, connect_bytes).unwrap();
        assert_eq!(
            decoded.auth,
            ConnectAuth::UsernamePassword {
                username: String::new(),
                password: Bytes::from_static(b"pw"),
            }
        );
    }

    #[test]
    fn connect_roundtrips_binary_password() {
        let connect = Connect {
            keep_alive: 10,
            client_id: "test".to_owned(),
            clean_session: true,
            last_will: None,
            auth: ConnectAuth::UsernamePassword {
                username: "binary".to_owned(),
                password: Bytes::from_static(b"\x00\xffproto\0buf"),
            },
        };

        let mut buf = BytesMut::new();
        connect.write(&mut buf).unwrap();

        let fixed_header = parse_fixed_header(buf.iter()).unwrap();
        let connect_bytes = buf.split_to(fixed_header.frame_length()).freeze();
        let decoded = Connect::read(fixed_header, connect_bytes).unwrap();
        assert_eq!(
            decoded.auth,
            ConnectAuth::UsernamePassword {
                username: "binary".to_owned(),
                password: Bytes::from_static(b"\x00\xffproto\0buf"),
            }
        );
    }

    #[test]
    fn connect_roundtrips_username_only_auth() {
        let connect = Connect {
            keep_alive: 10,
            client_id: "test".to_owned(),
            clean_session: true,
            last_will: None,
            auth: ConnectAuth::Username {
                username: String::new(),
            },
        };

        let mut buf = BytesMut::new();
        connect.write(&mut buf).unwrap();

        let fixed_header = parse_fixed_header(buf.iter()).unwrap();
        let connect_bytes = buf.split_to(fixed_header.frame_length()).freeze();
        let decoded = Connect::read(fixed_header, connect_bytes).unwrap();
        assert_eq!(
            decoded.auth,
            ConnectAuth::Username {
                username: String::new(),
            }
        );
    }

    #[test]
    fn connect_roundtrips_explicitly_empty_password() {
        let connect = Connect {
            keep_alive: 10,
            client_id: "test".to_owned(),
            clean_session: true,
            last_will: None,
            auth: ConnectAuth::UsernamePassword {
                username: "user".to_owned(),
                password: Bytes::new(),
            },
        };

        let mut buf = BytesMut::new();
        connect.write(&mut buf).unwrap();

        assert_eq!(buf[9], 0b1100_0010);

        let fixed_header = parse_fixed_header(buf.iter()).unwrap();
        let connect_bytes = buf.split_to(fixed_header.frame_length()).freeze();
        let decoded = Connect::read(fixed_header, connect_bytes).unwrap();
        assert_eq!(
            decoded.auth,
            ConnectAuth::UsernamePassword {
                username: "user".to_owned(),
                password: Bytes::new(),
            }
        );
    }

    #[test]
    fn connect_parsing_rejects_null_character_in_client_id() {
        let packetstream: Vec<u8> = [
            0x10, 15, // packet type, flags and remaining len
            0x00, 0x04, b'M', b'Q', b'T', b'T', 0x04, // protocol name + level
            0x02, // connect flags: clean session only
            0x00, 0x0a, // keep alive = 10
            0x00, 0x03, // client_id length = 3
            b'a', 0x00, b'b',
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::MalformedPacket)));
    }

    #[test]
    fn connect_encoding_rejects_null_character_in_client_id() {
        let connect = Connect {
            keep_alive: 10,
            client_id: "a\0b".to_owned(),
            clean_session: true,
            last_will: None,
            auth: ConnectAuth::None,
        };

        let mut buf = BytesMut::new();
        let result = connect.write(&mut buf);

        assert!(matches!(result, Err(Error::MalformedPacket)));
    }

    #[test]
    fn connect_encoding_rejects_client_id_larger_than_u16() {
        let connect = Connect {
            keep_alive: 10,
            client_id: "a".repeat(usize::from(u16::MAX) + 1),
            clean_session: true,
            last_will: None,
            auth: ConnectAuth::None,
        };

        let mut buf = BytesMut::new();
        let result = connect.write(&mut buf);

        assert!(matches!(result, Err(Error::PayloadTooLong)));
        assert!(
            !buf.is_empty(),
            "CONNECT encoding reports the protocol error after writing the packet prefix"
        );
    }

    #[test]
    fn connect_encoding_allows_broad_client_ids() {
        let client_id = "client-id_with.symbols/and/unicode-é".to_owned();
        assert!(client_id.len() > 23);
        let connect = Connect {
            keep_alive: 10,
            client_id: client_id.clone(),
            clean_session: true,
            last_will: None,
            auth: ConnectAuth::None,
        };

        let mut buf = BytesMut::new();
        connect.write(&mut buf).unwrap();
        let fixed_header = parse_fixed_header(buf.iter()).unwrap();
        let connect_bytes = buf.split_to(fixed_header.frame_length()).freeze();
        let decoded = Connect::read(fixed_header, connect_bytes).unwrap();

        assert_eq!(decoded.client_id, client_id);
    }

    #[test]
    fn connect_encoding_rejects_null_character_in_will_topic() {
        let connect = Connect {
            keep_alive: 10,
            client_id: "test".to_owned(),
            clean_session: true,
            last_will: Some(LastWill {
                topic: "a\0b".to_owned(),
                message: Bytes::from_static(b"x"),
                qos: QoS::AtMostOnce,
                retain: false,
            }),
            auth: ConnectAuth::None,
        };

        let mut buf = BytesMut::new();
        let result = connect.write(&mut buf);

        assert!(matches!(result, Err(Error::MalformedPacket)));
    }

    #[test]
    fn connect_encoding_rejects_null_character_in_username() {
        let connect = Connect {
            keep_alive: 10,
            client_id: "test".to_owned(),
            clean_session: true,
            last_will: None,
            auth: ConnectAuth::Username {
                username: "a\0b".to_owned(),
            },
        };

        let mut buf = BytesMut::new();
        let result = connect.write(&mut buf);

        assert!(matches!(result, Err(Error::MalformedPacket)));
    }

    /// MQTT-1.5.3-1: UTF-8 encoded strings MUST be well-formed
    /// and MUST NOT include encodings of code points between U+D800 and U+DFFF.
    #[test]
    fn connect_parsing_rejects_surrogate_in_client_id() {
        // CONNECT with client_id containing U+D800 (CESU-8: 0xED 0xA0 0x80)
        // remaining = var_header(10) + client_id(2+3) = 15
        let packetstream: Vec<u8> = [
            0x10, 15, // packet type, flags and remaining len
            0x00, 0x04, b'M', b'Q', b'T', b'T', 0x04, // protocol name + level
            0x02, // connect flags: clean session only
            0x00, 0x0a, // keep alive = 10
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

    #[test]
    fn connect_parsing_rejects_surrogate_in_will_topic() {
        // CONNECT with will_topic containing U+DFFF (CESU-8: 0xED 0xBF 0xBF)
        // remaining = var_header(10) + client_id(2+1) + will_topic(2+3) + will_msg(2+1) = 21
        let packetstream: Vec<u8> = [
            0x10,
            21, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x04,        // protocol name + level
            0b0000_1110, // connect flags: clean session + will flag + will qos 0
            0x00,
            0x0a, // keep alive = 10
            0x00,
            0x01,
            b'a', // client_id = "a"
            0x00,
            0x03, // will_topic length = 3
            0xED,
            0xBF,
            0xBF, // will_topic = U+DFFF (surrogate)
            0x00,
            0x01,
            b'x', // will_message = "x"
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);
        assert!(matches!(packet, Err(Error::TopicNotUtf8 { .. })));
    }

    #[test]
    fn connect_parsing_rejects_surrogate_in_username() {
        // CONNECT with username containing U+D801 (CESU-8: 0xED 0xA0 0x81)
        // remaining = var_header(10) + client_id(2+1) + username(2+3) = 18
        let packetstream: Vec<u8> = [
            0x10,
            18, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x04,        // protocol name + level
            0b1000_0010, // connect flags: username + clean session
            0x00,
            0x0a, // keep alive = 10
            0x00,
            0x01,
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

    #[test]
    fn connect_parsing_rejects_overlong_utf8_in_client_id() {
        // CONNECT with client_id containing overlong encoding (0xC0 0x80)
        // remaining = var_header(10) + client_id(2+2) = 14
        let packetstream: Vec<u8> = [
            0x10, 14, // packet type, flags and remaining len
            0x00, 0x04, b'M', b'Q', b'T', b'T', 0x04, // protocol name + level
            0x02, // connect flags: clean session only
            0x00, 0x0a, // keep alive = 10
            0x00, 0x02, // client_id length = 2
            0xC0, 0x80, // client_id = overlong U+0000
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);
        assert!(matches!(packet, Err(Error::TopicNotUtf8 { .. })));
    }

    /// MQTT-3.1.2-9 / MQTT-3.1.2-11: When no last will is set, the Will Flag,
    /// Will QoS, and Will Retain bits in the CONNECT flags MUST all be zero.
    #[test]
    fn connect_encoding_without_last_will_emits_zero_will_bits() {
        let connect = Connect {
            keep_alive: 10,
            client_id: "test".to_owned(),
            clean_session: true,
            last_will: None,
            auth: ConnectAuth::None,
        };

        let mut buf = BytesMut::new();
        connect.write(&mut buf).unwrap();

        // CONNECT flags byte sits at index 9 for this packet.
        // Will Flag is bit 2 (0x04), Will QoS is bits 3-4 (0x18),
        // Will Retain is bit 5 (0x20). All must be zero when last_will is None.
        assert_eq!(buf[9] & 0x3C, 0);
    }

    /// MQTT-3.1.2-9: If the Will Flag is set to 1, the Will Topic and Will
    /// Message fields MUST be present in the payload. Verify that decoding a
    /// CONNECT with Will Flag=1 but no Will Topic/Message data is rejected.
    #[test]
    fn connect_parsing_rejects_will_flag_set_without_will_topic() {
        // CONNECT with Will Flag=1 but payload truncated right after client_id
        // (no Will Topic or Will Message bytes follow).
        let packetstream: Vec<u8> = [
            0x10,
            14, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x04,        // protocol name + level
            0b0000_0110, // connect flags: clean session + will flag, will qos 0
            0x00,
            0x0a, // keep alive = 10
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

    /// MQTT-3.1.2-11 / MQTT-3.1.2-13: If the Will Flag is set to 0, the Will
    /// QoS fields in the Connect Flags MUST be set to zero.
    #[test]
    fn connect_parsing_rejects_will_flag_zero_with_will_qos_1() {
        // CONNECT with Will Flag=0 but Will QoS=1 (bits 3-4 = 0b01)
        // remaining = var_header(10) + client_id(2+4) = 16
        let packetstream: Vec<u8> = [
            0x10,
            16, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x04,        // protocol name + level
            0b0001_0010, // connect flags: clean session + will qos 1, will flag=0
            0x00,
            0x0a, // keep alive = 10
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

    /// MQTT-3.1.2-14: If the Will Flag is set to 1, Will QoS MUST NOT be 3.
    #[test]
    fn connect_parsing_rejects_will_qos_3() {
        // CONNECT with Will Flag=1 and Will QoS=3 (bits 3-4 = 0b11)
        // remaining = var_header(10) + client_id(2+1) + will_topic(2+1) + will_msg(2+1) = 19
        let packetstream: Vec<u8> = [
            0x10,
            19, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x04,        // protocol name + level
            0b0001_1110, // connect flags: clean session + will flag + will qos 3
            0x00,
            0x0a, // keep alive = 10
            0x00,
            0x01,
            b'a', // client_id = "a"
            0x00,
            0x01,
            b'b', // will_topic = "b"
            0x00,
            0x01,
            b'c', // will_message = "c"
        ]
        .into();

        let mut stream = BytesMut::new();
        stream.extend_from_slice(&packetstream);
        let fixed_header = parse_fixed_header(stream.iter()).unwrap();
        let connect_bytes = stream.split_to(fixed_header.frame_length()).freeze();
        let packet = Connect::read(fixed_header, connect_bytes);

        assert!(matches!(packet, Err(Error::InvalidQoS(3))));
    }

    /// MQTT-3.1.2-11: If the Will Flag is set to 0, the Will Retain field in
    /// the Connect Flags MUST be set to zero.
    #[test]
    fn connect_parsing_rejects_will_flag_zero_with_will_retain() {
        // CONNECT with Will Flag=0 but Will Retain=1 (bit 5)
        // remaining = var_header(10) + client_id(2+4) = 16
        let packetstream: Vec<u8> = [
            0x10,
            16, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x04,        // protocol name + level
            0b0010_0010, // connect flags: clean session + will retain, will flag=0
            0x00,
            0x0a, // keep alive = 10
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

    /// MQTT-3.1.2-18: If the User Name Flag is set to 0, a user name MUST NOT
    /// be present in the payload.
    #[test]
    fn connect_parsing_rejects_username_when_username_flag_zero() {
        // CONNECT with username_flag=0 but spurious username bytes in the payload.
        // The has_remaining() check in Connect::read() should reject this.
        // remaining = var_header(10) + client_id(2+4) + spurious_username(2+4) = 22
        let packetstream: Vec<u8> = [
            0x10,
            22, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x04,        // protocol name + level
            0b0000_0010, // connect flags: clean session only, no username flag
            0x00,
            0x0a, // keep alive = 10
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

    /// MQTT-3.1.2-20: If the Password Flag is set to 0, a password MUST NOT
    /// be present in the payload.
    #[test]
    fn connect_parsing_rejects_password_when_password_flag_zero() {
        // CONNECT with username_flag=1, password_flag=0, but spurious password
        // bytes in the payload after the username. The has_remaining() check
        // in Connect::read() should reject this.
        // remaining = var_header(10) + client_id(2+4) + username(2+4) + spurious_password(2+2) = 26
        let packetstream: Vec<u8> = [
            0x10,
            26, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x04,        // protocol name + level
            0b1000_0010, // connect flags: username + clean session, password_flag=0
            0x00,
            0x0a, // keep alive = 10
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

    /// MQTT-3.1.2-21: If the Password Flag is set to 1, a password MUST be
    /// present in the payload. Verify that decoding a CONNECT with
    /// password_flag=1 but no password data is rejected.
    #[test]
    fn connect_parsing_rejects_password_flag_set_without_password() {
        // CONNECT with username_flag=1, password_flag=1, but payload truncated
        // right after the username (no password bytes follow).
        // remaining = var_header(10) + client_id(2+4) + username(2+4) = 22
        let packetstream: Vec<u8> = [
            0x10,
            22, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x04,        // protocol name + level
            0b1100_0010, // connect flags: username + password + clean session
            0x00,
            0x0a, // keep alive = 10
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

    /// MQTT-3.1.2-11: If the Will Flag is 0, Will Topic and Will Message MUST
    /// NOT be present in the payload.
    #[test]
    fn connect_parsing_rejects_will_fields_when_will_flag_zero() {
        // CONNECT with Will Flag=0 and all Will bits zero, but with length-
        // prefixed Will Topic/Message bytes after the Client Identifier.
        // remaining = var_header(10) + client_id(2+4) + will_topic(2+1) + will_msg(2+1) = 22
        let packetstream: Vec<u8> = [
            0x10,
            22, // packet type, flags and remaining len
            0x00,
            0x04,
            b'M',
            b'Q',
            b'T',
            b'T',
            0x04,        // protocol name + level
            0b0000_0010, // connect flags: clean session only, will flag=0
            0x00,
            0x0a, // keep alive = 10
            0x00,
            0x04,
            b't',
            b'e',
            b's',
            b't', // client_id = "test"
            0x00,
            0x01,
            b'a', // forbidden will_topic = "a"
            0x00,
            0x01,
            b'x', // forbidden will_message = "x"
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
    /// Client Identifier, Will Topic, Will Message, User Name, Password.
    /// Verify byte-level positions of each field in the encoded output.
    #[test]
    fn connect_encoding_emits_payload_fields_in_spec_order() {
        let connect = Connect {
            keep_alive: 10,
            client_id: "cid".to_owned(),
            clean_session: true,
            last_will: Some(LastWill::new("wt", "wm", QoS::AtLeastOnce, false)),
            auth: ConnectAuth::UsernamePassword {
                username: "un".to_owned(),
                password: Bytes::from_static(b"pw"),
            },
        };

        let mut buf = BytesMut::new();
        connect.write(&mut buf).unwrap();

        // Fixed header: 0x10, remaining_length
        // Variable header: 0x00 0x04 M Q T T 0x04 connect_flags 0x00 0x0a
        // Payload starts after variable header (10 bytes) + fixed header (2 bytes) = offset 12.
        let payload_start = 12;

        // Client Identifier: 0x00 0x03 c i d
        let client_id_start = payload_start;
        assert_eq!(&buf[client_id_start..client_id_start + 5], b"\x00\x03cid");

        // Will Topic: 0x00 0x02 w t
        let will_topic_start = client_id_start + 5;
        assert_eq!(&buf[will_topic_start..will_topic_start + 4], b"\x00\x02wt");

        // Will Message: 0x00 0x02 w m
        let will_msg_start = will_topic_start + 4;
        assert_eq!(&buf[will_msg_start..will_msg_start + 4], b"\x00\x02wm");

        // Username: 0x00 0x02 u n
        let username_start = will_msg_start + 4;
        assert_eq!(&buf[username_start..username_start + 4], b"\x00\x02un");

        // Password: 0x00 0x02 p w
        let password_start = username_start + 4;
        assert_eq!(&buf[password_start..password_start + 4], b"\x00\x02pw");

        // Nothing after password.
        assert_eq!(buf.len(), password_start + 4);
    }

    /// MQTT-3.1.3-7: zero-byte ClientId with CleanSession=0 must be rejected.
    #[test]
    fn connect_encoding_rejects_empty_client_id_with_clean_session_false() {
        let connect = Connect {
            keep_alive: 10,
            client_id: String::new(),
            clean_session: false,
            last_will: None,
            auth: ConnectAuth::None,
        };

        let mut buf = BytesMut::new();
        let result = connect.write(&mut buf);
        assert!(matches!(result, Err(Error::IncorrectPacketFormat)));
    }
}
