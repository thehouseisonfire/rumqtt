use crate::{
    BrokerReason, Completion, DeliveryStatus, Error, ErrorKind, OutgoingActivity, PublishCommand,
    PublishCompletion, PublishProtocolOptions, QoS, Result, SubscribeCommand, SubscribeCompletion,
    SubscribeProtocolOptions, SubscribeResult, UnsubscribeCommand, UnsubscribeCompletion,
    UnsubscribeProtocolOptions, UnsubscribeResult, V5IncomingPublishProperties,
    V5OutgoingPublishProperties, V5RetainForwardRule, V5SubscribeProperties,
    V5UnsubscribeProperties,
};

use crate::validation::{protocol_option_error, validate_mqtt_utf8_string};

pub(crate) fn validate_publish(command: &PublishCommand) -> Result<()> {
    let PublishProtocolOptions::V5(properties) = &command.protocol else {
        return Ok(());
    };
    match properties.payload_format_indicator {
        None | Some(0) => {}
        Some(1) if std::str::from_utf8(&command.payload).is_ok() => {}
        Some(_) => {
            return Err(protocol_option_error(
                "invalid payload format indicator or payload",
            ));
        }
    }
    if properties.topic_alias == Some(0) {
        return Err(protocol_option_error(
            "topic alias must be greater than zero",
        ));
    }
    if let Some(response_topic) = &properties.response_topic {
        validate_mqtt_utf8_string(response_topic, "response topic")?;
        if response_topic.is_empty() || response_topic.contains(['+', '#']) {
            return Err(protocol_option_error(
                "response topic must be a nonempty publish topic without wildcards",
            ));
        }
    }
    if properties
        .correlation_data
        .as_ref()
        .is_some_and(|data| data.len() > usize::from(u16::MAX))
    {
        return Err(protocol_option_error(
            "correlation data exceeds the MQTT binary-data limit",
        ));
    }
    validate_user_properties(&properties.user_properties, "PUBLISH user property")?;
    if let Some(content_type) = &properties.content_type {
        validate_mqtt_utf8_string(content_type, "content type")?;
    }
    Ok(())
}

pub(crate) fn validate_subscribe(command: &SubscribeCommand) -> Result<()> {
    let SubscribeProtocolOptions::V5(properties) = &command.protocol else {
        return Ok(());
    };
    if properties.subscription_identifier == Some(0)
        || properties
            .subscription_identifier
            .is_some_and(|identifier| identifier > 268_435_455)
    {
        return Err(protocol_option_error(
            "subscription identifier must be between 1 and 268435455",
        ));
    }
    validate_user_properties(&properties.user_properties, "SUBSCRIBE user property")
}

pub(crate) fn validate_unsubscribe(command: &UnsubscribeCommand) -> Result<()> {
    let UnsubscribeProtocolOptions::V5(properties) = &command.protocol else {
        return Ok(());
    };
    validate_user_properties(&properties.user_properties, "UNSUBSCRIBE user property")
}

fn validate_user_properties(properties: &[(String, String)], name: &str) -> Result<()> {
    for (key, value) in properties {
        validate_mqtt_utf8_string(key, &format!("{name} key"))?;
        validate_mqtt_utf8_string(value, &format!("{name} value"))?;
    }
    Ok(())
}

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

use std::collections::HashMap;

use futures_util::stream::{FuturesUnordered, StreamExt};

use crate::handle::Shared;
use crate::operations::{
    PendingFuture, PendingSender, accept_registration, fail_pending, resolve_pending,
};
use crate::runtime::{
    DriverContext, EventDelivery, ShutdownInputs, TerminalStatus, complete_shutdown, deliver,
    finish_close, overflow_error,
};
use crate::shutdown::PollErrorAction;
use crate::{
    ConnectionPhase, DiagnosticsSnapshot, IncomingPublish, OperationId, ProtocolVersion,
    WrapperEvent,
};

