//! # mqttbytes
//!
//! This module contains the low level struct definitions required to assemble and disassemble MQTT 3.1.1 packets in rumqttc.
//! The [`bytes`](https://docs.rs/bytes) crate is used internally.

use bytes::{BufMut, Bytes, BytesMut};
use core::fmt;
use mqttbytes_core::primitives::{self as core_primitives, Error as PrimitiveError};
use std::slice::Iter;

pub mod v4;

pub use mqttbytes_core::{QoS, has_wildcards, matches, valid_filter, valid_topic};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Utf8ComplianceMode {
    #[default]
    Permissive,
    Warn,
    Strict,
}

/// Error during serialization and deserialization
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Expected Connect, received: {0:?}")]
    NotConnect(PacketType),
    #[error("Unexpected Connect")]
    UnexpectedConnect,
    #[error("Invalid Connect return code: {0}")]
    InvalidConnectReturnCode(u8),
    #[error("Invalid protocol")]
    InvalidProtocol,
    #[error("Invalid protocol level: {0}")]
    InvalidProtocolLevel(u8),
    #[error("Incorrect packet format")]
    IncorrectPacketFormat,
    #[error("Invalid packet type: {0}")]
    InvalidPacketType(u8),
    #[error("Invalid property type: {0}")]
    InvalidPropertyType(u8),
    #[error("Invalid QoS level: {0}")]
    InvalidQoS(u8),
    #[error("Invalid subscribe reason code: {0}")]
    InvalidSubscribeReasonCode(u8),
    #[error("Packet id Zero")]
    PacketIdZero,
    #[error("Payload size is incorrect")]
    PayloadSizeIncorrect,
    #[error("payload is too long")]
    PayloadTooLong,
    #[error("payload size limit exceeded: {0}")]
    PayloadSizeLimitExceeded(usize),
    #[error("Payload required")]
    PayloadRequired,
    #[error("Topic is not UTF-8")]
    TopicNotUtf8,
    #[error("Promised boundary crossed: {0}")]
    BoundaryCrossed(usize),
    #[error("Malformed packet")]
    MalformedPacket,
    #[error("Malformed remaining length")]
    MalformedRemainingLength,
    #[error("A Subscribe packet must contain atleast one filter")]
    EmptySubscription,
    /// More bytes required to frame packet. Argument
    /// implies minimum additional bytes required to
    /// proceed further
    #[error("At least {0} more bytes required to frame packet")]
    InsufficientBytes(usize),
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "Cannot send packet of size '{pkt_size:?}'. It's greater than the broker's maximum packet size of: '{max:?}'"
    )]
    OutgoingPacketTooLarge { pkt_size: usize, max: usize },
}

/// MQTT packet type
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    Connect = 1,
    ConnAck,
    Publish,
    PubAck,
    PubRec,
    PubRel,
    PubComp,
    Subscribe,
    SubAck,
    Unsubscribe,
    UnsubAck,
    PingReq,
    PingResp,
    Disconnect,
}

/// Protocol type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    V4,
    V5,
}

/// Packet type from a byte
///
/// ```text
///          7                          3                          0
///          +--------------------------+--------------------------+
/// byte 1   | MQTT Control Packet Type | Flags for each type      |
///          +--------------------------+--------------------------+
///          |         Remaining Bytes Len  (1/2/3/4 bytes)        |
///          +-----------------------------------------------------+
///
/// <https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349207>
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
pub struct FixedHeader {
    /// First byte of the stream. Used to identify packet types and
    /// several flags
    byte1: u8,
    /// Length of fixed header. Byte 1 + (1..4) bytes. So fixed header
    /// len can vary from 2 bytes to 5 bytes
    /// 1..4 bytes are variable length encoded to represent remaining length
    fixed_header_len: usize,
    /// Remaining length of the packet. Doesn't include fixed header bytes
    /// Represents variable header + payload size
    remaining_len: usize,
}

impl FixedHeader {
    #[must_use]
    pub fn new(byte1: u8, remaining_len_len: usize, remaining_len: usize) -> FixedHeader {
        FixedHeader {
            byte1,
            fixed_header_len: remaining_len_len + 1,
            remaining_len,
        }
    }

    pub fn packet_type(&self) -> Result<PacketType, Error> {
        let num = self.byte1 >> 4;
        match num {
            1 => Ok(PacketType::Connect),
            2 => Ok(PacketType::ConnAck),
            3 => Ok(PacketType::Publish),
            4 => Ok(PacketType::PubAck),
            5 => Ok(PacketType::PubRec),
            6 => Ok(PacketType::PubRel),
            7 => Ok(PacketType::PubComp),
            8 => Ok(PacketType::Subscribe),
            9 => Ok(PacketType::SubAck),
            10 => Ok(PacketType::Unsubscribe),
            11 => Ok(PacketType::UnsubAck),
            12 => Ok(PacketType::PingReq),
            13 => Ok(PacketType::PingResp),
            14 => Ok(PacketType::Disconnect),
            _ => Err(Error::InvalidPacketType(num)),
        }
    }

    /// Returns the size of full packet (fixed header + variable header + payload)
    /// Fixed header is enough to get the size of a frame in the stream
    #[must_use]
    pub fn frame_length(&self) -> usize {
        self.fixed_header_len + self.remaining_len
    }
}

