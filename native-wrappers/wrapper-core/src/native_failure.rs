//! Bounded, owner-free evidence extracted from native errors. These classifications
//! describe observations, never retry policy or delivery guarantees.

macro_rules! codes {
    ($(#[$meta:meta])* pub enum $name:ident { $($variant:ident = $code:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[non_exhaustive]
        pub enum $name { $($variant),+ }
        impl $name {
            /// Stable wrapper-owned number, independent of Rust enum layout.
            #[must_use]
            pub const fn code(self) -> u32 {
                match self { $(Self::$variant => $code),+ }
            }
        }
    };
}

codes! {
    pub enum NativeFailureDomain {
        Unknown = 0, Connection = 1, Io = 2, Protocol = 3,
        Tls = 4, Proxy = 5, WebSocket = 6,
    }
}
codes! {
    pub enum NativeFailureCode {
        Unknown = 0, Io = 1, ConnectionTimeout = 2, FlushTimeout = 3,
        DisconnectTimeout = 4, PeerClosed = 5, PingResponseTimeout = 6,
        CollisionTimeout = 7, UnsolicitedAck = 8, MalformedPacket = 9,
        UnexpectedPacket = 10, DuplicateConnAck = 11, AckCountMismatch = 12,
        SessionMismatch = 13, IncomingTooLarge = 14, OutgoingTooLarge = 15,
        InvalidState = 16, InvalidRequest = 17, WindowFull = 18,
        CapabilityUnsupported = 19, AliasOutOfRange = 20, BrokerRefused = 21,
        BrokerDisconnect = 22, Authentication = 23, Configuration = 24,
        Certificate = 25, TlsProtocol = 26, ProxyAuthentication = 27,
        ProxyPolicy = 28, ProxyUnavailable = 29, WebSocketHttp = 30,
        WebSocketProtocol = 31, Callback = 32, Shutdown = 33,
        Recovery = 34, Redirect = 35,
    }
}
codes! {
    /// Portable subset of `std::io::ErrorKind`; new/unmapped kinds use Unknown.
    pub enum NativeIoKind {
        Unknown = 0, NotFound = 1, PermissionDenied = 2, ConnectionRefused = 3,
        ConnectionReset = 4, HostUnreachable = 5, NetworkUnreachable = 6,
        ConnectionAborted = 7, NotConnected = 8, AddrInUse = 9,
        AddrNotAvailable = 10, NetworkDown = 11, BrokenPipe = 12,
        AlreadyExists = 13, WouldBlock = 14, InvalidInput = 15,
        InvalidData = 16, TimedOut = 17, WriteZero = 18,
        Interrupted = 19, Unsupported = 20, UnexpectedEof = 21,
        OutOfMemory = 22, Other = 23,
    }
}
codes! {
    /// The executing target's OS error namespace, not a portable errno number.
    pub enum NativeOsDomain {
        Unknown = 0, Linux = 1, Darwin = 2, Windows = 3, OtherUnix = 4,
    }
}
codes! {
    /// Semantics of all present actual/maximum/expected values in one record.
    pub enum NativeFailureQuantity {
        PacketBytes = 1, RemainingBytes = 2, Items = 3,
        QoS = 4, TopicAlias = 5, AdditionalBytes = 6,
    }
}

/// Fixed-size scalar evidence. `None` means unavailable, never an inferred zero.
///
/// Packet kinds use MQTT control packet numbers (1..=15), including CONNECT and
/// CONNACK. An unsolicited ACK can have an ID without an identifiable packet kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeFailureDetails {
    pub domain: NativeFailureDomain,
    pub code: NativeFailureCode,
    pub cause: Option<(NativeFailureDomain, NativeFailureCode)>,
    pub io_kind: Option<NativeIoKind>,
    pub os_error: Option<(NativeOsDomain, i32)>,
    pub packet_kind: Option<u8>,
    pub packet_id: Option<u16>,
    pub http_status: Option<u16>,
    pub quantity: Option<NativeFailureQuantity>,
    pub actual: Option<u64>,
    pub maximum: Option<u64>,
    pub expected: Option<u64>,
}

impl NativeFailureDetails {
    pub(crate) const fn new(domain: NativeFailureDomain, code: NativeFailureCode) -> Self {
        Self {
            domain,
            code,
            cause: None,
            io_kind: None,
            os_error: None,
            packet_kind: None,
            packet_id: None,
            http_status: None,
            quantity: None,
            actual: None,
            maximum: None,
            expected: None,
        }
    }

    pub(crate) const fn measured(
        mut self,
        quantity: NativeFailureQuantity,
        actual: Option<u64>,
        maximum: Option<u64>,
        expected: Option<u64>,
    ) -> Self {
        self.quantity = Some(quantity);
        self.actual = actual;
        self.maximum = maximum;
        self.expected = expected;
        self
    }
}

impl From<std::io::ErrorKind> for NativeIoKind {
    fn from(kind: std::io::ErrorKind) -> Self {
        use std::io::ErrorKind as K;
        match kind {
            K::NotFound => Self::NotFound,
            K::PermissionDenied => Self::PermissionDenied,
            K::ConnectionRefused => Self::ConnectionRefused,
            K::ConnectionReset => Self::ConnectionReset,
            K::HostUnreachable => Self::HostUnreachable,
            K::NetworkUnreachable => Self::NetworkUnreachable,
            K::ConnectionAborted => Self::ConnectionAborted,
            K::NotConnected => Self::NotConnected,
            K::AddrInUse => Self::AddrInUse,
            K::AddrNotAvailable => Self::AddrNotAvailable,
            K::NetworkDown => Self::NetworkDown,
            K::BrokenPipe => Self::BrokenPipe,
            K::AlreadyExists => Self::AlreadyExists,
            K::WouldBlock => Self::WouldBlock,
            K::InvalidInput => Self::InvalidInput,
            K::InvalidData => Self::InvalidData,
            K::TimedOut => Self::TimedOut,
            K::WriteZero => Self::WriteZero,
            K::Interrupted => Self::Interrupted,
            K::Unsupported => Self::Unsupported,
            K::UnexpectedEof => Self::UnexpectedEof,
            K::OutOfMemory => Self::OutOfMemory,
            K::Other => Self::Other,
            _ => Self::Unknown,
        }
    }
}

pub const fn os_domain() -> NativeOsDomain {
    if cfg!(target_os = "linux") {
        NativeOsDomain::Linux
    } else if cfg!(target_vendor = "apple") {
        NativeOsDomain::Darwin
    } else if cfg!(windows) {
        NativeOsDomain::Windows
    } else if cfg!(unix) {
        NativeOsDomain::OtherUnix
    } else {
        NativeOsDomain::Unknown
    }
}