pub(crate) fn build(
    common: &crate::CommonConfig,
    protocol: crate::V5Config,
) -> crate::Result<(rumqttc_v5::AsyncClient, Box<rumqttc_v5::EventLoop>)> {
    let tls = match &common.transport {
        crate::TransportConfig::Tls(tls) | crate::TransportConfig::Wss { tls, .. } => {
            Some(super::build_tls(tls)?)
        }
        _ => None,
    };
    let mut options = match &common.transport {
        crate::TransportConfig::Tcp | crate::TransportConfig::Tls(_) => {
            rumqttc_v5::MqttOptions::new(
                common.client_id.clone(),
                rumqttc_v5::Broker::tcp(common.broker_host.clone(), common.broker_port),
            )
        }
        crate::TransportConfig::WebSocket { url } => rumqttc_v5::MqttOptions::new(
            common.client_id.clone(),
            rumqttc_v5::Broker::websocket(url.clone()).map_err(|error| {
                Error::sourced(
                    ErrorKind::Configuration,
                    DeliveryStatus::NotApplicable,
                    error,
                )
            })?,
        ),
        crate::TransportConfig::Wss { url, .. } => {
            rumqttc_v5::MqttOptions::websocket_with_tls_config(
                common.client_id.clone(),
                url.clone(),
                tls.clone().expect("WSS TLS built"),
            )
            .map_err(|error| {
                Error::sourced(
                    ErrorKind::Configuration,
                    DeliveryStatus::NotApplicable,
                    error,
                )
            })?
        }
    };
    if matches!(common.transport, crate::TransportConfig::Tls(_)) {
        options.set_transport(rumqttc_v5::Transport::tls_with_config(
            tls.expect("TLS built"),
        ));
    }
    options.set_keep_alive(crate::handle::duration_to_u16(
        common.keep_alive,
        "keep alive",
    )?);
    options.set_incoming_packet_size_limit(rumqttc_v5::IncomingPacketSizeLimit::Bytes(
        common.incoming_packet_size_limit,
    ));
    options.set_request_channel_capacity(common.request_channel_capacity);
    options.set_ack_mode(match common.ack_mode {
        crate::AckMode::Automatic => rumqttc_v5::AckMode::Automatic,
        crate::AckMode::Manual => rumqttc_v5::AckMode::Manual,
    });
    let mut network = rumqttc_v5::NetworkOptions::new();
    network.set_connection_timeout(common.connection_timeout.as_secs());
    options.set_network_options(network);
    match (&common.username, &common.password) {
        (Some(username), Some(password)) => {
            options.set_credentials(username.clone(), password.clone());
        }
        (Some(username), None) => {
            options.set_username(username.clone());
        }
        (None, Some(password)) => {
            options.set_password(password.clone());
        }
        (None, None) => {}
    }
    options.set_clean_start(protocol.clean_start);
    options.set_session_expiry_interval(protocol.session_expiry_interval);
    options.validate().map_err(|error| {
        Error::sourced(
            ErrorKind::Configuration,
            DeliveryStatus::NotApplicable,
            error,
        )
    })?;
    let (client, eventloop) = rumqttc_v5::AsyncClient::builder(options)
        .capacity(common.request_channel_capacity)
        .publish_admission_policy(rumqttc_v5::PublishAdmissionPolicy::RequireNegotiatedCapabilities)
        .try_build()
        .map_err(|error| {
            Error::sourced(
                ErrorKind::Configuration,
                DeliveryStatus::NotApplicable,
                error,
            )
        })?;
    Ok((client, Box::new(eventloop)))
}
pub(crate) async fn run(
    mut eventloop: Box<rumqttc_v5::EventLoop>,
    context: DriverContext,
) -> TerminalStatus {
    let DriverContext {
        shared,
        completion_rx,
        diagnostics_rx,
        events,
        delivery_timeout,
        emit_outgoing,
        manual_ack,
        protocol,
        immediate_shutdown_rx,
    } = context;
    let mut pending = FuturesUnordered::<PendingFuture>::new();
    let mut senders = HashMap::<OperationId, PendingSender>::new();
    let mut connected = false;
    let mut diagnostics = snapshot_v5(&eventloop);
    let shutdown = ShutdownInputs::new(&shared, &completion_rx, &diagnostics_rx);
    let delivery = EventDelivery {
        shared: &shared,
        events: &events,
        timeout: delivery_timeout,
        immediate_shutdown: &immediate_shutdown_rx,
    };
    loop {
        // See the v4 loop: polling is an indivisible ownership boundary even while wrapper
        // registrations, cached diagnostics, and completed notices remain responsive.
        let polled = {
            let poll = eventloop.poll();
            tokio::pin!(poll);
            loop {
                // Keep parity with the fair and cooperative v4 arbitration above.
                tokio::select! {
                    _ = immediate_shutdown_rx.recv_async(), if !connected => break None,
                    registration = completion_rx.recv_async() => if let Ok(registration) = registration {
                        accept_registration(registration, &pending, &mut senders);
                        tokio::task::yield_now().await;
                    },
                    request = diagnostics_rx.recv_async() => if let Ok(request) = request {
                        request.resolve(diagnostics.clone());
                        tokio::task::yield_now().await;
                    },
                    result = pending.next(), if !pending.is_empty() => if let Some(result) = result {
                        resolve_pending(result, &mut senders);
                        tokio::task::yield_now().await;
                    },
                    result = &mut poll => break Some(result),
                }
            }
        };
        let Some(polled) = polled else {
            // Keep MQTT 5 connection-establishment cancellation identical to the v4 path.
            return finish_close(&shutdown, &diagnostics, &mut pending, &mut senders).await;
        };
        shared.notify_progress();
        diagnostics = snapshot_v5(&eventloop);
        match polled {
            Ok(event) => {
                if let Some(event) = map_v5_event(
                    &mut eventloop,
                    event,
                    &shared,
                    &mut connected,
                    emit_outgoing,
                    manual_ack,
                    protocol,
                ) && !deliver(&delivery, event).await
                {
                    let error = overflow_error();
                    shared.fail_acknowledgements(&error);
                    fail_pending(&mut senders, &error);
                    return TerminalStatus::Failed(error);
                }
            }
            Err(rumqttc_v5::ConnectionError::RequestsDone) => {
                let graceful =
                    complete_shutdown(&shutdown, &diagnostics, &mut pending, &mut senders).await;
                return TerminalStatus::Closed { graceful };
            }
            Err(error) => {
                let error = map_connection_error(error);
                match shared.poll_error_action() {
                    PollErrorAction::CompleteImmediateClose => {
                        return finish_close(&shutdown, &diagnostics, &mut pending, &mut senders)
                            .await;
                    }
                    PollErrorAction::Fail => {
                        shared.fail_acknowledgements(&error);
                        fail_pending(&mut senders, &error);
                        return TerminalStatus::Failed(error);
                    }
                    PollErrorAction::Reconnect => {}
                }
                let phase = if connected {
                    ConnectionPhase::Established
                } else {
                    ConnectionPhase::Attempt
                };
                connected = false;
                shared.invalidate_connection(&error);
                if !deliver(&delivery, WrapperEvent::Disconnected { phase, error }).await {
                    let error = overflow_error();
                    shared.fail_acknowledgements(&error);
                    fail_pending(&mut senders, &error);
                    return TerminalStatus::Failed(error);
                }
            }
        }
    }
}

