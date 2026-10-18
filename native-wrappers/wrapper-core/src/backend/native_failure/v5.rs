use super::{C, D, Details, Q, SOURCE_BUDGET, enter, io};
use rumqttc_v5 as native;

pub fn connection(error: &native::ConnectionError) -> Option<Details> {
    let mut budget = SOURCE_BUDGET;
    connection_with_budget(error, &mut budget)
}

fn connection_with_budget(error: &native::ConnectionError, budget: &mut usize) -> Option<Details> {
    use native::ConnectionError as E;
    if !enter(budget) {
        return Some(Details::new(D::Connection, C::Unknown));
    }
    Some(match error {
        E::MqttState(error) => state(error, budget),
        E::Io(error) => io(error, budget),
        E::Timeout(_) => Details::new(D::Connection, C::ConnectionTimeout),

        E::DisconnectTimeout => Details::new(D::Connection, C::DisconnectTimeout),
        E::ConnectionRefused(_) => Details::new(D::Connection, C::BrokerRefused),
        E::NotConnAck(packet) => {
            let mut details = Details::new(D::Protocol, C::UnexpectedPacket);
            details.packet_kind = Some(packet_kind(packet.packet_type()));
            details
        }
        E::SessionStateMismatch { .. } => Details::new(D::Protocol, C::SessionMismatch),
        E::BrokerTransportMismatch => Details::new(D::Connection, C::Configuration),
        E::RequestsDone => Details::new(D::Connection, C::Shutdown),
        E::SessionRecoveryPending | E::SessionRecoveryInvalid => {
            Details::new(D::Connection, C::Recovery)
        }
        // These domains already have dedicated typed details. Never chase their sources.
        E::SessionStore(_) | E::SessionRestore(_) => return None,
        #[cfg(feature = "ordered-shutdown")]
        E::OrderedDisconnect(_) => return None,
        #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
        E::Tls(error) => super::tls(error, budget),
        #[cfg(any(feature = "http-proxy", feature = "socks-proxy"))]
        E::Proxy(error) => super::proxy(error, budget),
        #[cfg(feature = "websocket")]
        E::Websocket(error) => super::websocket(error, budget),
        #[cfg(feature = "websocket")]
        E::WsConnect(_) | E::InvalidUrl(_) => Details::new(D::WebSocket, C::Configuration),
        #[cfg(feature = "websocket")]
        E::ResponseValidation(_) => Details::new(D::WebSocket, C::WebSocketProtocol),
        #[cfg(feature = "websocket")]
        E::RequestModifier(_) => Details::new(D::WebSocket, C::Callback),
        E::AuthProcessingError => Details::new(D::Protocol, C::Authentication),
        E::Redirect(error) => match &error.failure {
            native::RedirectFailure::FollowFailed(source)
            | native::RedirectFailure::SrvTargetsExhausted {
                last_error: source, ..
            } => {
                // Dedicated redirect fields retain the outer failure; supplement only
                // with the target's safe native evidence, under the same budget.
                return connection_with_budget(source, budget);
            }
            _ => return None,
        },
        _ => Details::new(D::Connection, C::Unknown),
    })
}

fn state(error: &native::StateError, budget: &mut usize) -> Details {
    use native::StateError as E;
    match error {
        E::Io(error) => io(error, budget),
        E::Unsolicited(id) => {
            let mut details = Details::new(D::Protocol, C::UnsolicitedAck);
            details.packet_id = Some(*id);
            details
        }
        E::AwaitPingResp => Details::new(D::Connection, C::PingResponseTimeout),
        E::CollisionTimeout => Details::new(D::Connection, C::CollisionTimeout),
        E::ConnectionAborted => Details::new(D::Io, C::PeerClosed),
        E::InvalidState => Details::new(D::Protocol, C::InvalidState),
        E::EmptySubscription | E::EmptyUnsubscription => {
            Details::new(D::Protocol, C::InvalidRequest)
        }
        E::OutgoingPublishQuotaExceeded { maximum } => Details::new(D::Protocol, C::WindowFull)
            .measured(Q::Items, None, Some(u64::from(*maximum)), None),
        E::Deserialization(error) => codec(error, budget),
        E::ProtocolViolation(error) => violation(error),
        E::InvalidAlias { alias, max } => Details::new(D::Protocol, C::AliasOutOfRange).measured(
            Q::TopicAlias,
            Some(u64::from(*alias)),
            Some(u64::from(*max)),
            None,
        ),
        E::QoSNotSupported { requested, maximum } => Details::new(
            D::Protocol,
            C::CapabilityUnsupported,
        )
        .measured(Q::QoS, Some(*requested as u64), Some(*maximum as u64), None),
        E::RetainNotSupported
        | E::WildcardSubscriptionNotSupported
        | E::SubscriptionIdentifiersNotSupported
        | E::SharedSubscriptionNotSupported => Details::new(D::Protocol, C::CapabilityUnsupported),
        E::OutgoingPacketTooLarge { pkt_size, max } => {
            Details::new(D::Protocol, C::OutgoingTooLarge).measured(
                Q::PacketBytes,
                Some(u64::from(*pkt_size)),
                Some(u64::from(*max)),
                None,
            )
        }
        E::IncomingPacketTooLarge { pkt_size, max } => {
            Details::new(D::Protocol, C::IncomingTooLarge).measured(
                Q::PacketBytes,
                Some(*pkt_size as u64),
                Some(*max as u64),
                None,
            )
        }
        E::ServerDisconnect { .. } => Details::new(D::Connection, C::BrokerDisconnect),
        E::ConnFail { .. } => Details::new(D::Connection, C::BrokerRefused),
        E::ServerRedirect(_) => Details::new(D::Connection, C::Redirect),
        E::AuthError(_) | E::AuthenticatorNotSet | E::AuthenticatorLockPoisoned => {
            Details::new(D::Protocol, C::Authentication)
        }
        E::Coversion(_) => Details::new(D::Protocol, C::InvalidRequest),
        _ => Details::new(D::Protocol, C::Unknown),
    }
}

