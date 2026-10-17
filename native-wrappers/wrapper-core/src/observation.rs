//! Optional observations of native processing and scheduling, never ACK authority.
use std::sync::Arc;

use crate::{AuthProperties, ProtocolVersion};

/// Startup-only observation selection. Each category is independent and defaults off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProtocolObservationSelection {
    pub incoming_control: bool,
    pub precise_outgoing: bool,
    pub raw_auth: bool,
}

impl ProtocolObservationSelection {
    #[must_use]
    pub const fn enabled(self) -> bool {
        self.incoming_control || self.precise_outgoing || self.raw_auth
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketDirection {
    Incoming = 1,
    Outgoing = 2,
}

/// MQTT control packet numbers. A variant does not imply native emission support.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketKind {
    Publish = 3,
    PubAck = 4,
    PubRec = 5,
    PubRel = 6,
    PubComp = 7,
    Subscribe = 8,
    SubAck = 9,
    Unsubscribe = 10,
    UnsubAck = 11,
    PingReq = 12,
    PingResp = 13,
    Disconnect = 14,
    Auth = 15,
}

/// Decoded packet properties. Contents are deliberately omitted from Debug.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ObservationProperties {
    pub(crate) reason_string: Option<String>,
    pub(crate) user_properties: Vec<(String, String)>,
    pub(crate) authentication_method: Option<String>,
    pub(crate) authentication_data: Option<bytes::Bytes>,
}

impl std::fmt::Debug for ObservationProperties {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObservationProperties")
            .finish_non_exhaustive()
    }
}

impl ObservationProperties {
    #[must_use]
    pub fn reason_string(&self) -> Option<&str> {
        self.reason_string.as_deref()
    }
    #[must_use]
    pub fn user_properties(&self) -> &[(String, String)] {
        &self.user_properties
    }
    #[must_use]
    pub fn authentication_method(&self) -> Option<&str> {
        self.authentication_method.as_deref()
    }
    #[must_use]
    pub fn authentication_data(&self) -> Option<&[u8]> {
        self.authentication_data.as_deref()
    }
}

impl From<&AuthProperties> for ObservationProperties {
    fn from(p: &AuthProperties) -> Self {
        Self {
            reason_string: p.reason_string.clone(),
            user_properties: p.user_properties.clone(),
            authentication_method: p.method.clone(),
            authentication_data: p.data.clone(),
        }
    }
}

/// An immutable decoded observation, owned by an event. No wire encoding or flush guarantee.
#[derive(Debug, PartialEq, Eq)]
pub struct ProtocolObservation {
    pub(crate) protocol: ProtocolVersion,
    pub(crate) direction: PacketDirection,
    pub(crate) kind: PacketKind,
    pub(crate) packet_id: Option<u16>,
    pub(crate) connection_generation: Option<u64>,
    pub(crate) reason_code: Option<u8>,
    pub(crate) filter_reason_codes: Option<Box<[u8]>>,
    /// False when native events lack contents or the protocol has no properties.
    pub(crate) properties_available: bool,
    pub(crate) properties: Option<ObservationProperties>,
}

impl ProtocolObservation {
    pub(crate) const fn new(
        protocol: ProtocolVersion,
        direction: PacketDirection,
        kind: PacketKind,
        packet_id: Option<u16>,
        connection_generation: Option<u64>,
    ) -> Self {
        Self {
            protocol,
            direction,
            kind,
            packet_id,
            connection_generation,
            reason_code: None,
            filter_reason_codes: None,
            properties_available: false,
            properties: None,
        }
    }
    #[must_use]
    pub const fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }
    #[must_use]
    pub const fn direction(&self) -> PacketDirection {
        self.direction
    }
    #[must_use]
    pub const fn kind(&self) -> PacketKind {
        self.kind
    }
    #[must_use]
    pub const fn packet_id(&self) -> Option<u16> {
        self.packet_id
    }
    /// Accepted connection generation; absent during pre-CONNACK handshakes, including retries.
    #[must_use]
    pub const fn connection_generation(&self) -> Option<u64> {
        self.connection_generation
    }
    #[must_use]
    pub const fn reason_code(&self) -> Option<u8> {
        self.reason_code
    }
    #[must_use]
    pub fn filter_reason_codes(&self) -> Option<&[u8]> {
        self.filter_reason_codes.as_deref()
    }
    #[must_use]
    pub const fn properties_available(&self) -> bool {
        self.properties_available
    }
    #[must_use]
    pub const fn properties(&self) -> Option<&ObservationProperties> {
        self.properties.as_ref()
    }
}

impl crate::WrapperEvent {
    /// Precise details exist only for selected control observations and enriched outgoing events.
    #[must_use]
    pub fn protocol_observation(&self) -> Option<&ProtocolObservation> {
        match self {
            Self::ProtocolObservation(value) => Some(value),
            Self::Outgoing(value) => value.observation.as_deref(),
            _ => None,
        }
    }
}

pub fn event(value: ProtocolObservation) -> crate::WrapperEvent {
    crate::WrapperEvent::ProtocolObservation(Arc::new(value))
}