fn map_v5_event(
    eventloop: &mut rumqttc_v5::EventLoop,
    event: rumqttc_v5::Event,
    shared: &Shared,
    connected: &mut bool,
    emit_outgoing: bool,
    manual_ack: bool,
    protocol: ProtocolVersion,
) -> Option<WrapperEvent> {
    match event {
        rumqttc_v5::Event::Incoming(rumqttc_v5::Packet::ConnAck(connack)) => {
            shared.begin_connection(protocol, connack.session_present, || {
                eventloop.discard_pending_manual_acknowledgements();
            });
            *connected = true;
            Some(WrapperEvent::Connected {
                protocol,
                session_present: connack.session_present,
            })
        }
        rumqttc_v5::Event::Incoming(rumqttc_v5::Packet::Publish(publish)) => {
            let ack_token = if manual_ack {
                match publish.qos {
                    rumqttc_v5::QoS::AtLeastOnce | rumqttc_v5::QoS::ExactlyOnce => shared
                        .backend()
                        .prepare_v5_ack(&publish)
                        .and_then(|ack| shared.prepare_ack(ack)),
                    _ => None,
                }
            } else {
                None
            };
            Some(WrapperEvent::IncomingPublish(Box::new(IncomingPublish {
                topic: publish.topic,
                payload: publish.payload,
                qos: from_qos(publish.qos),
                retain: publish.retain,
                duplicate: publish.dup,
                ack_token,
                v5_properties: publish.properties.map(from_incoming_publish_properties),
            })))
        }
        rumqttc_v5::Event::Outgoing(outgoing) => {
            match outgoing {
                rumqttc_v5::Outgoing::PubAck(packet_id) => {
                    shared.complete_v5_puback(packet_id);
                }
                rumqttc_v5::Outgoing::PubRec(packet_id) => {
                    shared.complete_v5_pubrec(packet_id);
                }
                _ => {}
            }
            emit_outgoing.then(|| WrapperEvent::Outgoing(map_outgoing(&outgoing)))
        }
        _ => None,
    }
}

