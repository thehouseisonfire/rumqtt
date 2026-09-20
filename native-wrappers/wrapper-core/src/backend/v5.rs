use crate::{
    BrokerReason, Completion, DeliveryStatus, Error, ErrorKind, OutgoingActivity, PublishCommand,
    PublishCompletion, PublishProtocolOptions, QoS, Result, SubscribeCommand, SubscribeCompletion,
    SubscribeProtocolOptions, SubscribeResult, UnsubscribeCommand, UnsubscribeCompletion,
    UnsubscribeProtocolOptions, UnsubscribeResult, V5IncomingPublishProperties,
    V5OutgoingPublishProperties, V5RetainForwardRule, V5SubscribeProperties,
    V5UnsubscribeProperties,
};

use crate::validation::{protocol_option_error, validate_mqtt_utf8_string};

pub fn validate_publish(command: &PublishCommand) -> Result<()> {
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

pub fn disconnect_properties(
    p: &crate::V5DisconnectOptions,
) -> Result<(
    rumqttc_v5::DisconnectReasonCode,
    rumqttc_v5::DisconnectProperties,
)> {
    let reason = rumqttc_v5::DisconnectReasonCode::try_from(p.reason_code)
        .map_err(|_| protocol_option_error("invalid MQTT 5 disconnect reason code"))?;
    if let Some(reason) = &p.reason_string {
        validate_mqtt_utf8_string(reason, "disconnect reason string")?;
    }
    if let Some(reference) = &p.server_reference {
        validate_mqtt_utf8_string(reference, "disconnect server reference")?;
    }
    for (key, value) in &p.user_properties {
        validate_mqtt_utf8_string(key, "disconnect user property key")?;
        validate_mqtt_utf8_string(value, "disconnect user property value")?;
    }
    Ok((
        reason,
        rumqttc_v5::DisconnectProperties {
            session_expiry_interval: p.session_expiry_interval,
            reason_string: p.reason_string.clone(),
            user_properties: p.user_properties.clone(),
            server_reference: p.server_reference.clone(),
        },
    ))
}

pub fn validate_subscribe(command: &SubscribeCommand) -> Result<()> {
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

pub fn validate_unsubscribe(command: &UnsubscribeCommand) -> Result<()> {
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

pub fn map_client_error(error: rumqttc_v5::ClientError) -> Error {
    let kind = match error {
        rumqttc_v5::ClientError::RequestChannelFull(_)
        | rumqttc_v5::ClientError::PublishAdmissionPending { .. } => ErrorKind::Backpressure,
        rumqttc_v5::ClientError::RequestChannelDisconnected(_) => ErrorKind::Shutdown,
        _ => ErrorKind::Admission,
    };
    // Client errors can own rejected requests, including payloads and AUTH data.
    Error::new(kind, "MQTT request admission failed").with_delivery(DeliveryStatus::NotAdmitted)
}

pub fn map_connection_error(error: rumqttc_v5::ConnectionError) -> Error {
    if let rumqttc_v5::ConnectionError::SessionStore(source) = &error {
        return Error::store(
            source
                .downcast_ref::<crate::StoreFailure>()
                .copied()
                .unwrap_or(crate::StoreFailure::Corrupt),
        )
        .with_delivery(DeliveryStatus::Ambiguous);
    }
    if let rumqttc_v5::ConnectionError::SessionRestore(source) = &error {
        let failure = match source {
            rumqttc_v5::SessionRestoreError::UnsupportedFormatVersion { .. } => {
                crate::StoreFailure::Version
            }
            _ => crate::StoreFailure::Corrupt,
        };
        return Error::store(failure).with_delivery(DeliveryStatus::Ambiguous);
    }
    let kind = match error {
        #[cfg(any(feature = "use-rustls", feature = "use-native-tls"))]
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
        rumqttc_v5::ConnectionError::Io(_) => ErrorKind::Network,
        #[cfg(any(feature = "http-proxy", feature = "socks-proxy"))]
        rumqttc_v5::ConnectionError::Proxy(_) => ErrorKind::Network,
        #[cfg(feature = "websocket")]
        rumqttc_v5::ConnectionError::Websocket(_) | rumqttc_v5::ConnectionError::WsConnect(_) => {
            ErrorKind::Network
        }
        _ => ErrorKind::Protocol,
    };
    let reason = match &error {
        rumqttc_v5::ConnectionError::ConnectionRefused(reason) => Some(connack_reason(*reason)),
        rumqttc_v5::ConnectionError::MqttState(rumqttc_v5::StateError::ServerDisconnect {
            reason_code,
            ..
        }) => Some(*reason_code as u8),
        _ => None,
    };
    // Error source chains may contain raw packets, credentials, or peer-supplied
    // text. Owned connection events carry legal details without logging them.
    let error = Error::new(kind, "MQTT connection failed").with_delivery(DeliveryStatus::Ambiguous);
    reason.map_or(error.clone(), |reason| error.with_broker_reason(reason))
}

pub const fn map_outgoing(outgoing: &rumqttc_v5::Outgoing) -> OutgoingActivity {
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

fn build_options(
    common: &crate::CommonConfig,
    protocol: crate::V5Config,
) -> crate::Result<rumqttc_v5::MqttOptions> {
    #[cfg(any(feature = "use-rustls", feature = "use-native-tls"))]
    let tls = match &common.transport {
        crate::TransportConfig::Tls(tls) | crate::TransportConfig::Wss(tls) => {
            Some(super::build_tls(tls)?)
        }
        _ => None,
    };
    let mut options = match (&common.broker, &common.transport) {
        (
            crate::BrokerTarget::Tcp { host, port },
            crate::TransportConfig::Tcp | crate::TransportConfig::Tls(_),
        ) => rumqttc_v5::MqttOptions::new(
            common.client_id.clone(),
            rumqttc_v5::Broker::tcp(host.clone(), *port),
        ),
        #[cfg(feature = "websocket")]
        (crate::BrokerTarget::WebSocket { url }, crate::TransportConfig::WebSocket) => {
            rumqttc_v5::MqttOptions::new(
                common.client_id.clone(),
                rumqttc_v5::Broker::websocket(url.clone()).map_err(|_| {
                    Error::new(ErrorKind::Configuration, "invalid WebSocket broker URL")
                })?,
            )
        }
        #[cfg(all(
            feature = "websocket",
            any(feature = "use-rustls", feature = "use-native-tls")
        ))]
        (crate::BrokerTarget::WebSocket { url }, crate::TransportConfig::Wss(_)) => {
            rumqttc_v5::MqttOptions::websocket_with_tls_config(
                common.client_id.clone(),
                url.clone(),
                tls.clone().expect("WSS TLS built"),
            )
            .map_err(|_| {
                Error::new(
                    ErrorKind::Configuration,
                    "invalid secure WebSocket broker URL",
                )
            })?
        }
        #[cfg(unix)]
        (crate::BrokerTarget::Unix { path }, crate::TransportConfig::Unix) => {
            rumqttc_v5::MqttOptions::new(
                common.client_id.clone(),
                rumqttc_v5::Broker::unix(path.clone()),
            )
        }
        _ => {
            return Err(Error::configuration(
                "unsupported broker and transport configuration",
            ));
        }
    };
    #[cfg(any(feature = "use-rustls", feature = "use-native-tls"))]
    if matches!(common.transport, crate::TransportConfig::Tls(_)) {
        options.set_transport(rumqttc_v5::Transport::tls_with_config(
            tls.expect("TLS built"),
        ));
    }
    options.set_keep_alive(crate::handle::duration_to_u16(
        common.keep_alive,
        "keep alive",
    )?);
    options.set_max_request_batch(common.max_request_batch);
    options.set_read_batch_size(common.read_batch_size);
    options.set_pending_throttle(common.pending_throttle);
    options.set_connect_timeout(common.connection_timeout);
    if let Some(limit) = protocol.outgoing_inflight_upper_limit {
        options.set_outgoing_inflight_upper_limit(limit);
    }
    options.set_topic_alias_policy(match protocol.topic_alias_policy {
        crate::TopicAliasPolicy::Disabled => rumqttc_v5::TopicAliasPolicy::Disabled,
        crate::TopicAliasPolicy::Monotonic => rumqttc_v5::TopicAliasPolicy::Monotonic,
        crate::TopicAliasPolicy::Lru => rumqttc_v5::TopicAliasPolicy::Lru,
    });
    if let Some(will) = &common.last_will {
        options.set_last_will(rumqttc_v5::LastWill {
            topic: bytes::Bytes::copy_from_slice(will.topic.as_bytes()),
            message: will.payload.clone(),
            qos: to_qos(will.qos),
            retain: will.retain,
            properties: match &will.protocol {
                crate::LastWillProtocolOptions::VersionNeutral => None,
                crate::LastWillProtocolOptions::V5(p) => Some(rumqttc_v5::LastWillProperties {
                    delay_interval: p.will_delay_interval,
                    payload_format_indicator: p.payload_format_indicator,
                    message_expiry_interval: p.message_expiry_interval,
                    content_type: p.content_type.clone(),
                    response_topic: p.response_topic.clone(),
                    correlation_data: p.correlation_data.clone(),
                    user_properties: p.user_properties.clone(),
                }),
            },
        });
    }
    options.set_request_channel_capacity(common.request_channel_capacity);
    #[cfg(any(feature = "http-proxy", feature = "socks-proxy"))]
    if let Some(proxy) = &common.proxy {
        options.set_proxy(super::build_proxy(proxy)?);
    }
    #[cfg(feature = "websocket")]
    if !common.websocket_headers.is_empty() {
        options.set_request_modifier(crate::websocket::prepare(&common.websocket_headers)?);
    }
    options.set_ack_mode(match common.ack_mode {
        crate::AckMode::Automatic => rumqttc_v5::AckMode::Automatic,
        crate::AckMode::Manual => rumqttc_v5::AckMode::Manual,
    });
    options.set_network_options(super::build_network(common));
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
    super::redirect::configure(
        &mut options,
        &protocol.redirect_policy,
        protocol.srv_resolver,
    )?;
    options.set_broker_session_resume_policy(match protocol.broker_session_resume_policy {
        crate::BrokerSessionResumePolicy::Strict => rumqttc_v5::BrokerSessionResumePolicy::Strict,
        crate::BrokerSessionResumePolicy::AllowBrokerOnly => {
            rumqttc_v5::BrokerSessionResumePolicy::AllowBrokerOnly
        }
    });
    let p = protocol.connect_properties;
    options.set_connect_properties(rumqttc_v5::ConnectProperties {
        session_expiry_interval: p.session_expiry_interval,
        receive_maximum: p.receive_maximum,
        max_packet_size: p.maximum_packet_size,
        topic_alias_max: p.topic_alias_maximum,
        request_response_info: p.request_response_information,
        request_problem_info: p.request_problem_information,
        user_properties: p.user_properties,
        authentication_method: p.authentication_method,
        authentication_data: p.authentication_data,
    });
    options.set_local_incoming_packet_size_limit(match common.incoming_packet_size_limit {
        crate::IncomingPacketLimit::Default => rumqttc_v5::IncomingPacketSizeLimit::Default,
        crate::IncomingPacketLimit::Bytes(bytes) => {
            rumqttc_v5::IncomingPacketSizeLimit::Bytes(bytes)
        }
        crate::IncomingPacketLimit::Unlimited => rumqttc_v5::IncomingPacketSizeLimit::Unlimited,
    });
    if let Some(store) = protocol.session_store {
        options.set_session_store_scope(store.scope.clone());
        options.set_session_store(super::session::Adapter::new(
            store,
            ProtocolVersion::V5,
            &common.client_id,
        )?);
    }
    options.validate().map_err(|error| {
        Error::sourced(
            ErrorKind::Configuration,
            DeliveryStatus::NotApplicable,
            error,
        )
    })?;
    Ok(options)
}

pub struct Driver {
    eventloop: rumqttc_v5::EventLoop,
    auth: std::sync::Arc<super::auth::Monitor>,
}

pub fn build(
    common: &crate::CommonConfig,
    protocol: crate::V5Config,
) -> crate::Result<(rumqttc_v5::AsyncClient, Box<Driver>)> {
    let authenticator = protocol.authenticator.clone();
    #[cfg(feature = "auth-scram")]
    let authenticator = match &protocol.scram {
        Some(scram) => Some(crate::scram::build(scram.clone())),
        None => authenticator,
    };
    let mut options = build_options(common, protocol)?;
    let auth = std::sync::Arc::new(super::auth::Monitor::default());
    if let Some(config) = authenticator {
        options.set_authenticator(std::sync::Arc::new(std::sync::Mutex::new(
            super::auth::Adapter {
                client_id: common.client_id.clone(),
                config,
                monitor: auth.clone(),
                generation: 0,
            },
        )));
    }
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
    Ok((client, Box::new(Driver { eventloop, auth })))
}
pub async fn run(driver: Box<Driver>, context: DriverContext) -> TerminalStatus {
    let Driver {
        mut eventloop,
        auth,
    } = *driver;
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
        panic_rx,
    } = context;
    let mut pending = FuturesUnordered::<PendingFuture>::new();
    let mut senders = HashMap::<OperationId, PendingSender>::new();
    let mut connected = false;
    let mut unresolved_redirect: Option<crate::RedirectEvent> = None;
    let mut diagnostics = snapshot_v5(&eventloop);
    let shutdown = ShutdownInputs::new(&shared, &completion_rx, &diagnostics_rx);
    let delivery = EventDelivery {
        shared: &shared,
        events: &events,
        timeout: delivery_timeout,
        immediate_shutdown: &immediate_shutdown_rx,
        panic: &panic_rx,
    };
    loop {
        // See the v4 loop: polling is an indivisible ownership boundary even while wrapper
        // registrations, cached diagnostics, and completed notices remain responsive.
        let polled = {
            let poll = eventloop.poll();
            tokio::pin!(poll);
            loop {
                let (failure, deadline) = auth.snapshot();
                if let Some(failure) = failure.or_else(|| {
                    deadline
                        .filter(|deadline| *deadline <= tokio::time::Instant::now())
                        .map(|_| crate::AuthFailure::Timeout)
                }) {
                    let error = Error::auth(failure).with_delivery(DeliveryStatus::Ambiguous);
                    shared.fail_acknowledgements(&error);
                    fail_pending(&mut senders, &error);
                    return TerminalStatus::Failed(error);
                }
                // Keep parity with the fair and cooperative v4 arbitration above.
                tokio::select! {
                    () = auth.changed.notified() => {},
                    () = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; } else { std::future::pending::<()>().await; } } => {},
                    _ = panic_rx.recv_async() => crate::runtime::terminate_driver_for_boundary_panic(),
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
        synchronize_admission_state(&eventloop, &shared);
        if let Some(packet) = eventloop.take_connection_failure_packet() {
            // Native poll cleanup can consume these packets without yielding an
            // Incoming event. Preserve their owned properties before recovery.
            // A packet still queued behind the current event must be delivered
            // by a later poll, preserving the broker's packet order.
            let queued = eventloop.state.events.iter().any(
                |event| matches!(event, rumqttc_v5::Event::Incoming(queued) if queued == &packet),
            );
            let event = if queued
                || matches!(&polled, Ok(rumqttc_v5::Event::Incoming(current)) if current == &packet)
            {
                None
            } else {
                map_v5_event(
                    &mut eventloop,
                    rumqttc_v5::Event::Incoming(packet),
                    &shared,
                    &mut connected,
                    emit_outgoing,
                    manual_ack,
                    protocol,
                )
            };
            if let Some(event) = event
                && !deliver(&delivery, event).await
            {
                return TerminalStatus::Failed(overflow_error());
            }
        }
        if let Some(failure) = auth.snapshot().0 {
            let error = Error::auth(failure).with_delivery(DeliveryStatus::Ambiguous);
            shared.fail_acknowledgements(&error);
            fail_pending(&mut senders, &error);
            return TerminalStatus::Failed(error);
        }
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
                ) {
                    if let WrapperEvent::Redirect(redirect) = &event {
                        unresolved_redirect = redirect.target.is_none().then(|| redirect.clone());
                    }
                    if matches!(&event, WrapperEvent::Connected { .. })
                        && let Some(mut redirect) = unresolved_redirect.take()
                    {
                        redirect.target = super::redirect::broker(eventloop.options.broker());
                        if !deliver(&delivery, WrapperEvent::Redirect(redirect)).await {
                            let error = overflow_error();
                            shared.fail_acknowledgements(&error);
                            fail_pending(&mut senders, &error);
                            return TerminalStatus::Failed(error);
                        }
                    }
                    if !deliver(&delivery, event).await {
                        let error = overflow_error();
                        shared.fail_acknowledgements(&error);
                        fail_pending(&mut senders, &error);
                        return TerminalStatus::Failed(error);
                    }
                }
            }
            Err(rumqttc_v5::ConnectionError::RequestsDone) => {
                let graceful =
                    complete_shutdown(&shutdown, &diagnostics, &mut pending, &mut senders).await;
                return TerminalStatus::Closed { graceful };
            }
            Err(error) => {
                if let rumqttc_v5::ConnectionError::Redirect(redirect) = &error {
                    let failure = super::redirect::failure(&redirect.failure);
                    let event =
                        super::redirect::event(redirect.outcome.clone(), None, Some(failure));
                    let terminal =
                        Error::redirect(failure).with_delivery(DeliveryStatus::Ambiguous);
                    shared.fail_acknowledgements(&terminal);
                    fail_pending(&mut senders, &terminal);
                    if !deliver(&delivery, WrapperEvent::Redirect(event)).await {
                        return TerminalStatus::Failed(overflow_error());
                    }
                    return TerminalStatus::Failed(terminal);
                }
                let graceful_disconnect_timed_out =
                    matches!(&error, rumqttc_v5::ConnectionError::DisconnectTimeout);
                let error = shared.contextualize(map_connection_error(error));
                if error.kind() == ErrorKind::Persistence {
                    shared.fail_acknowledgements(&error);
                    fail_pending(&mut senders, &error);
                    return TerminalStatus::Failed(error);
                }
                if graceful_disconnect_timed_out && shared.timeout_graceful_shutdown(error.clone())
                {
                    return finish_close(&shutdown, &diagnostics, &mut pending, &mut senders).await;
                }
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

const fn connack_reason(reason: rumqttc_v5::ConnectReturnCode) -> u8 {
    use rumqttc_v5::ConnectReturnCode as C;
    match reason {
        C::Success => 0,
        C::RefusedProtocolVersion => 1,
        C::BadClientId => 2,
        C::ServiceUnavailable => 3,
        C::UnspecifiedError => 0x80,
        C::MalformedPacket => 0x81,
        C::ProtocolError => 0x82,
        C::ImplementationSpecificError => 0x83,
        C::UnsupportedProtocolVersion => 0x84,
        C::ClientIdentifierNotValid => 0x85,
        C::BadUserNamePassword => 0x86,
        C::NotAuthorized => 0x87,
        C::ServerUnavailable => 0x88,
        C::ServerBusy => 0x89,
        C::Banned => 0x8a,
        C::BadAuthenticationMethod => 0x8c,
        C::TopicNameInvalid => 0x90,
        C::PacketTooLarge => 0x95,
        C::QuotaExceeded => 0x97,
        C::PayloadFormatInvalid => 0x99,
        C::RetainNotSupported => 0x9a,
        C::QoSNotSupported => 0x9b,
        C::UseAnotherServer => 0x9c,
        C::ServerMoved => 0x9d,
        C::ConnectionRateExceeded => 0x9f,
    }
}

fn connack_details(connack: rumqttc_v5::ConnAck) -> crate::ConnAckDetails {
    crate::ConnAckDetails {
        reason_code: connack_reason(connack.code),
        v5_properties: connack.properties.map(|p| {
            Box::new(crate::V5ConnAckProperties {
                session_expiry_interval: p.session_expiry_interval,
                receive_maximum: p.receive_max,
                maximum_qos: p.max_qos,
                retain_available: p.retain_available,
                maximum_packet_size: p.max_packet_size,
                assigned_client_identifier: p.assigned_client_identifier,
                topic_alias_maximum: p.topic_alias_max,
                reason_string: p.reason_string,
                wildcard_subscription_available: p.wildcard_subscription_available,
                subscription_identifiers_available: p.subscription_identifiers_available,
                shared_subscription_available: p.shared_subscription_available,
                server_keep_alive: p.server_keep_alive,
                response_information: p.response_information,
                server_reference: p.server_reference,
                authentication_method: p.authentication_method,
                authentication_data: p.authentication_data,
                user_properties: p.user_properties,
            })
        }),
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
        rumqttc_v5::Event::Redirect(outcome) => {
            shared.invalidate_connection(&Error::new(ErrorKind::Network, "connection redirected"));
            *connected = false;
            let redirect = eventloop.diagnostics().redirect;
            let target = if redirect.srv_owner.is_some() && redirect.srv_current_target.is_none() {
                None
            } else {
                super::redirect::broker(eventloop.options.broker())
            };
            Some(WrapperEvent::Redirect(super::redirect::event(
                outcome, target, None,
            )))
        }
        rumqttc_v5::Event::Auth(event) => {
            use rumqttc_v5::AuthEvent as E;
            let (kind, method, stage, failure) = match event {
                E::Started { kind, method } => (kind, method, crate::AuthStage::Started, None),
                E::Continue { kind, method } => (kind, method, crate::AuthStage::Continue, None),
                E::Succeeded { kind, method } => (kind, method, crate::AuthStage::Succeeded, None),
                E::Failed {
                    kind,
                    method,
                    reason,
                } => {
                    let failure = match reason {
                        rumqttc_v5::AuthFailureReason::BrokerDisconnected(_) => {
                            crate::AuthFailure::BrokerRejected
                        }
                        rumqttc_v5::AuthFailureReason::OverlappingReauth => {
                            crate::AuthFailure::Overlapping
                        }
                        rumqttc_v5::AuthFailureReason::MissingAuthenticationMethod => {
                            crate::AuthFailure::Method
                        }
                        rumqttc_v5::AuthFailureReason::AuthenticationFailed(_) => {
                            crate::AuthFailure::Rejected
                        }
                        rumqttc_v5::AuthFailureReason::ProtocolError => {
                            crate::AuthFailure::InvalidResponse
                        }
                        _ => crate::AuthFailure::ConnectionClosed,
                    };
                    (kind, method, crate::AuthStage::Failed, Some(failure))
                }
            };
            Some(WrapperEvent::Authentication(crate::AuthEvent {
                exchange: match kind {
                    rumqttc_v5::AuthExchangeKind::InitialConnect => crate::AuthExchange::Initial,
                    rumqttc_v5::AuthExchangeKind::Reauthentication => {
                        crate::AuthExchange::Reauthentication
                    }
                },
                method,
                stage,
                failure,
            }))
        }
        rumqttc_v5::Event::Incoming(rumqttc_v5::Packet::ConnAck(connack)) => {
            if connack.code != rumqttc_v5::ConnectReturnCode::Success {
                return Some(WrapperEvent::ConnectionRejected(connack_details(connack)));
            }
            shared.begin_connection(protocol, connack.session_present, || {
                eventloop.discard_pending_manual_acknowledgements();
            });
            *connected = true;
            Some(WrapperEvent::Connected {
                protocol,
                session_present: connack.session_present,
                details: connack_details(connack),
            })
        }
        rumqttc_v5::Event::Incoming(rumqttc_v5::Packet::Disconnect(packet)) => {
            let p = packet
                .properties
                .unwrap_or(rumqttc_v5::DisconnectProperties {
                    session_expiry_interval: None,
                    reason_string: None,
                    user_properties: Vec::new(),
                    server_reference: None,
                });
            Some(WrapperEvent::BrokerDisconnect(crate::V5DisconnectOptions {
                reason_code: packet.reason_code as u8,
                session_expiry_interval: p.session_expiry_interval,
                reason_string: p.reason_string,
                user_properties: p.user_properties,
                server_reference: p.server_reference,
            }))
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
            emit_outgoing.then(|| {
                let packet_id = match outgoing {
                    rumqttc_v5::Outgoing::Publish(id)
                    | rumqttc_v5::Outgoing::Subscribe(id)
                    | rumqttc_v5::Outgoing::Unsubscribe(id)
                    | rumqttc_v5::Outgoing::PubAck(id)
                    | rumqttc_v5::Outgoing::PubRec(id)
                    | rumqttc_v5::Outgoing::PubRel(id)
                    | rumqttc_v5::Outgoing::PubComp(id)
                    | rumqttc_v5::Outgoing::AwaitAck(id) => (id != 0).then_some(id),
                    _ => None,
                };
                WrapperEvent::Outgoing(crate::OutgoingEvent {
                    activity: map_outgoing(&outgoing),
                    packet_id,
                })
            })
        }
        _ => None,
    }
}

fn synchronize_admission_state(eventloop: &rumqttc_v5::EventLoop, shared: &Shared) {
    let options = &eventloop.options;
    shared.set_protocol_admission_state(
        options.session_expiry_interval().unwrap_or(0) == 0,
        options.authenticator().is_some(),
    );
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

pub fn publish_options(command: &PublishCommand) -> rumqttc_v5::PublishOptions {
    let options = rumqttc_v5::PublishOptions::new(to_qos(command.qos)).retain(command.retain);
    match command.protocol.clone() {
        PublishProtocolOptions::VersionNeutral => options,
        PublishProtocolOptions::V5(properties) => {
            options.properties(to_outgoing_publish_properties(properties))
        }
    }
}

pub const fn to_retain_forward_rule(rule: V5RetainForwardRule) -> rumqttc_v5::RetainForwardRule {
    match rule {
        V5RetainForwardRule::OnEverySubscribe => rumqttc_v5::RetainForwardRule::OnEverySubscribe,
        V5RetainForwardRule::OnNewSubscribe => rumqttc_v5::RetainForwardRule::OnNewSubscribe,
        V5RetainForwardRule::Never => rumqttc_v5::RetainForwardRule::Never,
    }
}

pub fn to_subscribe_properties(
    properties: V5SubscribeProperties,
) -> rumqttc_v5::SubscribeProperties {
    rumqttc_v5::SubscribeProperties {
        id: properties.subscription_identifier,
        user_properties: properties.user_properties,
    }
}

pub fn to_unsubscribe_properties(
    properties: V5UnsubscribeProperties,
) -> rumqttc_v5::UnsubscribeProperties {
    rumqttc_v5::UnsubscribeProperties {
        user_properties: properties.user_properties,
    }
}

pub const fn to_qos(qos: QoS) -> rumqttc_v5::QoS {
    match qos {
        QoS::AtMostOnce => rumqttc_v5::QoS::AtMostOnce,
        QoS::AtLeastOnce => rumqttc_v5::QoS::AtLeastOnce,
        QoS::ExactlyOnce => rumqttc_v5::QoS::ExactlyOnce,
    }
}

pub const fn from_qos(qos: rumqttc_v5::QoS) -> QoS {
    match qos {
        rumqttc_v5::QoS::AtMostOnce => QoS::AtMostOnce,
        rumqttc_v5::QoS::AtLeastOnce => QoS::AtLeastOnce,
        rumqttc_v5::QoS::ExactlyOnce => QoS::ExactlyOnce,
    }
}

pub fn to_outgoing_publish_properties(
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

pub fn from_incoming_publish_properties(
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

pub fn map_publish_notice(
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

pub fn map_subscribe_notice(
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

pub fn map_unsubscribe_notice(
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

pub const fn v5_suback_code(reason: rumqttc_v5::SubscribeReasonCode) -> u8 {
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

pub const fn v5_puback_success(reason: rumqttc_v5::PubAckReason) -> bool {
    matches!(
        reason,
        rumqttc_v5::PubAckReason::Success | rumqttc_v5::PubAckReason::NoMatchingSubscribers
    )
}

pub const fn v5_puback_code(reason: rumqttc_v5::PubAckReason) -> u8 {
    reason as u8
}

pub const fn v5_pubrec_code(reason: rumqttc_v5::PubRecReason) -> u8 {
    reason as u8
}

pub const fn v5_pubcomp_code(reason: rumqttc_v5::PubCompReason) -> u8 {
    reason as u8
}

pub fn v5_pubcomp_success(reason: rumqttc_v5::PubCompReason) -> bool {
    reason == rumqttc_v5::PubCompReason::Success
}

pub fn map_notice_error<E: std::error::Error + Send + Sync + 'static>(error: E) -> Error {
    Error::sourced(ErrorKind::Protocol, DeliveryStatus::Ambiguous, error)
}

pub fn broker_rejection(code: u8) -> Error {
    Error::new(
        ErrorKind::Protocol,
        format!("broker rejected operation with reason code 0x{code:02x}"),
    )
    .with_delivery(DeliveryStatus::Rejected)
    .with_broker_reason(code)
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn unrecoverable_alias_notice_retains_the_native_terminal_reason() {
        // Managed producer admission prevents unknown alias-only publishes.
        // Keep the defensive native replay failure observable if a restored or
        // legacy request nevertheless reaches this path.
        let error = map_publish_notice(Err(
            rumqttc_v5::PublishNoticeError::TopicAliasReplayUnavailable(7),
        ))
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Protocol);
        assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
        assert_eq!(
            std::error::Error::source(&error).unwrap().to_string(),
            rumqttc_v5::PublishNoticeError::TopicAliasReplayUnavailable(7).to_string()
        );
    }

    #[test]
    fn connack_conversion_preserves_every_owned_property() {
        let properties = rumqttc_v5::ConnAckProperties {
            session_expiry_interval: Some(11),
            receive_max: Some(12),
            max_qos: Some(1),
            retain_available: Some(0),
            max_packet_size: Some(12345),
            assigned_client_identifier: Some("assigned".into()),
            topic_alias_max: Some(13),
            reason_string: Some(String::new()),
            wildcard_subscription_available: Some(0),
            subscription_identifiers_available: Some(1),
            shared_subscription_available: Some(0),
            server_keep_alive: Some(14),
            response_information: Some(String::new()),
            server_reference: Some("target".into()),
            authentication_method: Some("method".into()),
            authentication_data: Some(bytes::Bytes::from_static(&[0, 255])),
            user_properties: vec![("k".into(), "one".into()), ("k".into(), String::new())],
        };
        let details = connack_details(rumqttc_v5::ConnAck {
            session_present: false,
            code: rumqttc_v5::ConnectReturnCode::NotAuthorized,
            properties: Some(properties.clone()),
        });
        assert_eq!(details.reason_code, 0x87);
        assert_eq!(
            *details.v5_properties.unwrap(),
            crate::V5ConnAckProperties {
                session_expiry_interval: properties.session_expiry_interval,
                receive_maximum: properties.receive_max,
                maximum_qos: properties.max_qos,
                retain_available: properties.retain_available,
                maximum_packet_size: properties.max_packet_size,
                assigned_client_identifier: properties.assigned_client_identifier,
                topic_alias_maximum: properties.topic_alias_max,
                reason_string: properties.reason_string,
                wildcard_subscription_available: properties.wildcard_subscription_available,
                subscription_identifiers_available: properties.subscription_identifiers_available,
                shared_subscription_available: properties.shared_subscription_available,
                server_keep_alive: properties.server_keep_alive,
                response_information: properties.response_information,
                server_reference: properties.server_reference,
                authentication_method: properties.authentication_method,
                authentication_data: properties.authentication_data,
                user_properties: properties.user_properties,
            }
        );
    }

    #[test]
    fn options_preserve_all_connect_properties_and_operational_controls() {
        let mut common = crate::CommonConfig::new("mapping", "localhost", 1883);
        common.max_request_batch = 17;
        common.read_batch_size = 23;
        common.pending_throttle = std::time::Duration::from_nanos(313);
        common.connection_timeout = std::time::Duration::from_secs(19);
        common.incoming_packet_size_limit = crate::IncomingPacketLimit::Unlimited;
        common.network.local_address = Some("127.0.0.1:0".parse().unwrap());
        let properties = crate::V5ConnectProperties {
            session_expiry_interval: Some(11),
            receive_maximum: Some(13),
            maximum_packet_size: Some(65537),
            topic_alias_maximum: Some(17),
            request_response_information: Some(0),
            request_problem_information: Some(1),
            user_properties: vec![("k".into(), "v".into()), ("k".into(), String::new())],
            authentication_method: Some("method".into()),
            authentication_data: Some(bytes::Bytes::new()),
        };
        let options = build_options(
            &common,
            crate::V5Config {
                connect_properties: properties.clone(),
                topic_alias_policy: crate::TopicAliasPolicy::Lru,
                outgoing_inflight_upper_limit: Some(7),
                ..Default::default()
            },
        )
        .unwrap();
        let actual = options.connect_properties().unwrap();
        assert_eq!(
            actual.session_expiry_interval,
            properties.session_expiry_interval
        );
        assert_eq!(actual.receive_maximum, properties.receive_maximum);
        assert_eq!(actual.max_packet_size, properties.maximum_packet_size);
        assert_eq!(actual.topic_alias_max, properties.topic_alias_maximum);
        assert_eq!(
            actual.request_response_info,
            properties.request_response_information
        );
        assert_eq!(
            actual.request_problem_info,
            properties.request_problem_information
        );
        assert_eq!(actual.user_properties, properties.user_properties);
        assert_eq!(
            actual.authentication_method,
            properties.authentication_method
        );
        assert_eq!(actual.authentication_data, properties.authentication_data);
        assert_eq!(options.max_request_batch(), 17);
        assert_eq!(options.read_batch_size(), 23);
        assert_eq!(options.pending_throttle(), common.pending_throttle);
        assert_eq!(options.connect_timeout(), common.connection_timeout);
        assert_eq!(options.network_options().connection_timeout(), 19);
        assert_eq!(
            options.network_options().bind_addr(),
            common.network.local_address
        );
        assert_eq!(
            options.incoming_packet_size_limit(),
            rumqttc_v5::IncomingPacketSizeLimit::Unlimited
        );
        assert_eq!(
            options.topic_alias_policy(),
            rumqttc_v5::TopicAliasPolicy::Lru
        );
        assert_eq!(options.get_outgoing_inflight_upper_limit(), Some(7));
    }
}
