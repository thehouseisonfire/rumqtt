use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::slice::Iter;
use std::str::Utf8Error;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("Payload is too long: {actual} bytes exceeds max {max} bytes")]
    PayloadTooLong { actual: usize, max: usize },
    #[error("Promised boundary crossed, contains {0} bytes")]
    BoundaryCrossed(usize),
    #[error("Packet is malformed")]
    MalformedPacket,
    #[error("Remaining length is malformed")]
    MalformedRemainingLength,
    #[error("Topic not utf-8: {source}")]
    TopicNotUtf8 { source: Utf8Error },
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
    pub const fn frame_length(self) -> usize {
        1 + self.remaining_len_len + self.remaining_len
    }
}

/// Checks whether the buffer contains a complete MQTT frame header and payload.
///
/// # Errors
///
/// Returns [`Error::InsufficientBytes`] when the full frame has not arrived yet,
/// or another framing error if the fixed header is malformed.
pub fn check(stream: Iter<u8>) -> Result<ParsedFixedHeader, Error> {
    let stream_len = stream.len();
    let fixed_header = parse_fixed_header(stream)?;

    let frame_length = fixed_header.frame_length();
    if stream_len < frame_length {
        return Err(Error::InsufficientBytes(frame_length - stream_len));
    }

    Ok(fixed_header)
}