fn snapshot_v5(eventloop: &rumqttc_v5::EventLoop) -> DiagnosticsSnapshot {
    let diagnostics = eventloop.diagnostics();
    DiagnosticsSnapshot {
        connected: diagnostics.connected,
        disconnecting: diagnostics.disconnecting,
        pending_requests: diagnostics.queues.pending_len,
        queued_requests: diagnostics.queues.requests_rx_len
            + diagnostics.queues.control_requests_rx_len,
        inflight_publishes: diagnostics.outbound.inflight,
        max_inflight_publishes: diagnostics.outbound.max_inflight,
        pending_subscribes: diagnostics.outbound.pending_subscribe,
        pending_unsubscribes: diagnostics.outbound.pending_unsubscribe,
        outbound_drained: diagnostics.outbound.outbound_drained,
    }
}

pub(crate) fn publish_options(command: &PublishCommand) -> rumqttc_v5::PublishOptions {
    let options = rumqttc_v5::PublishOptions::new(to_qos(command.qos)).retain(command.retain);
    match command.protocol.clone() {
        PublishProtocolOptions::VersionNeutral => options,
        PublishProtocolOptions::V5(properties) => {
            options.properties(to_outgoing_publish_properties(properties))
        }
    }
}

pub(crate) const fn to_retain_forward_rule(
    rule: V5RetainForwardRule,
) -> rumqttc_v5::RetainForwardRule {
    match rule {
        V5RetainForwardRule::OnEverySubscribe => rumqttc_v5::RetainForwardRule::OnEverySubscribe,
        V5RetainForwardRule::OnNewSubscribe => rumqttc_v5::RetainForwardRule::OnNewSubscribe,
        V5RetainForwardRule::Never => rumqttc_v5::RetainForwardRule::Never,
    }
}

pub(crate) fn to_subscribe_properties(
    properties: V5SubscribeProperties,
) -> rumqttc_v5::SubscribeProperties {
    rumqttc_v5::SubscribeProperties {
        id: properties.subscription_identifier,
        user_properties: properties.user_properties,
    }
}

pub(crate) fn to_unsubscribe_properties(
    properties: V5UnsubscribeProperties,
) -> rumqttc_v5::UnsubscribeProperties {
    rumqttc_v5::UnsubscribeProperties {
        user_properties: properties.user_properties,
    }
}

pub(crate) const fn to_qos(qos: QoS) -> rumqttc_v5::QoS {
    match qos {
        QoS::AtMostOnce => rumqttc_v5::QoS::AtMostOnce,
        QoS::AtLeastOnce => rumqttc_v5::QoS::AtLeastOnce,
        QoS::ExactlyOnce => rumqttc_v5::QoS::ExactlyOnce,
    }
}

pub(crate) const fn from_qos(qos: rumqttc_v5::QoS) -> QoS {
    match qos {
        rumqttc_v5::QoS::AtMostOnce => QoS::AtMostOnce,
        rumqttc_v5::QoS::AtLeastOnce => QoS::AtLeastOnce,
        rumqttc_v5::QoS::ExactlyOnce => QoS::ExactlyOnce,
    }
}

pub(crate) fn to_outgoing_publish_properties(
    properties: V5OutgoingPublishProperties,
) -> rumqttc_v5::PublishProperties {
    rumqttc_v5::PublishProperties {
        payload_format_indicator: properties.payload_format_indicator,
        message_expiry_interval: properties.message_expiry_interval,
        topic_alias: properties.topic_alias,
        response_topic: properties.response_topic,
        correlation_data: properties.correlation_data,
        user_properties: properties.user_properties,
        subscription_identifiers: Vec::new(),
        content_type: properties.content_type,
    }
}

pub(crate) fn from_incoming_publish_properties(
    properties: rumqttc_v5::PublishProperties,
) -> V5IncomingPublishProperties {
    V5IncomingPublishProperties {
        response_topic: properties.response_topic,
        correlation_data: properties.correlation_data,
        content_type: properties.content_type,
        payload_format_indicator: properties.payload_format_indicator,
        topic_alias: properties.topic_alias,
        subscription_identifiers: properties.subscription_identifiers,
        message_expiry_interval: properties.message_expiry_interval,
        user_properties: properties.user_properties,
    }
}