const fn violation(error: &native::ProtocolViolation) -> Details {
    use native::ProtocolViolation as E;
    match error {
        E::DuplicateConnAck => {
            let mut details = Details::new(D::Protocol, C::DuplicateConnAck);
            details.packet_kind = Some(2);
            details
        }
        E::UnexpectedIncomingPacket(kind) => {
            let mut details = Details::new(D::Protocol, C::UnexpectedPacket);
            details.packet_kind = Some(packet_kind(*kind));
            details
        }
        E::SubAckReturnCodeCountMismatch {
            pkid,
            expected,
            actual,
        } => count_mismatch(9, *pkid, *actual, *expected),
        E::UnsubAckReasonCodeCountMismatch {
            pkid,
            expected,
            actual,
        } => count_mismatch(11, *pkid, *actual, *expected),
        _ => Details::new(D::Protocol, C::Unknown),
    }
}

const fn count_mismatch(kind: u8, id: u16, actual: usize, expected: usize) -> Details {
    let mut details = Details::new(D::Protocol, C::AckCountMismatch).measured(
        Q::Items,
        Some(actual as u64),
        None,
        Some(expected as u64),
    );
    details.packet_kind = Some(kind);
    details.packet_id = Some(id);
    details
}

fn codec(error: &native::mqttbytes::Error, budget: &mut usize) -> Details {
    use native::mqttbytes::Error as E;
    match error {
        E::Io(error) => io(error, budget),
        E::OutgoingPacketTooLarge { pkt_size, max } => {
            Details::new(D::Protocol, C::OutgoingTooLarge).measured(
                Q::PacketBytes,
                Some(u64::from(*pkt_size)),
                Some(u64::from(*max)),
                None,
            )
        }
        E::PayloadSizeLimitExceeded { pkt_size, max } => {
            Details::new(D::Protocol, C::IncomingTooLarge).measured(
                Q::PacketBytes,
                Some(*pkt_size as u64),
                Some(u64::from(*max)),
                None,
            )
        }
        E::InsufficientBytes(size) => Details::new(D::Protocol, C::MalformedPacket).measured(
            Q::AdditionalBytes,
            Some(*size as u64),
            None,
            None,
        ),
        E::EmptySubscription | E::EmptyUnsubscription => {
            Details::new(D::Protocol, C::InvalidRequest)
        }

        E::InvalidReason(_)
        | E::InvalidRemainingLength(_)
        | E::InvalidRetainForwardRule(_)
        | E::ProtocolError
        | E::ProtocolViolation(_)
        | E::PayloadNotUtf8(_)
        | E::InvalidConnectReturnCode(_)
        | E::InvalidProtocol
        | E::InvalidProtocolLevel(_)
        | E::IncorrectPacketFormat
        | E::InvalidPacketType(_)
        | E::InvalidPropertyType(_)
        | E::InvalidQoS(_)
        | E::InvalidSubscribeReasonCode(_)
        | E::PacketIdZero
        | E::PayloadSizeIncorrect
        | E::PayloadTooLong
        | E::PayloadRequired
        | E::TopicNotUtf8 { .. }
        | E::BoundaryCrossed(_)
        | E::MalformedPacket
        | E::MalformedRemainingLength => Details::new(D::Protocol, C::MalformedPacket),
        _ => Details::new(D::Protocol, C::Unknown),
    }
}

const fn packet_kind(kind: native::mqttbytes::PacketType) -> u8 {
    use native::mqttbytes::PacketType as P;
    match kind {
        P::Connect => 1,
        P::ConnAck => 2,
        P::Publish => 3,
        P::PubAck => 4,
        P::PubRec => 5,
        P::PubRel => 6,
        P::PubComp => 7,
        P::Subscribe => 8,
        P::SubAck => 9,
        P::Unsubscribe => 10,
        P::UnsubAck => 11,
        P::PingReq => 12,
        P::PingResp => 13,
        P::Disconnect => 14,
        P::Auth => 15,
    }
}
