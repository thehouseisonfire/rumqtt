use super::{
    BufMut, BytesMut, Error, FixedHeader, Protocol, QoS, len_len, qos, read_mqtt_bytes,
    read_mqtt_string, read_u8, read_u16, write_mqtt_bytes, write_mqtt_string,
    write_remaining_length,
};
use bytes::{Buf, Bytes};

/// Connection packet initiated by the client
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connect {
    /// Mqtt protocol version
    pub protocol: Protocol,
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
            protocol: Protocol::V4,
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

        let protocol = match protocol_level {
            4 => Protocol::V4,
            5 => Protocol::V5,
            num => return Err(Error::InvalidProtocolLevel(num)),
        };

        let connect_flags = read_u8(&mut bytes)?;
        validate_connect_flags(connect_flags)?;
        let clean_session = (connect_flags & 0b10) != 0;
        let keep_alive = read_u16(&mut bytes)?;

        let client_id = read_mqtt_string(&mut bytes)?;
        let last_will = LastWill::read(connect_flags, &mut bytes)?;
        let auth = ConnectAuth::read(connect_flags, &mut bytes)?;

        let connect = Self {
            protocol,
            keep_alive,
            client_id,
            clean_session,
            last_will,
            auth,
        };

        Ok(connect)
    }

    pub fn write(&self, buffer: &mut BytesMut) -> Result<usize, Error> {
        let len = self.len();
        buffer.put_u8(0b0001_0000);
        let count = write_remaining_length(buffer, len)?;
        write_mqtt_string(buffer, "MQTT");

        match self.protocol {
            Protocol::V4 => buffer.put_u8(0x04),
            Protocol::V5 => buffer.put_u8(0x05),
        }

        let flags_index = 1 + count + 2 + 4 + 1;

        let mut connect_flags = 0;
        if self.clean_session {
            connect_flags |= 0x02;
        }

        buffer.put_u8(connect_flags);
        buffer.put_u16(self.keep_alive);
        write_mqtt_string(buffer, &self.client_id);

        if let Some(last_will) = &self.last_will {
            connect_flags |= last_will.write(buffer);
        }

        connect_flags |= self.auth.write(buffer);

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

    fn write(&self, buffer: &mut BytesMut) -> u8 {
        let mut connect_flags = 0;

        connect_flags |= 0x04 | ((self.qos as u8) << 3);
        if self.retain {
            connect_flags |= 0x20;
        }

        write_mqtt_string(buffer, &self.topic);
        write_mqtt_bytes(buffer, &self.message);
        connect_flags
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

    fn write(&self, buffer: &mut BytesMut) -> u8 {
        match self {
            Self::None => 0,
            Self::Username { username } => {
                write_mqtt_string(buffer, username);
                0x80
            }
            Self::UsernamePassword { username, password } => {
                write_mqtt_string(buffer, username);
                write_mqtt_bytes(buffer, password.as_ref());
                0xC0
            }
        }
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
                protocol: Protocol::V4,
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
            protocol: Protocol::V4,
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

    #[test]
    fn connect_encoding_with_password_and_empty_username_writes_zero_len_username() {
        let connect = Connect {
            protocol: Protocol::V4,
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
            protocol: Protocol::V4,
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
            protocol: Protocol::V4,
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
            protocol: Protocol::V4,
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
}
