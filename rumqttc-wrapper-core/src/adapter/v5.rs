use crate::{DeliveryStatus, Error, ErrorKind, OutgoingActivity};

pub(crate) fn map_client_error(error: rumqttc_v5::ClientError) -> Error {
    let kind = match error {
        rumqttc_v5::ClientError::RequestChannelFull(_)
        | rumqttc_v5::ClientError::PublishAdmissionPending { .. } => ErrorKind::Backpressure,
        rumqttc_v5::ClientError::RequestChannelDisconnected(_) => ErrorKind::Shutdown,
        _ => ErrorKind::Admission,
    };
    Error::sourced(kind, DeliveryStatus::NotAdmitted, error)
}

pub(crate) fn map_connection_error(error: rumqttc_v5::ConnectionError) -> Error {
    let kind = match error {
        rumqttc_v5::ConnectionError::Tls(_) => ErrorKind::Tls,
        rumqttc_v5::ConnectionError::ConnectionRefused(
            rumqttc_v5::ConnectReturnCode::BadUserNamePassword
            | rumqttc_v5::ConnectReturnCode::NotAuthorized
            | rumqttc_v5::ConnectReturnCode::BadAuthenticationMethod,
        ) => ErrorKind::Authentication,
        rumqttc_v5::ConnectionError::SessionStore(_)
        | rumqttc_v5::ConnectionError::SessionRestore(_) => ErrorKind::Persistence,
        rumqttc_v5::ConnectionError::Timeout(_)
        | rumqttc_v5::ConnectionError::DisconnectTimeout => ErrorKind::Timeout,
        rumqttc_v5::ConnectionError::Io(_)
        | rumqttc_v5::ConnectionError::Websocket(_)
        | rumqttc_v5::ConnectionError::WsConnect(_) => ErrorKind::Network,
        _ => ErrorKind::Protocol,
    };
    Error::sourced(kind, DeliveryStatus::Ambiguous, error)
}

pub(crate) const fn map_outgoing(outgoing: &rumqttc_v5::Outgoing) -> OutgoingActivity {
    match outgoing {
        rumqttc_v5::Outgoing::Publish(_) => OutgoingActivity::Publish,
        rumqttc_v5::Outgoing::Subscribe(_) => OutgoingActivity::Subscribe,
        rumqttc_v5::Outgoing::Unsubscribe(_) => OutgoingActivity::Unsubscribe,
        rumqttc_v5::Outgoing::PubAck(_)
        | rumqttc_v5::Outgoing::PubRec(_)
        | rumqttc_v5::Outgoing::PubRel(_)
        | rumqttc_v5::Outgoing::PubComp(_) => OutgoingActivity::Acknowledgement,
        rumqttc_v5::Outgoing::PingReq | rumqttc_v5::Outgoing::PingResp => OutgoingActivity::Ping,
        rumqttc_v5::Outgoing::Disconnect => OutgoingActivity::Disconnect,
        rumqttc_v5::Outgoing::AwaitAck(_) => OutgoingActivity::AwaitAcknowledgement,
        rumqttc_v5::Outgoing::Auth => OutgoingActivity::Other,
    }
}
