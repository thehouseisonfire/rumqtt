use crate::{
    BrokerReason, Completion, DeliveryStatus, Error, ErrorKind, OutgoingActivity, PublishCommand,
    PublishCompletion, QoS, Result, SubscribeCompletion, SubscribeResult, UnsubscribeCompletion,
};

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
    protocol: crate::V4Config,
) -> crate::Result<(rumqttc_v4::AsyncClient, Box<rumqttc_v4::EventLoop>)> {
    let tls = match &common.transport {
        crate::TransportConfig::Tls(tls) | crate::TransportConfig::Wss { tls, .. } => {
            Some(super::build_tls(tls)?)
        }
        _ => None,
    };
    let mut options = match &common.transport {
        crate::TransportConfig::Tcp | crate::TransportConfig::Tls(_) => {
            rumqttc_v4::MqttOptions::new(
                common.client_id.clone(),
                rumqttc_v4::Broker::tcp(common.broker_host.clone(), common.broker_port),
            )
        }
        crate::TransportConfig::WebSocket { url } => rumqttc_v4::MqttOptions::new(
            common.client_id.clone(),
            rumqttc_v4::Broker::websocket(url.clone()).map_err(|error| {
                Error::sourced(
                    ErrorKind::Configuration,
                    DeliveryStatus::NotApplicable,
                    error,
                )
            })?,
        ),
        crate::TransportConfig::Wss { url, .. } => {
            rumqttc_v4::MqttOptions::websocket_with_tls_config(
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
        options.set_transport(rumqttc_v4::Transport::tls_with_config(
            tls.expect("TLS built"),
        ));
    }
    options.set_keep_alive(crate::handle::duration_to_u16(
        common.keep_alive,
        "keep alive",
    )?);
    options.set_max_packet_size(common.incoming_packet_size_limit as usize, usize::MAX);
    options.set_request_channel_capacity(common.request_channel_capacity);
    options.set_ack_mode(match common.ack_mode {
        crate::AckMode::Automatic => rumqttc_v4::AckMode::Automatic,
        crate::AckMode::Manual => rumqttc_v4::AckMode::Manual,
    });
    match (&common.username, &common.password) {
        (Some(username), Some(password)) => {
            options.set_credentials(username.clone(), password.clone());
        }
        (Some(username), None) => {
            options.set_username(username.clone());
        }
        (None, None | Some(_)) => {}
    }
    options
        .try_set_clean_session(protocol.clean_session)
        .map_err(|error| {
            Error::sourced(
                ErrorKind::Configuration,
                DeliveryStatus::NotApplicable,
                error,
            )
        })?;
    options.validate().map_err(|error| {
        Error::sourced(
            ErrorKind::Configuration,
            DeliveryStatus::NotApplicable,
            error,
        )
    })?;
    let (client, mut eventloop) = rumqttc_v4::AsyncClient::builder(options)
        .capacity(common.request_channel_capacity)
        .try_build()
        .map_err(|error| {
            Error::sourced(
                ErrorKind::Configuration,
                DeliveryStatus::NotApplicable,
                error,
            )
        })?;
    let mut network = rumqttc_v4::NetworkOptions::new();
    network.set_connection_timeout(common.connection_timeout.as_secs());
    eventloop.network_options = network;
    Ok((client, Box::new(eventloop)))
}
pub(crate) async fn run(
    mut eventloop: Box<rumqttc_v4::EventLoop>,
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
    let mut diagnostics = snapshot_v4(&eventloop);
    let shutdown = ShutdownInputs::new(&shared, &completion_rx, &diagnostics_rx);
    let delivery = EventDelivery {
        shared: &shared,
        events: &events,
        timeout: delivery_timeout,
        immediate_shutdown: &immediate_shutdown_rx,
    };
    loop {
        // `EventLoop::poll` can dequeue requests and mutate protocol state before awaiting I/O.
        // Keep the same future alive across wrapper-control wakeups so those side effects cannot
        // be abandoned by `select!` cancellation. Diagnostics use the last completed snapshot
        // while the poll future holds the mutable event-loop borrow.
        let polled = {
            let poll = eventloop.poll();
            tokio::pin!(poll);
            loop {
                // Fair selection arbitrates among ready branches. Yield after synchronously
                // handled wrapper work as well so a continuously ready flume channel cannot keep
                // this current-thread runtime from driving the MQTT socket I/O reactor.
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
            // There is no established MQTT session to close cleanly. Dropping the event loop is
            // the cancellation boundary for DNS/TCP/TLS/CONNACK work; unlike resuming a cancelled
            // poll, termination cannot lose a dequeued request and then continue with corrupt state.
            return finish_close(&shutdown, &diagnostics, &mut pending, &mut senders).await;
        };
        shared.notify_progress();
        diagnostics = snapshot_v4(&eventloop);
        match polled {
            Ok(event) => {
                if let Some(event) = map_v4_event(
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
            Err(rumqttc_v4::ConnectionError::RequestsDone) => {
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

fn map_v4_event(
    eventloop: &mut rumqttc_v4::EventLoop,
    event: rumqttc_v4::Event,
    shared: &Shared,
    connected: &mut bool,
    emit_outgoing: bool,
    manual_ack: bool,
    protocol: ProtocolVersion,
) -> Option<WrapperEvent> {
    match event {
        rumqttc_v4::Event::Incoming(rumqttc_v4::Packet::ConnAck(connack)) => {
            shared.begin_connection(protocol, connack.session_present, || {
                eventloop.discard_pending_manual_acknowledgements();
            });
            *connected = true;
            Some(WrapperEvent::Connected {
                protocol,
                session_present: connack.session_present,
            })
        }
        rumqttc_v4::Event::Incoming(rumqttc_v4::Packet::Publish(publish)) => {
            let ack_token = if manual_ack {
                match publish.qos {
                    rumqttc_v4::QoS::AtLeastOnce | rumqttc_v4::QoS::ExactlyOnce => shared
                        .backend()
                        .prepare_v4_ack(&publish)
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
                v5_properties: None,
            })))
        }
        rumqttc_v4::Event::Outgoing(outgoing) => {
            match outgoing {
                rumqttc_v4::Outgoing::PubAck(packet_id) => {
                    shared.complete_v4_puback(packet_id);
                }
                rumqttc_v4::Outgoing::PubRec(packet_id) => {
                    shared.complete_v4_pubrec(packet_id);
                }
                _ => {}
            }
            emit_outgoing.then(|| WrapperEvent::Outgoing(map_outgoing(&outgoing)))
        }
        rumqttc_v4::Event::Incoming(_) => None,
    }
}

fn snapshot_v4(eventloop: &rumqttc_v4::EventLoop) -> DiagnosticsSnapshot {
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

pub(crate) const fn publish_options(command: &PublishCommand) -> rumqttc_v4::PublishOptions {
    rumqttc_v4::PublishOptions::new(to_qos(command.qos)).retain(command.retain)
}

pub(crate) const fn to_qos(qos: QoS) -> rumqttc_v4::QoS {
    match qos {
        QoS::AtMostOnce => rumqttc_v4::QoS::AtMostOnce,
        QoS::AtLeastOnce => rumqttc_v4::QoS::AtLeastOnce,
        QoS::ExactlyOnce => rumqttc_v4::QoS::ExactlyOnce,
    }
}

pub(crate) const fn from_qos(qos: rumqttc_v4::QoS) -> QoS {
    match qos {
        rumqttc_v4::QoS::AtMostOnce => QoS::AtMostOnce,
        rumqttc_v4::QoS::AtLeastOnce => QoS::AtLeastOnce,
        rumqttc_v4::QoS::ExactlyOnce => QoS::ExactlyOnce,
    }
}

pub(crate) fn map_publish_notice(
    result: std::result::Result<rumqttc_v4::PublishResult, rumqttc_v4::PublishNoticeError>,
) -> Result<Completion> {
    result
        .map(|result| {
            Completion::Publish(match result {
                rumqttc_v4::PublishResult::Qos0Flushed => PublishCompletion::Qos0Flushed,
                rumqttc_v4::PublishResult::Qos1(_) => PublishCompletion::Qos1Acknowledged,
                rumqttc_v4::PublishResult::Qos2Completed(_) => PublishCompletion::Qos2Completed,
            })
        })
        .map_err(map_notice_error)
}

pub(crate) fn map_subscribe_notice(
    result: std::result::Result<rumqttc_v4::SubAck, rumqttc_v4::SubscribeNoticeError>,
) -> Result<Completion> {
    result
        .map(|ack| {
            Completion::Subscribe(SubscribeCompletion {
                results: ack
                    .return_codes
                    .into_iter()
                    .map(|reason| match reason {
                        rumqttc_v4::SubscribeReasonCode::Success(qos) => {
                            SubscribeResult::Granted(from_qos(qos))
                        }
                        rumqttc_v4::SubscribeReasonCode::Failure => {
                            SubscribeResult::Rejected(BrokerReason { code: 0x80 })
                        }
                    })
                    .collect(),
            })
        })
        .map_err(map_notice_error)
}

pub(crate) fn map_unsubscribe_notice(
    result: std::result::Result<rumqttc_v4::UnsubAck, rumqttc_v4::UnsubscribeNoticeError>,
) -> Result<Completion> {
    result
        .map(|_| Completion::Unsubscribe(UnsubscribeCompletion { results: None }))
        .map_err(map_notice_error)
}

pub(crate) fn map_notice_error<E: std::error::Error + Send + Sync + 'static>(error: E) -> Error {
    Error::sourced(ErrorKind::Protocol, DeliveryStatus::Ambiguous, error)
}