/// Parses the fixed header from the provided iterator.
///
/// # Errors
///
/// Returns an error when the header is incomplete or the remaining length field
/// is malformed.
///
/// # Panics
///
/// Panics only if the iterator yields fewer than two bytes after the explicit
/// length check above, which would indicate a broken iterator implementation.
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
///
/// # Errors
///
/// Returns an error when the variable-length integer is incomplete or exceeds
/// the MQTT maximum encoding width.
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
        let byte = usize::from(*byte);
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
///
/// # Errors
///
/// Returns an error when the stream does not contain enough bytes for the
/// length prefix or the declared payload length crosses the packet boundary.
pub fn read_mqtt_bytes(stream: &mut Bytes) -> Result<Bytes, Error> {
    let len = usize::from(read_u16(stream)?);

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
///
/// # Errors
///
/// Returns an error when the stream does not contain a complete MQTT string or
/// when the bytes do not satisfy MQTT UTF-8 string requirements.
pub fn read_mqtt_string(stream: &mut Bytes) -> Result<String, Error> {
    let s = read_mqtt_bytes(stream)?;
    let s = validate_mqtt_string(&s)?;
    Ok(s.to_owned())
}

/// Validates MQTT UTF-8 encoded string bytes without allocating.
///
/// # Errors
///
/// Returns the validated borrowed string on success, [`Error::TopicNotUtf8`]
/// when the bytes are not well-formed UTF-8, preserving the underlying
/// [`Utf8Error`], and [`Error::MalformedPacket`] when the string contains
/// U+0000.
pub fn validate_mqtt_string(bytes: &[u8]) -> Result<&str, Error> {
    let string = std::str::from_utf8(bytes).map_err(|source| Error::TopicNotUtf8 { source })?;

    if bytes.contains(&0) {
        return Err(Error::MalformedPacket);
    }

    Ok(string)
}

/// Serializes bytes to stream (including length)
///
/// # Errors
///
/// Returns [`Error::PayloadTooLong`] when `bytes` cannot fit in an MQTT
/// two-byte length-prefixed field.
pub fn write_mqtt_bytes(stream: &mut BytesMut, bytes: &[u8]) -> Result<(), Error> {
    let len = u16::try_from(bytes.len()).map_err(|_| Error::PayloadTooLong {
        actual: bytes.len(),
        max: usize::from(u16::MAX),
    })?;
    stream.put_u16(len);
    stream.extend_from_slice(bytes);
    Ok(())
}

/// Serializes a string to stream
///
/// # Errors
///
/// Returns [`Error::PayloadTooLong`] when `string` cannot fit in an MQTT
/// two-byte length-prefixed field.
pub fn write_mqtt_string(stream: &mut BytesMut, string: &str) -> Result<(), Error> {
    write_mqtt_bytes(stream, string.as_bytes())
}

/// Writes remaining length to stream and returns number of bytes for remaining length
///
/// # Errors
///
/// Returns [`Error::PayloadTooLong`] when `len` exceeds the MQTT remaining
/// length limit.
///
/// # Panics
///
/// Panics only if converting a remainder in `0..=127` to `u8` fails, which
/// cannot happen for valid Rust integer conversions.
pub fn write_remaining_length(stream: &mut BytesMut, len: usize) -> Result<usize, Error> {
    if len > 268_435_455 {
        return Err(Error::PayloadTooLong {
            actual: len,
            max: 268_435_455,
        });
    }

    let mut done = false;
    let mut x = len;
    let mut count = 0;

    while !done {
        // `x % 128` is always in 0..=127 and therefore fits in u8.
        let mut byte = (x % 128) as u8;
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
pub const fn len_len(len: usize) -> usize {
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

/// After collecting enough bytes to frame a packet, the packet payload itself
/// can still be malformed.
///
/// For example, a packet may be missing an expected packet identifier or `QoS`
/// field. These pre-checks prevent `bytes` panics when `read_mqtt_string` or
/// `read_mqtt_bytes` exhaust the remaining length before the packet parser
/// reaches the next expected field.
///
/// # Errors
///
/// Returns [`Error::MalformedPacket`] when fewer than two bytes remain.
pub fn read_u16(stream: &mut Bytes) -> Result<u16, Error> {
    if stream.len() < 2 {
        return Err(Error::MalformedPacket);
    }

    Ok(stream.get_u16())
}

/// Reads the next byte from the stream.
///
/// # Errors
///
/// Returns [`Error::MalformedPacket`] when the stream is empty.
pub fn read_u8(stream: &mut Bytes) -> Result<u8, Error> {
    if stream.is_empty() {
        return Err(Error::MalformedPacket);
    }

    Ok(stream.get_u8())
}

/// Reads the next big-endian `u32` from the stream.
///
/// # Errors
///
/// Returns [`Error::MalformedPacket`] when fewer than four bytes remain.
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
            assert_eq!(count, len_len(len));
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

    /// MQTT-1.5.3-1 / MQTT-1.5.3-2: UTF-8 encoded strings MUST be well-formed
    /// and MUST NOT include encodings of code points between U+D800 and U+DFFF.
    #[test]
    fn read_mqtt_string_rejects_surrogate_ud800() {
        // U+D800 encoded as CESU-8: 0xED 0xA0 0x80
        let mut bytes = BytesMut::new();
        bytes.put_u16(3);
        bytes.extend_from_slice(&[0xED, 0xA0, 0x80]);
        let mut stream = bytes.freeze();
        let result = read_mqtt_string(&mut stream);
        assert!(matches!(result, Err(Error::TopicNotUtf8 { .. })));
    }

    #[test]
    fn read_mqtt_string_rejects_surrogate_udfff() {
        // U+DFFF encoded as CESU-8: 0xED 0xBF 0xBF
        let mut bytes = BytesMut::new();
        bytes.put_u16(3);
        bytes.extend_from_slice(&[0xED, 0xBF, 0xBF]);
        let mut stream = bytes.freeze();
        let result = read_mqtt_string(&mut stream);
        assert!(matches!(result, Err(Error::TopicNotUtf8 { .. })));
    }

    #[test]
    fn read_mqtt_string_rejects_surrogate_ud801() {
        // U+D801 encoded as CESU-8: 0xED 0xA0 0x81
        let mut bytes = BytesMut::new();
        bytes.put_u16(3);
        bytes.extend_from_slice(&[0xED, 0xA0, 0x81]);
        let mut stream = bytes.freeze();
        let result = read_mqtt_string(&mut stream);
        assert!(matches!(result, Err(Error::TopicNotUtf8 { .. })));
    }

    #[test]
    fn read_mqtt_string_rejects_overlong_encoding() {
        // Overlong encoding of U+0000: 0xC0 0x80 (not valid UTF-8)
        let mut bytes = BytesMut::new();
        bytes.put_u16(2);
        bytes.extend_from_slice(&[0xC0, 0x80]);
        let mut stream = bytes.freeze();
        let result = read_mqtt_string(&mut stream);
        assert!(matches!(result, Err(Error::TopicNotUtf8 { .. })));
    }

    #[test]
    fn read_mqtt_string_rejects_truncated_sequence() {
        // Start of a 3-byte sequence (0xE0) but missing continuation bytes
        let mut bytes = BytesMut::new();
        bytes.put_u16(1);
        bytes.extend_from_slice(&[0xE0]);
        let mut stream = bytes.freeze();
        let result = read_mqtt_string(&mut stream);
        assert!(matches!(result, Err(Error::TopicNotUtf8 { .. })));
    }

    #[test]
    fn read_mqtt_string_rejects_invalid_continuation_byte() {
        // 0xC2 followed by 0x00 (invalid continuation byte)
        let mut bytes = BytesMut::new();
        bytes.put_u16(2);
        bytes.extend_from_slice(&[0xC2, 0x00]);
        let mut stream = bytes.freeze();
        let result = read_mqtt_string(&mut stream);
        assert!(matches!(result, Err(Error::TopicNotUtf8 { .. })));
    }

    #[test]
    fn read_mqtt_string_rejects_null_character() {
        let mut bytes = BytesMut::new();
        bytes.put_u16(3);
        bytes.extend_from_slice(b"a\0b");
        let mut stream = bytes.freeze();
        let result = read_mqtt_string(&mut stream);
        assert!(matches!(result, Err(Error::MalformedPacket)));
    }

    #[test]
    fn read_mqtt_string_accepts_valid_utf8() {
        let mut bytes = BytesMut::new();
        bytes.put_u16(5);
        bytes.extend_from_slice("a/b/c".as_bytes());
        let mut stream = bytes.freeze();
        let result = read_mqtt_string(&mut stream);
        assert_eq!(result.unwrap(), "a/b/c");
    }

    #[test]
    fn read_mqtt_string_accepts_multibyte_valid_utf8() {
        // U+1F600 (😀) encoded as 4-byte UTF-8: 0xF0 0x9F 0x98 0x80
        let mut bytes = BytesMut::new();
        bytes.put_u16(4);
        bytes.extend_from_slice(&[0xF0, 0x9F, 0x98, 0x80]);
        let mut stream = bytes.freeze();
        let result = read_mqtt_string(&mut stream);
        assert_eq!(result.unwrap(), "😀");
    }

    #[test]
    fn read_mqtt_string_preserves_bom() {
        let mut bytes = BytesMut::new();
        bytes.put_u16(6);
        bytes.extend_from_slice("\u{FEFF}abc".as_bytes());
        let mut stream = bytes.freeze();
        let result = read_mqtt_string(&mut stream).unwrap();
        assert_eq!(result, "\u{FEFF}abc");
    }

    #[test]
    fn write_mqtt_bytes_rejects_payloads_larger_than_u16() {
        let bytes = vec![0; usize::from(u16::MAX) + 1];
        let mut stream = BytesMut::new();

        let result = write_mqtt_bytes(&mut stream, &bytes);

        assert_eq!(
            result,
            Err(Error::PayloadTooLong {
                actual: bytes.len(),
                max: usize::from(u16::MAX),
            })
        );
        assert!(stream.is_empty());
    }

    #[test]
    fn write_mqtt_string_rejects_payloads_larger_than_u16() {
        let string = "a".repeat(usize::from(u16::MAX) + 1);
        let mut stream = BytesMut::new();

        let result = write_mqtt_string(&mut stream, &string);

        assert_eq!(
            result,
            Err(Error::PayloadTooLong {
                actual: string.len(),
                max: usize::from(u16::MAX),
            })
        );
        assert!(stream.is_empty());
    }
}
