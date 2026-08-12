use crate::{DeliveryStatus, Error, ErrorKind, OutgoingActivity};

pub(crate) fn map_client_error(error: rumqttc_v4::ClientError) -> Error {
    let kind = match error {
        rumqttc_v4::ClientError::RequestChannelFull(_) => ErrorKind::Backpressure,
        rumqttc_v4::ClientError::RequestChannelDisconnected(_) => ErrorKind::Shutdown,
        _ => ErrorKind::Admission,
    };
    Error::sourced(kind, DeliveryStatus::NotAdmitted, error)
}

pub(crate) fn map_connection_error(error: rumqttc_v4::ConnectionError) -> Error {
    let kind = match error {
        rumqttc_v4::ConnectionError::Tls(_) => ErrorKind::Tls,
        rumqttc_v4::ConnectionError::ConnectionRefused(
            rumqttc_v4::ConnectReturnCode::BadUserNamePassword
            | rumqttc_v4::ConnectReturnCode::NotAuthorized,
        ) => ErrorKind::Authentication,
        rumqttc_v4::ConnectionError::SessionStore(_)
        | rumqttc_v4::ConnectionError::SessionRestore(_) => ErrorKind::Persistence,
        rumqttc_v4::ConnectionError::NetworkTimeout
        | rumqttc_v4::ConnectionError::FlushTimeout
        | rumqttc_v4::ConnectionError::DisconnectTimeout => ErrorKind::Timeout,
        rumqttc_v4::ConnectionError::Io(_)
        | rumqttc_v4::ConnectionError::Websocket(_)
        | rumqttc_v4::ConnectionError::WsConnect(_) => ErrorKind::Network,
        _ => ErrorKind::Protocol,
    };
    Error::sourced(kind, DeliveryStatus::Ambiguous, error)
}

pub(crate) const fn map_outgoing(outgoing: &rumqttc_v4::Outgoing) -> OutgoingActivity {
    match outgoing {
        rumqttc_v4::Outgoing::Publish(_) => OutgoingActivity::Publish,
        rumqttc_v4::Outgoing::Subscribe(_) => OutgoingActivity::Subscribe,
        rumqttc_v4::Outgoing::Unsubscribe(_) => OutgoingActivity::Unsubscribe,
        rumqttc_v4::Outgoing::PubAck(_)
        | rumqttc_v4::Outgoing::PubRec(_)
        | rumqttc_v4::Outgoing::PubRel(_)
        | rumqttc_v4::Outgoing::PubComp(_) => OutgoingActivity::Acknowledgement,
        rumqttc_v4::Outgoing::PingReq | rumqttc_v4::Outgoing::PingResp => OutgoingActivity::Ping,
        rumqttc_v4::Outgoing::Disconnect => OutgoingActivity::Disconnect,
        rumqttc_v4::Outgoing::AwaitAck(_) => OutgoingActivity::AwaitAcknowledgement,
    }
}