/// Checks if the stream has enough bytes to frame a packet and returns fixed header
/// only if a packet can be framed with existing bytes in the `stream`.
/// The passed stream doesn't modify parent stream's cursor. If this function
/// returned an error, next `check` on the same parent stream is forced start
/// with cursor at 0 again (Iter is owned. Only Iter's cursor is changed internally)
pub fn check(stream: Iter<u8>, max_packet_size: usize) -> Result<FixedHeader, Error> {
    let stream_len = stream.len();
    let fixed_header = parse_fixed_header(stream)?;

    // Don't let rogue connections attack with huge payloads.
    // Disconnect them before reading all that data
    if fixed_header.remaining_len > max_packet_size {
        return Err(Error::PayloadSizeLimitExceeded(fixed_header.remaining_len));
    }

    let frame_length = fixed_header.frame_length();
    if stream_len < frame_length {
        return Err(Error::InsufficientBytes(frame_length - stream_len));
    }

    Ok(fixed_header)
}

fn parse_fixed_header(stream: Iter<u8>) -> Result<FixedHeader, Error> {
    let fixed_header = core_primitives::parse_fixed_header(stream).map_err(map_primitive_error)?;
    Ok(FixedHeader::new(
        fixed_header.byte1,
        fixed_header.remaining_len_len,
        fixed_header.remaining_len,
    ))
}

/// Reads a series of bytes with a length from a byte stream
fn read_mqtt_bytes(stream: &mut Bytes) -> Result<Bytes, Error> {
    core_primitives::read_mqtt_bytes(stream).map_err(map_primitive_error)
}

/// Reads a string from bytes stream
fn read_mqtt_string(stream: &mut Bytes) -> Result<String, Error> {
    core_primitives::read_mqtt_string(stream).map_err(map_primitive_error)
}

pub(crate) fn read_mqtt_string_with_mode(
    stream: &mut Bytes,
    mode: Utf8ComplianceMode,
    warned: &mut bool,
    field: &'static str,
) -> Result<String, Error> {
    let value = read_mqtt_string(stream)?;
    validate_utf8_string_with_mode(&value, mode, warned, "decode", field)?;
    Ok(value)
}

/// Serializes bytes to stream (including length)
fn write_mqtt_bytes(stream: &mut BytesMut, bytes: &[u8]) {
    core_primitives::write_mqtt_bytes(stream, bytes);
}

/// Serializes a string to stream
fn write_mqtt_string(stream: &mut BytesMut, string: &str) {
    core_primitives::write_mqtt_string(stream, string);
}

pub(crate) fn write_mqtt_string_with_mode(
    stream: &mut BytesMut,
    string: &str,
    mode: Utf8ComplianceMode,
    warned: &mut bool,
    field: &'static str,
) -> Result<(), Error> {
    validate_utf8_string_with_mode(string, mode, warned, "encode", field)?;
    write_mqtt_string(stream, string);
    Ok(())
}

/// Writes remaining length to stream and returns number of bytes for remaining length
fn write_remaining_length(stream: &mut BytesMut, len: usize) -> Result<usize, Error> {
    core_primitives::write_remaining_length(stream, len).map_err(map_primitive_error)
}

/// Maps a number to QoS
pub fn qos(num: u8) -> Result<QoS, Error> {
    mqttbytes_core::qos(num).ok_or(Error::InvalidQoS(num))
}

/// After collecting enough bytes to frame a packet (packet's frame())
/// , It's possible that content itself in the stream is wrong. Like expected
/// packet id or qos not being present. In cases where `read_mqtt_string` or
/// `read_mqtt_bytes` exhausted remaining length but packet framing expects to
/// parse qos next, these pre checks will prevent `bytes` crashes
fn read_u16(stream: &mut Bytes) -> Result<u16, Error> {
    core_primitives::read_u16(stream).map_err(map_primitive_error)
}

fn read_u8(stream: &mut Bytes) -> Result<u8, Error> {
    core_primitives::read_u8(stream).map_err(map_primitive_error)
}

pub(crate) fn validate_utf8_slice_with_mode(
    bytes: &[u8],
    mode: Utf8ComplianceMode,
    warned: &mut bool,
    phase: &'static str,
    field: &'static str,
) -> Result<(), Error> {
    let value = std::str::from_utf8(bytes).map_err(|_| Error::TopicNotUtf8)?;
    validate_utf8_string_with_mode(value, mode, warned, phase, field)
}

fn validate_utf8_string_with_mode(
    value: &str,
    mode: Utf8ComplianceMode,
    warned: &mut bool,
    phase: &'static str,
    field: &'static str,
) -> Result<(), Error> {
    if mode == Utf8ComplianceMode::Permissive {
        return Ok(());
    }

    let Some(code_point) = core_primitives::first_mqtt_discouraged_code_point(value) else {
        return Ok(());
    };

    match mode {
        Utf8ComplianceMode::Permissive => Ok(()),
        Utf8ComplianceMode::Warn => {
            if !*warned {
                warn!(
                    "MQTT UTF-8 {} field '{}' contains discouraged code point U+{:04X}; accepting in Warn mode",
                    phase, field, code_point
                );
                *warned = true;
            }
            Ok(())
        }
        Utf8ComplianceMode::Strict => Err(Error::IncorrectPacketFormat),
    }
}

fn map_primitive_error(error: PrimitiveError) -> Error {
    match error {
        PrimitiveError::PayloadTooLong => Error::PayloadTooLong,
        PrimitiveError::BoundaryCrossed(len) => Error::BoundaryCrossed(len),
        PrimitiveError::MalformedPacket => Error::MalformedPacket,
        PrimitiveError::MalformedRemainingLength => Error::MalformedRemainingLength,
        PrimitiveError::TopicNotUtf8 => Error::TopicNotUtf8,
        PrimitiveError::InsufficientBytes(required) => Error::InsufficientBytes(required),
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, check};

    #[test]
    fn check_rejects_oversized_packet_on_partial_frame() {
        let stream = [0x30, 0x14];
        let result = check(stream.iter(), 10);

        assert!(matches!(result, Err(Error::PayloadSizeLimitExceeded(20))));
    }
}