pub(crate) fn map_publish_notice(
    result: std::result::Result<rumqttc_v5::PublishResult, rumqttc_v5::PublishNoticeError>,
) -> Result<Completion> {
    match result {
        Ok(rumqttc_v5::PublishResult::Qos0Flushed) => {
            Ok(Completion::Publish(PublishCompletion::Qos0Flushed))
        }
        Ok(rumqttc_v5::PublishResult::Qos1(ack)) if v5_puback_success(ack.reason) => {
            Ok(Completion::Publish(PublishCompletion::Qos1Acknowledged))
        }
        Ok(rumqttc_v5::PublishResult::Qos2Completed(ack)) if v5_pubcomp_success(ack.reason) => {
            Ok(Completion::Publish(PublishCompletion::Qos2Completed))
        }
        Ok(rumqttc_v5::PublishResult::Qos2Recovered(_)) => {
            Ok(Completion::Publish(PublishCompletion::Qos2Completed))
        }
        Ok(rumqttc_v5::PublishResult::Qos1(ack)) => {
            Err(broker_rejection(v5_puback_code(ack.reason)))
        }
        Ok(rumqttc_v5::PublishResult::Qos2Completed(ack)) => {
            Err(broker_rejection(v5_pubcomp_code(ack.reason)))
        }
        Ok(rumqttc_v5::PublishResult::Qos2PubRecRejected(ack)) => {
            Err(broker_rejection(v5_pubrec_code(ack.reason)))
        }
        Err(error) => Err(map_notice_error(error)),
    }
}

pub(crate) fn map_subscribe_notice(
    result: std::result::Result<rumqttc_v5::SubAck, rumqttc_v5::SubscribeNoticeError>,
) -> Result<Completion> {
    result
        .map(|ack| {
            Completion::Subscribe(SubscribeCompletion {
                results: ack
                    .return_codes
                    .into_iter()
                    .map(|reason| match reason {
                        rumqttc_v5::SubscribeReasonCode::Success(qos) => {
                            SubscribeResult::Granted(from_qos(qos))
                        }
                        reason => SubscribeResult::Rejected(BrokerReason {
                            code: v5_suback_code(reason),
                        }),
                    })
                    .collect(),
            })
        })
        .map_err(map_notice_error)
}

pub(crate) fn map_unsubscribe_notice(
    result: std::result::Result<rumqttc_v5::UnsubAck, rumqttc_v5::UnsubscribeNoticeError>,
) -> Result<Completion> {
    result
        .map(|ack| {
            Completion::Unsubscribe(UnsubscribeCompletion {
                results: Some(
                    ack.reasons
                        .into_iter()
                        .map(|reason| match reason {
                            rumqttc_v5::UnsubAckReason::Success => UnsubscribeResult::Success,
                            rumqttc_v5::UnsubAckReason::NoSubscriptionExisted => {
                                UnsubscribeResult::NoSubscriptionExisted
                            }
                            reason => {
                                UnsubscribeResult::Rejected(BrokerReason { code: reason as u8 })
                            }
                        })
                        .collect(),
                ),
            })
        })
        .map_err(map_notice_error)
}

pub(crate) const fn v5_suback_code(reason: rumqttc_v5::SubscribeReasonCode) -> u8 {
    use rumqttc_v5::SubscribeReasonCode as R;
    match reason {
        R::Success(qos) => from_qos(qos) as u8,
        R::Failure | R::Unspecified => 0x80,
        R::ImplementationSpecific => 0x83,
        R::NotAuthorized => 0x87,
        R::TopicFilterInvalid => 0x8f,
        R::PkidInUse => 0x91,
        R::QuotaExceeded => 0x97,
        R::SharedSubscriptionsNotSupported => 0x9e,
        R::SubscriptionIdNotSupported => 0xa1,
        R::WildcardSubscriptionsNotSupported => 0xa2,
    }
}

pub(crate) const fn v5_puback_success(reason: rumqttc_v5::PubAckReason) -> bool {
    matches!(
        reason,
        rumqttc_v5::PubAckReason::Success | rumqttc_v5::PubAckReason::NoMatchingSubscribers
    )
}

pub(crate) const fn v5_puback_code(reason: rumqttc_v5::PubAckReason) -> u8 {
    reason as u8
}

pub(crate) const fn v5_pubrec_code(reason: rumqttc_v5::PubRecReason) -> u8 {
    reason as u8
}

pub(crate) const fn v5_pubcomp_code(reason: rumqttc_v5::PubCompReason) -> u8 {
    reason as u8
}

pub(crate) fn v5_pubcomp_success(reason: rumqttc_v5::PubCompReason) -> bool {
    reason == rumqttc_v5::PubCompReason::Success
}

pub(crate) fn map_notice_error<E: std::error::Error + Send + Sync + 'static>(error: E) -> Error {
    Error::sourced(ErrorKind::Protocol, DeliveryStatus::Ambiguous, error)
}

pub(crate) fn broker_rejection(code: u8) -> Error {
    Error::new(
        ErrorKind::Protocol,
        format!("broker rejected operation with reason code 0x{code:02x}"),
    )
    .with_delivery(DeliveryStatus::Rejected)
    .with_broker_reason(code)
}
