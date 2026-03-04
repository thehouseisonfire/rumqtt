use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::slice::Iter;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("Payload is too long")]
    PayloadTooLong,
    #[error("Promised boundary crossed, contains {0} bytes")]
    BoundaryCrossed(usize),
    #[error("Packet is malformed")]
    MalformedPacket,
    #[error("Remaining length is malformed")]
    MalformedRemainingLength,
    #[error("Topic not utf-8")]
    TopicNotUtf8,
    #[error("Insufficient number of bytes to frame packet, {0} more bytes required")]
    InsufficientBytes(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
pub struct ParsedFixedHeader {
    pub byte1: u8,
    pub remaining_len_len: usize,
    pub remaining_len: usize,
}

impl ParsedFixedHeader {
    #[must_use]
    pub fn frame_length(self) -> usize {
        1 + self.remaining_len_len + self.remaining_len
    }
}

pub fn check(stream: Iter<u8>) -> Result<ParsedFixedHeader, Error> {
    let stream_len = stream.len();
    let fixed_header = parse_fixed_header(stream)?;

    let frame_length = fixed_header.frame_length();
    if stream_len < frame_length {
        return Err(Error::InsufficientBytes(frame_length - stream_len));
    }

    Ok(fixed_header)
}

pub fn parse_fixed_header(mut stream: Iter<u8>) -> Result<ParsedFixedHeader, Error> {
    let stream_len = stream.len();
    if stream_len < 2 {
        return Err(Error::InsufficientBytes(2 - stream_len));
    }

    let byte1 = *stream.next().unwrap();
    let (remaining_len_len, remaining_len) = length(stream)?;

    Ok(ParsedFixedHeader {
        byte1,
        remaining_len_len,
        remaining_len,
    })
}

/// Parses variable byte integer in the stream and returns the length
/// and number of bytes that make it. Used for remaining length calculation
/// as well as for calculating property lengths
pub fn length(stream: Iter<u8>) -> Result<(usize, usize), Error> {
    let mut len: usize = 0;
    let mut len_len = 0;
    let mut done = false;
    let mut shift = 0;

    // Use continuation bit at position 7 to continue reading next
    // byte to frame 'length'.
    // Stream 0b1xxx_xxxx 0b1yyy_yyyy 0b1zzz_zzzz 0b0www_wwww will
    // be framed as number 0bwww_wwww_zzz_zzzz_yyy_yyyy_xxx_xxxx
    for byte in stream {
        len_len += 1;
        let byte = *byte as usize;
        len += (byte & 0x7F) << shift;

        // stop when continue bit is 0
        done = (byte & 0x80) == 0;
        if done {
            break;
        }

        shift += 7;

        // Only a max of 4 bytes allowed for remaining length
        // more than 4 shifts (0, 7, 14, 21) implies bad length
        if shift > 21 {
            return Err(Error::MalformedRemainingLength);
        }
    }

    // Not enough bytes to frame remaining length. wait for
    // one more byte
    if !done {
        return Err(Error::InsufficientBytes(1));
    }

    Ok((len_len, len))
}

/// Reads a series of bytes with a length from a byte stream
pub fn read_mqtt_bytes(stream: &mut Bytes) -> Result<Bytes, Error> {
    let len = read_u16(stream)? as usize;

    // Prevent attacks with wrong remaining length. This method is used in
    // `packet.assembly()` with (enough) bytes to frame packet. Ensures that
    // reading variable len string or bytes doesn't cross promised boundary
    // with `read_fixed_header()`
    if len > stream.len() {
        return Err(Error::BoundaryCrossed(len));
    }

    Ok(stream.split_to(len))
}

/// Reads a string from bytes stream
pub fn read_mqtt_string(stream: &mut Bytes) -> Result<String, Error> {
    let s = read_mqtt_bytes(stream)?;
    let s = std::str::from_utf8(&s).map_err(|_| Error::TopicNotUtf8)?;
    Ok(s.to_owned())
}

#[must_use]
pub fn is_mqtt_discouraged_code_point(ch: char) -> bool {
    let code = ch as u32;

    code == 0x0000
        || (0x0001..=0x001F).contains(&code)
        || (0x007F..=0x009F).contains(&code)
        || (0xFDD0..=0xFDEF).contains(&code)
        || (code & 0xFFFF) == 0xFFFE
        || (code & 0xFFFF) == 0xFFFF
}

#[must_use]
pub fn first_mqtt_discouraged_code_point(s: &str) -> Option<u32> {
    s.chars()
        .find(|ch| is_mqtt_discouraged_code_point(*ch))
        .map(|ch| ch as u32)
}

/// Serializes bytes to stream (including length)
pub fn write_mqtt_bytes(stream: &mut BytesMut, bytes: &[u8]) {
    let len = u16::try_from(bytes.len()).expect("MQTT string/bytes length must fit in u16");
    stream.put_u16(len);
    stream.extend_from_slice(bytes);
}

/// Serializes a string to stream
pub fn write_mqtt_string(stream: &mut BytesMut, string: &str) {
    write_mqtt_bytes(stream, string.as_bytes());
}

/// Writes remaining length to stream and returns number of bytes for remaining length
pub fn write_remaining_length(stream: &mut BytesMut, len: usize) -> Result<usize, Error> {
    if len > 268_435_455 {
        return Err(Error::PayloadTooLong);
    }

    let mut done = false;
    let mut x = len;
    let mut count = 0;

    while !done {
        let mut byte = u8::try_from(x % 128).expect("remainder in 0..=127 always fits in u8");
        x /= 128;
        if x > 0 {
            byte |= 128;
        }

        stream.put_u8(byte);
        count += 1;
        done = x == 0;
    }

    Ok(count)
}

/// Return number of remaining length bytes required for encoding length
#[must_use]
pub fn len_len(len: usize) -> usize {
    if len >= 2_097_152 {
        4
    } else if len >= 16_384 {
        3
    } else if len >= 128 {
        2
    } else {
        1
    }
}

/// After collecting enough bytes to frame a packet (packet's frame())
/// , It's possible that content itself in the stream is wrong. Like expected
/// packet id or qos not being present. In cases where `read_mqtt_string` or
/// `read_mqtt_bytes` exhausted remaining length but packet framing expects to
/// parse qos next, these pre checks will prevent `bytes` crashes
pub fn read_u16(stream: &mut Bytes) -> Result<u16, Error> {
    if stream.len() < 2 {
        return Err(Error::MalformedPacket);
    }

    Ok(stream.get_u16())
}

pub fn read_u8(stream: &mut Bytes) -> Result<u8, Error> {
    if stream.is_empty() {
        return Err(Error::MalformedPacket);
    }

    Ok(stream.get_u8())
}

pub fn read_u32(stream: &mut Bytes) -> Result<u32, Error> {
    if stream.len() < 4 {
        return Err(Error::MalformedPacket);
    }

    Ok(stream.get_u32())
}

#[cfg(test)]
mod tests {
    use bytes::BytesMut;

    use super::*;

    #[test]
    fn len_len_matches_expected_thresholds() {
        assert_eq!(len_len(0), 1);
        assert_eq!(len_len(127), 1);
        assert_eq!(len_len(128), 2);
        assert_eq!(len_len(16_383), 2);
        assert_eq!(len_len(16_384), 3);
        assert_eq!(len_len(2_097_151), 3);
        assert_eq!(len_len(2_097_152), 4);
    }

    #[test]
    fn write_remaining_length_round_trip() {
        for len in [0usize, 127, 128, 321, 16_384, 268_435_455] {
            let mut b = BytesMut::new();
            let count = write_remaining_length(&mut b, len).unwrap();
            let (decoded_count, decoded) = length(b.iter()).unwrap();
            assert_eq!(count, decoded_count);
            assert_eq!(decoded, len);
        }
    }

    #[test]
    fn check_reports_missing_bytes() {
        let b = [0x30u8, 0x05, 1, 2];
        let result = check(b.iter());
        assert_eq!(result, Err(Error::InsufficientBytes(3)));
    }

    #[test]
    fn mqtt_discouraged_code_points_are_detected() {
        assert_eq!(
            first_mqtt_discouraged_code_point("a\u{0000}b"),
            Some(0x0000)
        );
        assert_eq!(first_mqtt_discouraged_code_point("\u{009F}"), Some(0x009F));
        assert_eq!(first_mqtt_discouraged_code_point("\u{FDEF}"), Some(0xFDEF));
        assert_eq!(
            first_mqtt_discouraged_code_point("\u{1FFFF}"),
            Some(0x1FFFF)
        );
    }

    #[test]
    fn mqtt_non_discouraged_code_points_pass() {
        assert_eq!(first_mqtt_discouraged_code_point("hello/world"), None);
        assert_eq!(first_mqtt_discouraged_code_point("cafe\u{00E9}"), None);
    }
}
