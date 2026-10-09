use crate::{
    AcknowledgementKind, AcknowledgementProperties, BrokerAcknowledgement, BrokerReason,
    Completion, DeliveryStatus, Error, ErrorKind, OutgoingActivity, PublishCommand,
    PublishCompletion, PublishProtocolOptions, QoS, Result, SubscribeCommand, SubscribeCompletion,
    SubscribeProtocolOptions, SubscribeResult, TerminalOutcome, UnsubscribeCommand,
    UnsubscribeCompletion, UnsubscribeProtocolOptions, UnsubscribeResult,
    V5IncomingPublishProperties, V5OutgoingPublishProperties, V5RetainForwardRule,
    V5SubscribeProperties, V5UnsubscribeProperties,
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
        return Err(
            protocol_option_error("topic alias must be greater than zero")
                .with_publish_failure(crate::PublishFailure::TopicAliasZero),
        );
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

fn recovery_shutdown(shared: &crate::handle::Shared) -> Option<rumqttc_v5::Disconnect> {
    let options = shared.recovery_shutdown_options()?;
    let disconnect = match options {
        crate::DisconnectProtocolOptions::VersionNeutral => {
            rumqttc_v5::Disconnect::new(rumqttc_v5::DisconnectReasonCode::NormalDisconnection)
        }
        crate::DisconnectProtocolOptions::V5(properties) => {
            let (reason, properties) = disconnect_properties(&properties)
                .expect("committed shutdown properties were validated at admission");
            rumqttc_v5::Disconnect {
                reason_code: reason,
                properties: Some(properties),
            }
        }
    };
    Some(disconnect)
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

pub fn map_client_error(error: &rumqttc_v5::ClientError) -> Error {
    use crate::{ErrorCode as C, PublishFailure as F};
    use rumqttc_v5::{ClientError as E, PublishAdmissionError as A, PublishBudgetError as B};
    let (kind, code, failure) = match error {
        E::RequestChannelFull(request) => (
            ErrorKind::Backpressure,
            C::RequestBackpressure,
            matches!(request.as_ref(), rumqttc_v5::Request::Publish(_))
                .then_some(F::RequestChannelFull),
        ),
        E::PublishAdmissionPending { .. } => (
            ErrorKind::Backpressure,
            C::PublishCapabilitiesPending,
            Some(F::CapabilitiesPending),
        ),
        E::PublishBudget { reason, .. } => match reason {
            B::RecoveryPending => (
                ErrorKind::Backpressure,
                C::PublishRecoveryPending,
                Some(F::RecoveryPending),
            ),
            B::CountExhausted => (
                ErrorKind::Backpressure,
                C::PublishBudgetExhausted,
                Some(F::CountExhausted),
            ),
            B::BytesExhausted => (
                ErrorKind::Backpressure,
                C::PublishBudgetExhausted,
                Some(F::BytesExhausted),
            ),
            B::TooLarge => (ErrorKind::Admission, C::PublishTooLarge, Some(F::TooLarge)),
        },
        E::PublishAdmissionRejected { reason, .. } => (
            ErrorKind::Admission,
            C::PublishRejected,
            Some(match reason {
                A::RetainUnavailable => F::RetainUnavailable,
                A::MaximumQos { .. } => F::MaximumQos,
                A::TopicAliasZero => F::TopicAliasZero,
                A::TopicAliasMaximum { .. } => F::TopicAliasMaximum,
                A::TopicAliasUnmapped(_) => F::TopicAliasUnmapped,
            }),
        ),
        #[cfg(feature = "ordered-shutdown")]
        E::Closing(_) => (ErrorKind::Shutdown, C::Shutdown, None),
        E::RequestChannelDisconnected(_) => (ErrorKind::Shutdown, C::Shutdown, None),
        _ => (ErrorKind::Admission, C::CommandInvalid, None),
    };
    // Rejected requests can own credentials or payloads. Never retain them in wrapper errors.
    let mut error = Error::new(kind, "MQTT request admission failed")
        .with_code(code)
        .with_delivery(DeliveryStatus::NotAdmitted);
    if let Some(failure) = failure {
        error = error.with_publish_failure(failure);
    }
    error
}

pub fn map_connection_error(error: &rumqttc_v5::ConnectionError) -> Error {
    #[cfg(feature = "ordered-shutdown")]
    if let rumqttc_v5::ConnectionError::OrderedDisconnect(reason) = error {
        return map_ordered_error(reason.clone());
    }

    #[cfg(feature = "websocket")]
    if let rumqttc_v5::ConnectionError::RequestModifier(source) = error
        && let Some(failure) = source.downcast_ref::<crate::WebSocketHandshakeFailure>()
    {
        return Error::websocket(*failure);
    }
    if let rumqttc_v5::ConnectionError::SessionStore(source) = error {
        return Error::store(
            source
                .downcast_ref::<crate::StoreFailure>()
                .copied()
                .unwrap_or(crate::StoreFailure::Corrupt),
        )
        .with_delivery(DeliveryStatus::Ambiguous);
    }
    if let rumqttc_v5::ConnectionError::SessionRestore(source) = error {
        let failure = match source {
            rumqttc_v5::SessionRestoreError::PublishBudgetExceeded => {
                return Error::store(crate::StoreFailure::PublishBudgetExceeded)
                    .with_code(crate::ErrorCode::PublishRestoreBudgetExceeded)
                    .with_publish_failure(crate::PublishFailure::RestoreBudgetExceeded);
            }
            rumqttc_v5::SessionRestoreError::UnsupportedFormatVersion { .. } => {
                crate::StoreFailure::Version
            }
            _ => crate::StoreFailure::Corrupt,
        };
        return Error::store(failure).with_delivery(DeliveryStatus::Ambiguous);
    }
    if let rumqttc_v5::ConnectionError::Redirect(redirect) = error {
        let mut terminal = Error::redirect(super::redirect::failure(&redirect.failure))
            .with_delivery(DeliveryStatus::Ambiguous);
        if let Some(failure) = crate::tls_advanced::callback_failure(error) {
            terminal = terminal.with_tls_callback_failure(failure);
        }
        if let Some(failure) = super::transport::failure(error) {
            terminal = terminal.with_transport_failure(failure);
        }
        if matches!(
            redirect.failure,
            rumqttc_v5::RedirectFailure::Target(rumqttc_v5::RedirectTargetError::Policy(
                rumqttc_v5::RedirectPolicyFailure::StoreInUse
            ))
        ) {
            terminal = terminal.with_store_failure(crate::StoreFailure::InUse);
        }
        if let rumqttc_v5::RedirectFailure::FollowFailed(source)
        | rumqttc_v5::RedirectFailure::SrvTargetsExhausted {
            last_error: source, ..
        } = &redirect.failure
            && let Some(failure) = map_connection_error(source).store_failure()
        {
            terminal = terminal.with_store_failure(failure);
        }
        return terminal;
    }
    if let Some(failure) = crate::tls_advanced::callback_failure(error) {
        return Error::tls_callback(failure).with_delivery(DeliveryStatus::Ambiguous);
    }
    if let Some(failure) = super::transport::failure(error) {
        return Error::transport(failure).with_delivery(DeliveryStatus::Ambiguous);
    }
    let kind = match error {
        #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
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
    let reason = match error {
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
    let Some(reason) = reason else { return error };
    error.with_broker_reason(reason)
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
    PendingFuture, PendingSender, accept_registration, fail_pending, process_ready_completions,
    resolve_pending,
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

pub(super) fn build_transport_options(
    common: &crate::CommonConfig,
    tls_callbacks: &std::sync::Arc<super::TlsCallbackMonitor>,
) -> crate::Result<rumqttc_v5::MqttOptions> {
    #[cfg(not(any(feature = "use-rustls-no-provider", feature = "use-native-tls")))]
    let _ = tls_callbacks;
    #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
    let tls = match &common.transport {
        crate::TransportConfig::Tls(tls) | crate::TransportConfig::Wss(tls) => Some(
            super::build_tls(tls, crate::TlsLayer::Broker, tls_callbacks)?,
        ),
        _ => None,
    };
    let options = match (&common.broker, &common.transport) {
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
            any(feature = "use-rustls-no-provider", feature = "use-native-tls")
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
    #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
    let options = if matches!(common.transport, crate::TransportConfig::Tls(_)) {
        let mut options = options;
        options.set_transport(rumqttc_v5::Transport::tls_with_config(
            tls.expect("TLS built"),
        ));
        options
    } else {
        options
    };
    Ok(options)
}

fn build_options(
    common: &crate::CommonConfig,
    protocol: crate::V5Config,
    tls_callbacks: &std::sync::Arc<super::TlsCallbackMonitor>,
) -> crate::Result<rumqttc_v5::MqttOptions> {
    let mut options = build_transport_options(common, tls_callbacks)?;
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
        options.set_last_will(last_will(will));
    }
    options.set_request_channel_capacity(common.request_channel_capacity);
    #[cfg(any(feature = "http-proxy", feature = "socks-proxy"))]
    if let Some(proxy) = &common.proxy {
        options.set_proxy(super::build_proxy(proxy, tls_callbacks)?);
    }
    #[cfg(feature = "websocket")]
    configure_handshake(&mut options, common, std::sync::Arc::default())?;
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
    options
        .protocol_compatibility_mut()
        .set_broker_session_resume_policy(match protocol.broker_session_resume_policy {
            crate::BrokerSessionResumePolicy::Strict => {
                rumqttc_v5::BrokerSessionResumePolicy::Strict
            }
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
    let store_factory = configure_store(&mut options, protocol.session_store, &common.client_id)?;
    super::redirect::configure(
        &mut options,
        &protocol.redirect_policy,
        protocol.srv_resolver,
        common,
        store_factory,
        tls_callbacks,
    )?;
    super::transport::configure_v5(&mut options, common);
    options.validate().map_err(|error| {
        Error::sourced(
            ErrorKind::Configuration,
            DeliveryStatus::NotApplicable,
            error,
        )
    })?;
    Ok(options)
}

fn configure_store(
    options: &mut rumqttc_v5::MqttOptions,
    store: Option<crate::SessionStoreConfig>,
    client_id: &str,
) -> crate::Result<Option<std::sync::Arc<super::session::AdapterFactory>>> {
    let Some(store) = store else { return Ok(None) };
    let scope = store.scope.clone();
    let factory = std::sync::Arc::new(super::session::AdapterFactory::new(store));
    options.set_session_store_scope(scope.clone());
    options.set_session_store_arc(factory.prepare(&scope, client_id)?);
    Ok(Some(factory))
}

fn last_will(will: &crate::LastWillConfig) -> rumqttc_v5::LastWill {
    rumqttc_v5::LastWill {
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
    }
}

pub struct Driver {
    pub(super) eventloop: rumqttc_v5::EventLoop,
    pub(super) tls_callbacks: std::sync::Arc<super::TlsCallbackMonitor>,
    auth: std::sync::Arc<super::auth::Monitor>,
    websocket: std::sync::Arc<crate::websocket::HandshakeMonitor>,
}

#[cfg(feature = "websocket")]
fn configure_handshake(
    options: &mut rumqttc_v5::MqttOptions,
    common: &crate::CommonConfig,
    monitor: std::sync::Arc<crate::websocket::HandshakeMonitor>,
) -> crate::Result<()> {
    if let Some(config) = &common.websocket_handshake {
        options.set_fallible_request_modifier(crate::websocket::prepare_dynamic(
            &common.websocket_headers,
            config.clone(),
            crate::ProtocolVersion::V5,
            common.client_id.clone(),
            monitor,
        )?);
    } else if !common.websocket_headers.is_empty() {
        options.set_request_modifier(crate::websocket::prepare(&common.websocket_headers)?);
    }
    Ok(())
}

pub fn build(
    common: &crate::CommonConfig,
    protocol: crate::V5Config,
) -> crate::Result<(rumqttc_v5::AsyncClient, Box<Driver>)> {
    let publish_admission_policy = protocol.publish_admission_policy;
    let publish_budget = protocol.publish_budget;
    let authenticator = protocol.authenticator.clone();
    let async_authenticator = protocol.async_authenticator.clone();
    #[cfg(feature = "auth-scram")]
    let authenticator = protocol.scram.as_ref().map_or(authenticator, |scram| {
        Some(crate::scram::build(scram.clone()))
    });
    let tls_callbacks = std::sync::Arc::new(super::TlsCallbackMonitor::default());
    let mut options = build_options(common, protocol, &tls_callbacks)?;
    let auth = std::sync::Arc::new(super::auth::Monitor::default());
    if let Some(config) = authenticator {
        options.set_authenticator(std::sync::Arc::new(std::sync::Mutex::new(
            super::auth::Adapter {
                config,
                monitor: auth.clone(),
                generation: 0,
            },
        )));
    }
    if let Some(config) = async_authenticator {
        options.set_async_authenticator(std::sync::Arc::new(super::auth::AsyncAdapter {
            config,
            monitor: auth.clone(),
            generation: std::sync::atomic::AtomicU64::new(0),
        }));
    }
    let websocket = std::sync::Arc::new(crate::websocket::HandshakeMonitor::default());
    #[cfg(feature = "websocket")]
    configure_handshake(&mut options, common, websocket.clone())?;
    let (client, eventloop) = rumqttc_v5::AsyncClient::builder(options)
        .capacity(common.request_channel_capacity)
        .publish_admission_policy(match publish_admission_policy {
            crate::PublishAdmissionPolicy::RequireNegotiatedCapabilities => {
                rumqttc_v5::PublishAdmissionPolicy::RequireNegotiatedCapabilities
            }
            crate::PublishAdmissionPolicy::EventLoopValidated => {
                rumqttc_v5::PublishAdmissionPolicy::EventLoopValidated
            }
        })
        .publish_budget(rumqttc_v5::PublishBudgetLimits {
            max_outstanding: publish_budget.max_outstanding,
            max_bytes: publish_budget.max_bytes,
        })
        .try_build()
        .map_err(|error| {
            Error::sourced(
                ErrorKind::Configuration,
                DeliveryStatus::NotApplicable,
                error,
            )
        })?;
    Ok((
        client,
        Box::new(Driver {
            eventloop,
            tls_callbacks,
            auth,
            websocket,
        }),
    ))
}
#[allow(
    clippy::too_many_lines,
    reason = "Keep poll ownership and cancellation arbitration in one driver loop"
)]
pub async fn run(driver: Box<Driver>, context: DriverContext) -> TerminalStatus {
    let Driver {
        mut eventloop,
        tls_callbacks,
        auth,
        websocket,
    } = *driver;
    let async_authentication = eventloop.options.async_authenticator().is_some();
    let DriverContext {
        shared,
        completion_rx,
        diagnostics_rx,
        mut configuration,
        events,
        delivery_timeout,
        emit_outgoing,
        manual_ack,
        protocol,
        immediate_shutdown_rx,
        panic_rx,
    } = context;
    let configuration_rx = configuration.receiver.clone();
    let mut pending = FuturesUnordered::<PendingFuture>::new();
    let mut senders = HashMap::<OperationId, PendingSender>::new();
    let mut connected = false;
    let mut recovery_retire = false;
    #[allow(
        unused_mut,
        reason = "Ordered shutdown enables native terminal cleanup"
    )]
    let mut native_cleanup = false;
    let mut unresolved_redirect: Option<crate::RedirectEvent> = None;
    let mut pending_auth: Option<(u8, Option<crate::AuthProperties>)> = None;
    let mapping = EventMappingOptions {
        emit_outgoing,
        manual_ack,
        protocol,
        auth_failure: None,
    };
    let mut diagnostics = snapshot_v5(&eventloop);
    let shutdown = ShutdownInputs::new(&shared, &completion_rx, &diagnostics_rx);
    let delivery = EventDelivery {
        shared: &shared,
        events: &events,
        timeout: delivery_timeout,
        immediate_shutdown: &immediate_shutdown_rx,
        panic: &panic_rx,
        #[cfg(feature = "ordered-shutdown")]
        staged: std::sync::Mutex::new(None),
    };
    loop {
        // Retire notices promptly as native publish capacity is released, but leave
        // sustained control traffic to the fair arbitration below. Yield after this
        // bounded pass on every iteration so shared execution remains cooperative.
        process_ready_completions(&completion_rx, &mut pending, &mut senders);
        if !connected && !native_cleanup && !recovery_retire {
            match crate::runtime::wait_reconnect(
                &shutdown,
                &mut configuration,
                &diagnostics,
                &mut pending,
                &mut senders,
                &immediate_shutdown_rx,
                &panic_rx,
            )
            .await
            {
                crate::runtime::RetryReady::Poll | crate::runtime::RetryReady::Recovery => {}
                #[cfg(feature = "ordered-shutdown")]
                crate::runtime::RetryReady::Cleanup => native_cleanup = true,
                crate::runtime::RetryReady::Terminal(status) => return status,
            }
        }
        tokio::task::yield_now().await;
        // See the v4 loop: polling is an indivisible ownership boundary even while wrapper
        // registrations, cached diagnostics, and completed notices remain responsive.
        let mut authentication_timed_out = false;
        if !connected {
            websocket.reset();
        }
        super::configuration::apply_v5(&mut configuration, &mut eventloop);
        let recovery_phase = shared.recovery_phase();
        let recovery_cleanup = recovery_phase == Some(crate::RecoveryPhase::Quiescing);
        let recovery_attempt = recovery_phase.is_some() && !recovery_cleanup;
        let polled = {
            let poll = async {
                if recovery_retire {
                    let disconnect = recovery_shutdown(&shared)
                        .expect("candidate retirement requires committed shutdown");
                    eventloop
                        .retire_session_recovery_transport_with_disconnect(disconnect)
                        .await?;
                    Err(rumqttc_v5::ConnectionError::SessionRecoveryInvalid)
                } else if recovery_cleanup {
                    eventloop
                        .abandon_session_for_recovery_with_shutdown(|| recovery_shutdown(&shared))
                        .await?;
                    Err(rumqttc_v5::ConnectionError::SessionRecoveryPending)
                } else if recovery_attempt {
                    eventloop
                        .establish_session_for_recovery_with_shutdown(|| recovery_shutdown(&shared))
                        .await
                } else {
                    eventloop.poll().await
                }
            };
            tokio::pin!(poll);
            loop {
                if connected
                    && async_authentication
                    && shared.immediate_shutdown_requested()
                    && auth.callback_active()
                {
                    // The event loop cannot process its queued DISCONNECT while awaiting the
                    // authority. Dropping the poll cancels the retained response future.
                    break None;
                }
                let (failure, deadline) = auth.snapshot();
                if async_authentication {
                    if deadline.is_some_and(|deadline| deadline <= tokio::time::Instant::now())
                        && !auth.callback_active()
                    {
                        // Dropping the poll can notify the authority through its AUTH
                        // guard. Record the cause before cancellation invokes that callback.
                        auth.fail(crate::AuthFailure::Timeout);
                        authentication_timed_out = true;
                        break None;
                    }
                } else if let Some(failure) = failure.or_else(|| {
                    deadline
                        .filter(|deadline| *deadline <= tokio::time::Instant::now())
                        .map(|_| crate::AuthFailure::Timeout)
                }) {
                    let error = shared.contextualize(configuration.connection_error(
                        Error::auth(failure).with_delivery(DeliveryStatus::Ambiguous),
                    ));
                    shared.fail_acknowledgements(&error);
                    fail_pending(&mut senders, &error);
                    return TerminalStatus::Failed(error);
                }
                // Keep parity with the fair and cooperative v4 arbitration above.
                tokio::select! {
                    () = auth.changed.notified() => {},
                    () = auth.callback_changed.notified(), if async_authentication => {},
                    () = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; } else { std::future::pending::<()>().await; } },
                        if !async_authentication || !auth.callback_active() => {},
                    _ = panic_rx.recv_async() => crate::runtime::terminate_driver_for_boundary_panic(),
                    _ = immediate_shutdown_rx.recv_async(), if !connected || async_authentication || shared.has_ordered() => {
                        if !connected || shared.has_ordered() {
                            break None;
                        }
                    },
                    update = configuration_rx.recv_async(), if !configuration.is_preparing() => if let Ok(update) = update {
                        configuration.start(update);
                        tokio::task::yield_now().await;
                    },
                    () = configuration.complete_preparation(&shared), if configuration.is_preparing() => {
                        // Preparation completes without cancelling the native poll.
                        tokio::task::yield_now().await;
                    },
                    registration = completion_rx.recv_async() => if let Ok(registration) = registration {
                        accept_registration(registration, &pending, &mut senders);
                        tokio::task::yield_now().await;
                    },
                    request = diagnostics_rx.recv_async() => if let Ok(request) = request {
                        let mut snapshot = diagnostics.clone();
                        shared.ordered_diagnostics(&mut snapshot);
                        request.resolve(snapshot);
                        tokio::task::yield_now().await;
                    },
                    result = pending.next(), if !pending.is_empty() => if let Some(result) = result {
                        resolve_pending(result, &mut senders);
                        tokio::task::yield_now().await;
                    },
                    () = shared.reconnect.stability() => { shared.reconnect.refresh(); },
                    result = &mut poll => break Some(result),
                }
            }
        };
        // Native attempt success retires origin owners before event delivery can block or terminate the driver.
        configuration.permanent_redirect();
        let polled = if authentication_timed_out {
            let message = "authentication exchange timed out".to_owned();
            eventloop
                .state
                .abort_authentication(rumqttc_v5::AuthError::Failed(message.clone()));
            Some(Err(rumqttc_v5::ConnectionError::MqttState(
                rumqttc_v5::StateError::AuthError(message),
            )))
        } else {
            polled
        };
        let polled = if polled.is_none() {
            // The poll has been dropped, cancelling any pending host future. Initial
            // connection establishment has no poll guard, so notify its authority here.
            eventloop
                .state
                .abort_authentication(rumqttc_v5::AuthError::Failed(
                    "connection closed during authentication".into(),
                ));
            // Destruction or the failure notification can panic. Preserve the typed
            // authentication failure instead of reporting a successful close.
            auth.snapshot().0.map(|_| {
                Err(rumqttc_v5::ConnectionError::MqttState(
                    rumqttc_v5::StateError::AuthError(
                        "wrapper authentication callback failed".into(),
                    ),
                ))
            })
        } else {
            polled
        };
        // The poll has been dropped and its authentication authority notified.
        // Preserve TLS destructor failures before completing shutdown, even
        // though the cancelled connecting future cannot return its error.
        if let Some(failure) = tls_callbacks.failure() {
            let error = shared.contextualize(configuration.connection_error(
                Error::tls_callback(failure).with_delivery(DeliveryStatus::Ambiguous),
            ));
            shared.fail_acknowledgements(&error);
            fail_pending(&mut senders, &error);
            return TerminalStatus::Failed(error);
        }
        let Some(polled) = polled else {
            // Dropping the poll also destroys pending host handshake work. Its
            // terminal failure must take precedence over a successful cancellation.
            if let Some(failure) = websocket.failure().filter(|failure| !failure.retryable()) {
                let error =
                    shared.contextualize(configuration.connection_error(Error::websocket(failure)));
                shared.fail_acknowledgements(&error);
                fail_pending(&mut senders, &error);
                return TerminalStatus::Failed(error);
            }
            return finish_close(
                &shutdown,
                &mut configuration,
                &diagnostics,
                &mut pending,
                &mut senders,
            )
            .await;
        };
        if recovery_cleanup
            && matches!(
                polled,
                Err(rumqttc_v5::ConnectionError::SessionRecoveryPending)
            )
        {
            let error = Error::new(
                ErrorKind::Protocol,
                "incoming acknowledgement ownership was abandoned with the session",
            )
            .with_delivery(DeliveryStatus::Ambiguous);
            connected = false;
            shared.fail_acknowledgements(&error);
            shared.reconnect.abandoned();
            diagnostics = snapshot_v5(&eventloop);
            shared.notify_progress();
            continue;
        }
        // Hide candidate events overtaken by recovery, while keeping errors on the
        // ordinary terminal-classification and shutdown path before any abandonment.
        let recovery_overtook_poll =
            !recovery_cleanup && !recovery_attempt && shared.recovery_cleanup_pending();
        if recovery_overtook_poll
            && matches!(
                &polled,
                Ok(_) | Err(rumqttc_v5::ConnectionError::SessionRecoveryPending)
            )
        {
            continue;
        }
        shared.notify_progress();
        synchronize_admission_state(&eventloop, &shared);
        if !recovery_overtook_poll && let Some(packet) = eventloop.take_connection_failure_packet()
        {
            if matches!(&packet, rumqttc_v5::Packet::Disconnect(_)) {
                // The connection has ended even if earlier events remain queued.
                // Freeze stability before any delivery can block; the next native
                // polls still own error classification, cleanup and retry timing.
                shared.reconnect.connection_ended();
            }
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
                    mapping,
                    &mut pending_auth,
                )
            };
            if let Some(event) = event
                && !deliver(&delivery, event).await
            {
                return TerminalStatus::Failed(overflow_error());
            }
        }
        if let Some(failure) = auth.snapshot().0 {
            // Poll defers connection cleanup until these events are delivered. Preserve
            // their order, including ordinary packets consumed before the failed AUTH.
            let mut pending_events: Vec<_> = polled.as_ref().ok().cloned().into_iter().collect();
            if !recovery_overtook_poll {
                pending_events.extend(eventloop.state.events.drain(..));
            }
            for event in pending_events {
                if let Some(event) = map_v5_event(
                    &mut eventloop,
                    event,
                    &shared,
                    &mut connected,
                    EventMappingOptions {
                        auth_failure: Some(failure),
                        ..mapping
                    },
                    &mut pending_auth,
                ) && !deliver(&delivery, event).await
                {
                    return TerminalStatus::Failed(overflow_error());
                }
            }
            let error =
                shared.contextualize(configuration.connection_error(
                    Error::auth(failure).with_delivery(DeliveryStatus::Ambiguous),
                ));
            shared.fail_acknowledgements(&error);
            fail_pending(&mut senders, &error);
            return TerminalStatus::Failed(error);
        }
        diagnostics = snapshot_v5(&eventloop);
        match polled {
            Ok(event) => {
                let was_connected = connected;
                if let Some(event) = map_v5_event(
                    &mut eventloop,
                    event,
                    &shared,
                    &mut connected,
                    mapping,
                    &mut pending_auth,
                ) {
                    if matches!(&event, WrapperEvent::Connected { .. }) {
                        shared.reconnect.connected();
                    }
                    if was_connected && !connected {
                        shared.reconnect.failed(shared.contextualize(Error::new(
                            ErrorKind::Network,
                            "connection redirected",
                        )));
                    }
                    if let WrapperEvent::Redirect(redirect) = &event {
                        unresolved_redirect = redirect.target.is_none().then(|| redirect.clone());
                    }
                    if matches!(&event, WrapperEvent::Connected { .. })
                        && let Some(redirect) = unresolved_redirect.take()
                    {
                        let resolved = eventloop
                            .take_last_redirect_diagnostics()
                            .unwrap_or_else(|| eventloop.diagnostics().redirect);
                        let redirect = crate::RedirectEvent {
                            target: super::redirect::broker(eventloop.options.broker()),
                            attempts: resolved.attempts,
                            attempt_limit: resolved.attempt_limit,
                            visited_endpoints: resolved.visited_endpoints,
                            srv_candidate_index: resolved.srv_candidate_index,
                            srv_candidate_count: resolved.srv_candidate_count,
                            ..redirect
                        };
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
                if recovery_attempt && !connected && shared.recovery_shutdown_committed() {
                    // Shutdown can commit after the native last-boundary check but before
                    // begin_connection. Retire that hidden candidate with the same retained
                    // poll/control loop instead of reopening traffic or dropping graceful I/O.
                    recovery_retire = true;
                }
            }
            Err(rumqttc_v5::ConnectionError::RequestsDone) => {
                let graceful = complete_shutdown(
                    &shutdown,
                    &mut configuration,
                    &diagnostics,
                    &mut pending,
                    &mut senders,
                )
                .await;
                return TerminalStatus::Closed { graceful };
            }
            Err(error) => {
                if shared.recovering()
                    && shared.recovery_shutdown_committed()
                    && matches!(&error, rumqttc_v5::ConnectionError::SessionRecoveryInvalid)
                {
                    return finish_close(
                        &shutdown,
                        &mut configuration,
                        &diagnostics,
                        &mut pending,
                        &mut senders,
                    )
                    .await;
                }
                if shared.recovering()
                    && matches!(
                        &error,
                        rumqttc_v5::ConnectionError::SessionRecoveryInvalid
                            | rumqttc_v5::ConnectionError::SessionStateMismatch { .. }
                    )
                {
                    return TerminalStatus::Failed(map_connection_error(&error));
                }
                #[cfg(feature = "ordered-shutdown")]
                if matches!(&error, rumqttc_v5::ConnectionError::OrderedDisconnect(_)) {
                    // The native error can precede wrapper admission commitment (e.g. zero deadline).
                    // It remains the authority: further native polls own pending terminal cleanup.
                    connected = false;
                    native_cleanup = true;
                    continue;
                }
                if let rumqttc_v5::ConnectionError::Redirect(redirect) = &error {
                    let failure = super::redirect::failure(&redirect.failure);
                    let redirect_diagnostics = eventloop
                        .take_last_redirect_diagnostics()
                        .unwrap_or_else(|| eventloop.diagnostics().redirect);
                    let event = super::redirect::event(
                        redirect.outcome.clone(),
                        None,
                        Some(failure),
                        &redirect_diagnostics,
                    );
                    let terminal = shared.contextualize(
                        configuration.connection_error(map_connection_error(&error)),
                    );
                    shared.fail_acknowledgements(&terminal);
                    fail_pending(&mut senders, &terminal);
                    if !recovery_overtook_poll
                        && !deliver(&delivery, WrapperEvent::Redirect(event)).await
                    {
                        return TerminalStatus::Failed(overflow_error());
                    }
                    return TerminalStatus::Failed(terminal);
                }
                let classified = shared.reconnect.classified();
                let eligible = super::reconnect::v5(&error);
                let graceful_disconnect_timed_out =
                    matches!(&error, rumqttc_v5::ConnectionError::DisconnectTimeout);
                let mut error = shared.contextualize(
                    configuration.connection_error(
                        websocket
                            .failure()
                            .map_or_else(|| map_connection_error(&error), Error::websocket),
                    ),
                );
                if classified {
                    let retryable = super::reconnect::mapped(&error, eligible);
                    error = error.with_retryable(retryable);
                }
                // Failed abandonment is terminal even for retryable I/O: teardown
                // and checkpoint clearing must commit before establishment can run.
                if recovery_cleanup
                    || error.kind() == ErrorKind::Persistence
                    || (error.transport_failure().is_some() && !error.retryable())
                    || (error.tls_callback_failure().is_some() && !error.retryable())
                    || (error.websocket_failure().is_some() && !error.retryable())
                    || (classified && !error.retryable() && !graceful_disconnect_timed_out)
                {
                    shared.fail_acknowledgements(&error);
                    fail_pending(&mut senders, &error);
                    return TerminalStatus::Failed(error);
                }
                if graceful_disconnect_timed_out && shared.timeout_graceful_shutdown(error.clone())
                {
                    return finish_close(
                        &shutdown,
                        &mut configuration,
                        &diagnostics,
                        &mut pending,
                        &mut senders,
                    )
                    .await;
                }
                match shared.poll_error_action() {
                    PollErrorAction::CompleteImmediateClose => {
                        return finish_close(
                            &shutdown,
                            &mut configuration,
                            &diagnostics,
                            &mut pending,
                            &mut senders,
                        )
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
                shared.reconnect.failed(error.clone());
                if recovery_overtook_poll {
                    shared.notify_progress();
                    continue;
                }
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

pub(super) const fn connack_reason(reason: rumqttc_v5::ConnectReturnCode) -> u8 {
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
        // Keep an empty bag for MQTT 5 so consumers can distinguish it from v4.
        v5_properties: Some(Box::new(connack.properties.map_or_else(
            crate::V5ConnAckProperties::default,
            |p| crate::V5ConnAckProperties {
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
            },
        ))),
    }
}

#[derive(Clone, Copy)]
struct EventMappingOptions {
    emit_outgoing: bool,
    manual_ack: bool,
    protocol: ProtocolVersion,
    auth_failure: Option<crate::AuthFailure>,
}

fn map_auth_event(
    event: rumqttc_v5::AuthEvent,
    auth_failure: Option<crate::AuthFailure>,
    pending_auth: &mut Option<(u8, Option<crate::AuthProperties>)>,
) -> crate::AuthEvent {
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
            let failure = auth_failure.unwrap_or(match reason {
                rumqttc_v5::AuthFailureReason::BrokerRejected(_)
                | rumqttc_v5::AuthFailureReason::BrokerDisconnected(_) => {
                    crate::AuthFailure::BrokerRejected
                }
                rumqttc_v5::AuthFailureReason::OverlappingReauth => crate::AuthFailure::Overlapping,
                rumqttc_v5::AuthFailureReason::MissingAuthenticationMethod => {
                    crate::AuthFailure::Method
                }
                rumqttc_v5::AuthFailureReason::AuthenticationFailed(_) => {
                    crate::AuthFailure::Rejected
                }
                rumqttc_v5::AuthFailureReason::ProtocolError => crate::AuthFailure::InvalidResponse,
                _ => crate::AuthFailure::ConnectionClosed,
            });
            (kind, method, crate::AuthStage::Failed, Some(failure))
        }
    };
    if matches!(stage, crate::AuthStage::Started | crate::AuthStage::Failed) {
        *pending_auth = None;
    }
    let (reason_code, properties) = if matches!(
        stage,
        crate::AuthStage::Continue | crate::AuthStage::Succeeded
    ) {
        pending_auth
            .take()
            .map_or((None, None), |(reason, properties)| {
                (Some(reason), properties)
            })
    } else {
        (None, None)
    };
    crate::AuthEvent {
        exchange: match kind {
            rumqttc_v5::AuthExchangeKind::InitialConnect => crate::AuthExchange::Initial,
            rumqttc_v5::AuthExchangeKind::Reauthentication => crate::AuthExchange::Reauthentication,
        },
        method,
        stage,
        failure,
        reason_code,
        properties,
    }
}

fn map_v5_event(
    eventloop: &mut rumqttc_v5::EventLoop,
    event: rumqttc_v5::Event,
    shared: &Shared,
    connected: &mut bool,
    mapping: EventMappingOptions,
    pending_auth: &mut Option<(u8, Option<crate::AuthProperties>)>,
) -> Option<WrapperEvent> {
    match event {
        rumqttc_v5::Event::Incoming(rumqttc_v5::Packet::Auth(auth)) => {
            let reason = match auth.code {
                rumqttc_v5::AuthReasonCode::Success => 0,
                rumqttc_v5::AuthReasonCode::Continue => 0x18,
                rumqttc_v5::AuthReasonCode::ReAuthenticate => 0x19,
            };
            *pending_auth = Some((reason, auth.properties.map(super::auth::from_properties)));
            None
        }
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
                outcome, target, None, &redirect,
            )))
        }
        rumqttc_v5::Event::Auth(event) => Some(WrapperEvent::Authentication(map_auth_event(
            event,
            mapping.auth_failure,
            pending_auth,
        ))),
        rumqttc_v5::Event::Incoming(rumqttc_v5::Packet::ConnAck(connack)) => {
            if connack.code != rumqttc_v5::ConnectReturnCode::Success {
                return Some(WrapperEvent::ConnectionRejected(connack_details(connack)));
            }
            *pending_auth = None;
            if let Some(properties) = connack.properties.as_ref()
                && properties.authentication_method.is_some()
            {
                *pending_auth = Some((
                    0,
                    Some(crate::AuthProperties {
                        method: properties.authentication_method.clone(),
                        data: properties.authentication_data.clone(),
                        reason_string: properties.reason_string.clone(),
                        user_properties: properties.user_properties.clone(),
                    }),
                ));
            }
            if !shared.begin_connection(
                mapping.protocol,
                connack.session_present,
                connack.properties.as_ref().and_then(|p| p.max_packet_size),
                || {
                    eventloop.discard_pending_manual_acknowledgements();
                },
            ) {
                return None;
            }
            *connected = true;
            Some(WrapperEvent::Connected {
                protocol: mapping.protocol,
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
            Some(map_incoming_publish(publish, shared, mapping.manual_ack))
        }
        rumqttc_v5::Event::Outgoing(outgoing) => {
            map_outgoing_event(&outgoing, shared, mapping.emit_outgoing)
        }
        rumqttc_v5::Event::Incoming(_) => None,
    }
}

fn map_incoming_publish(
    publish: rumqttc_v5::Publish,
    shared: &Shared,
    manual_ack: bool,
) -> WrapperEvent {
    let ack_token = if manual_ack {
        match publish.qos {
            rumqttc_v5::QoS::AtLeastOnce | rumqttc_v5::QoS::ExactlyOnce => shared
                .backend()
                .prepare_v5_ack(&publish)
                .and_then(|ack| shared.prepare_ack(ack)),
            rumqttc_v5::QoS::AtMostOnce => None,
        }
    } else {
        None
    };
    WrapperEvent::IncomingPublish(Box::new(IncomingPublish {
        topic: publish.topic,
        payload: publish.payload,
        qos: from_qos(publish.qos),
        retain: publish.retain,
        duplicate: publish.dup,
        ack_token,
        v5_properties: publish.properties.map(from_incoming_publish_properties),
    }))
}

fn map_outgoing_event(
    outgoing: &rumqttc_v5::Outgoing,
    shared: &Shared,
    emit_outgoing: bool,
) -> Option<WrapperEvent> {
    match outgoing {
        rumqttc_v5::Outgoing::PubAck(packet_id) => {
            shared.complete_v5_puback(*packet_id);
        }
        rumqttc_v5::Outgoing::PubRec(packet_id) => {
            shared.complete_v5_pubrec(*packet_id);
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
            | rumqttc_v5::Outgoing::AwaitAck(id) => (*id != 0).then_some(*id),
            _ => None,
        };
        WrapperEvent::Outgoing(crate::OutgoingEvent {
            activity: map_outgoing(outgoing),
            packet_id,
        })
    })
}

fn synchronize_admission_state(eventloop: &rumqttc_v5::EventLoop, shared: &Shared) {
    let options = &eventloop.options;
    shared.set_protocol_admission_state(
        options.session_expiry_interval().unwrap_or(0) == 0,
        options.authenticator().is_some() || options.async_authenticator().is_some(),
    );
}

fn snapshot_v5(eventloop: &rumqttc_v5::EventLoop) -> DiagnosticsSnapshot {
    let diagnostics = eventloop.diagnostics();
    DiagnosticsSnapshot {
        reconnect: None,
        #[cfg(not(feature = "ordered-shutdown"))]
        ordered_shutdown: None,
        #[cfg(feature = "ordered-shutdown")]
        ordered_shutdown: diagnostics.disconnect_fence_sequence.map(|sequence| {
            let captured_at = std::time::Instant::now();
            Box::new(crate::OrderedShutdownDiagnostics {
                phase: match diagnostics.shutdown_phase {
                    rumqttc_v5::ShutdownPhase::Open => crate::OrderedShutdownPhase::Open,
                    rumqttc_v5::ShutdownPhase::AdmittedDrain => {
                        crate::OrderedShutdownPhase::AdmittedDrain
                    }
                    rumqttc_v5::ShutdownPhase::Approaching => {
                        crate::OrderedShutdownPhase::Approaching
                    }
                    rumqttc_v5::ShutdownPhase::Draining => crate::OrderedShutdownPhase::Draining,
                    rumqttc_v5::ShutdownPhase::Flushing => crate::OrderedShutdownPhase::Flushing,
                    rumqttc_v5::ShutdownPhase::Completed => crate::OrderedShutdownPhase::Completed,
                    rumqttc_v5::ShutdownPhase::TimedOut => crate::OrderedShutdownPhase::TimedOut,
                    rumqttc_v5::ShutdownPhase::Failed => crate::OrderedShutdownPhase::Failed,
                },
                fence_sequence: Some(sequence),
                remaining_at_capture: diagnostics
                    .disconnect_deadline
                    .map(|deadline| deadline.saturating_duration_since(captured_at)),
                local_queued_publishes: diagnostics.ordered_local_queued_publishes,
                captured_at,
            })
        }),
        connack: diagnostics
            .session
            .connack
            .map(|session| crate::ConnAckSessionDiagnostics {
                raw_session_present: session.raw_session_present,
                session_resumed: session.session_resumed,
                diagnostic: session.diagnostic.and_then(|diagnostic| match diagnostic {
                    rumqttc_v5::ConnAckDiagnostic::BrokerOnlySessionResume => {
                        Some(crate::ConnAckDiagnostic::BrokerOnlySessionResume)
                    }
                    _ => None,
                }),
            }),
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

#[cfg(test)]
pub fn map_publish_notice(
    result: std::result::Result<rumqttc_v5::PublishResult, rumqttc_v5::PublishNoticeError>,
) -> TerminalOutcome {
    map_publish_outcome(rumqttc_v5::PublishNoticeOutcome {
        result,
        possibly_transmitted: true,
    })
}

pub fn map_publish_outcome(outcome: rumqttc_v5::PublishNoticeOutcome) -> TerminalOutcome {
    use rumqttc_v5::PublishResult as R;
    let result = match outcome.result {
        Ok(result) => result,
        Err(error) => return Err(map_publish_failure(&error, outcome.possibly_transmitted)).into(),
    };
    let (kind, packet_id, reason, properties, recovered, completion) = match result {
        R::Qos0Flushed => return Ok(Completion::Publish(PublishCompletion::Qos0Flushed)).into(),
        R::Qos1(ack) => (
            AcknowledgementKind::PubAck,
            ack.pkid,
            v5_puback_code(ack.reason),
            ack.properties.map(|p| AcknowledgementProperties {
                reason_string: p.reason_string,
                user_properties: p.user_properties,
            }),
            false,
            if v5_puback_success(ack.reason) {
                Ok(Completion::Publish(PublishCompletion::Qos1Acknowledged))
            } else {
                Err(broker_rejection(v5_puback_code(ack.reason)))
            },
        ),
        R::Qos2Completed(ack) => (
            AcknowledgementKind::PubComp,
            ack.pkid,
            v5_pubcomp_code(ack.reason),
            ack.properties.map(|p| AcknowledgementProperties {
                reason_string: p.reason_string,
                user_properties: p.user_properties,
            }),
            false,
            if v5_pubcomp_success(ack.reason) {
                Ok(Completion::Publish(PublishCompletion::Qos2Completed))
            } else {
                Err(broker_rejection(v5_pubcomp_code(ack.reason)))
            },
        ),
        R::Qos2Recovered(ack) => (
            AcknowledgementKind::PubComp,
            ack.pkid,
            v5_pubcomp_code(ack.reason),
            ack.properties.map(|p| AcknowledgementProperties {
                reason_string: p.reason_string,
                user_properties: p.user_properties,
            }),
            true,
            Ok(Completion::Publish(PublishCompletion::Qos2Completed)),
        ),
        R::Qos2PubRecRejected(ack) => (
            AcknowledgementKind::PubRec,
            ack.pkid,
            v5_pubrec_code(ack.reason),
            ack.properties.map(|p| AcknowledgementProperties {
                reason_string: p.reason_string,
                user_properties: p.user_properties,
            }),
            false,
            Err(broker_rejection(v5_pubrec_code(ack.reason))),
        ),
    };
    TerminalOutcome::with_acknowledgement(
        completion,
        BrokerAcknowledgement::new(
            ProtocolVersion::V5,
            kind,
            packet_id,
            Some(reason),
            None,
            properties,
            recovered,
        ),
    )
}

pub fn map_subscribe_notice(
    result: std::result::Result<rumqttc_v5::SubAck, rumqttc_v5::SubscribeNoticeError>,
) -> TerminalOutcome {
    let ack = match result {
        Ok(ack) => ack,
        Err(error) => return Err(map_notice_error(error)).into(),
    };
    let codes = ack
        .return_codes
        .iter()
        .copied()
        .map(v5_suback_code)
        .collect();
    let completion = Completion::Subscribe(SubscribeCompletion {
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
    });
    TerminalOutcome::with_acknowledgement(
        Ok(completion),
        BrokerAcknowledgement::new(
            ProtocolVersion::V5,
            AcknowledgementKind::SubAck,
            ack.pkid,
            None,
            Some(codes),
            ack.properties.map(|p| AcknowledgementProperties {
                reason_string: p.reason_string,
                user_properties: p.user_properties,
            }),
            false,
        ),
    )
}

pub fn map_unsubscribe_notice(
    result: std::result::Result<rumqttc_v5::UnsubAck, rumqttc_v5::UnsubscribeNoticeError>,
) -> TerminalOutcome {
    let ack = match result {
        Ok(ack) => ack,
        Err(error) => return Err(map_notice_error(error)).into(),
    };
    let codes = ack.reasons.iter().map(|reason| *reason as u8).collect();
    let completion = Completion::Unsubscribe(UnsubscribeCompletion {
        results: Some(
            ack.reasons
                .into_iter()
                .map(|reason| match reason {
                    rumqttc_v5::UnsubAckReason::Success => UnsubscribeResult::Success,
                    rumqttc_v5::UnsubAckReason::NoSubscriptionExisted => {
                        UnsubscribeResult::NoSubscriptionExisted
                    }
                    reason => UnsubscribeResult::Rejected(BrokerReason { code: reason as u8 }),
                })
                .collect(),
        ),
    });
    TerminalOutcome::with_acknowledgement(
        Ok(completion),
        BrokerAcknowledgement::new(
            ProtocolVersion::V5,
            AcknowledgementKind::UnsubAck,
            ack.pkid,
            None,
            Some(codes),
            ack.properties.map(|p| AcknowledgementProperties {
                reason_string: p.reason_string,
                user_properties: p.user_properties,
            }),
            false,
        ),
    )
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

fn map_publish_failure(
    error: &rumqttc_v5::PublishNoticeError,
    possibly_transmitted: bool,
) -> Error {
    use crate::{ErrorCode as C, PublishFailure as F};
    use rumqttc_v5::PublishNoticeError as E;
    let (failure, code, kind) = match error {
        E::RetainNotSupported => (
            F::RetainUnavailable,
            C::PublishRejected,
            ErrorKind::Protocol,
        ),
        E::QoSNotSupported { .. } => (F::MaximumQos, C::PublishRejected, ErrorKind::Protocol),
        E::TopicAliasInvalid { .. } => (
            F::TopicAliasMaximum,
            C::PublishRejected,
            ErrorKind::Protocol,
        ),
        E::TopicAliasMappingUnavailable(_) => (
            F::TopicAliasUnmapped,
            C::PublishRejected,
            ErrorKind::Protocol,
        ),
        E::TopicAliasReplayUnavailable(_) => (
            F::TopicAliasReplayUnavailable,
            C::PublishAliasReplayUnavailable,
            ErrorKind::Protocol,
        ),
        E::SessionReset => (F::SessionReset, C::PublishSessionReset, ErrorKind::Protocol),
        E::Redirected => (F::Redirected, C::PublishRejected, ErrorKind::Protocol),
        E::BrokerOnlySessionResume => (
            F::BrokerOnlySessionResume,
            C::PublishRejected,
            ErrorKind::Protocol,
        ),
        E::Qos0NotFlushed => (F::Qos0NotFlushed, C::Protocol, ErrorKind::Protocol),
        E::SessionPersistence(_) => (F::Persistence, C::Persistence, ErrorKind::Persistence),
        _ => (F::ReceiverTerminated, C::Protocol, ErrorKind::Protocol),
    };
    let delivery = if possibly_transmitted || matches!(failure, F::ReceiverTerminated) {
        DeliveryStatus::Ambiguous
    } else {
        DeliveryStatus::Rejected
    };
    // Known variants contain only local numeric metadata, except persistence diagnostics.
    let message = if matches!(failure, F::Persistence | F::ReceiverTerminated) {
        "MQTT publish failed".to_owned()
    } else {
        error.to_string()
    };
    Error::new(kind, message)
        .with_code(code)
        .with_delivery(delivery)
        .with_publish_failure(failure)
        .with_retryable(failure == F::SessionReset && delivery == DeliveryStatus::Rejected)
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

#[cfg(feature = "ordered-shutdown")]
pub(super) fn map_ordered_error(error: rumqttc_v5::DisconnectNoticeError) -> Error {
    use crate::OrderedDisconnectFailure as F;
    use rumqttc_v5::DisconnectNoticeError as E;
    let (failure, error) = match error {
        E::DisconnectTimeout => (
            F::Timeout,
            Error::new(ErrorKind::Timeout, "ordered shutdown deadline expired"),
        ),
        E::Transport(source) => (F::Transport, map_connection_error(&source)),
        E::Protocol(source) => (F::Protocol, map_connection_error(&source)),
        E::Persistence(source) => (F::Persistence, map_connection_error(&source)),
        E::Publish(source) => return map_ordered_publish_error(&source),
        E::SupersededByImmediate => (
            F::SupersededByImmediate,
            Error::new(
                ErrorKind::Shutdown,
                "ordered shutdown superseded by immediate close",
            ),
        ),
        E::Superseded => (
            F::Superseded,
            Error::new(ErrorKind::Shutdown, "ordered shutdown superseded"),
        ),
        E::ReceiverTerminated => (
            F::ReceiverTerminated,
            Error::new(ErrorKind::Shutdown, "ordered shutdown execution terminated"),
        ),
        E::SessionReset => (
            F::SessionReset,
            Error::new(ErrorKind::Shutdown, "ordered shutdown session reset"),
        ),
        E::Redirected => (
            F::Redirected,
            Error::new(
                ErrorKind::Shutdown,
                "ordered shutdown cannot cross a redirect",
            ),
        ),
        E::ReplayUnavailable => (
            F::ReplayUnavailable,
            Error::new(ErrorKind::Shutdown, "ordered shutdown replay unavailable"),
        ),
        _ => (
            F::Protocol,
            Error::new(ErrorKind::Protocol, "ordered shutdown failed"),
        ),
    };
    error
        .with_ordered_failure(failure)
        .with_delivery(DeliveryStatus::Ambiguous)
}

#[cfg(feature = "ordered-shutdown")]
fn map_ordered_publish_error(error: &rumqttc_v5::PublishNoticeError) -> Error {
    use crate::OrderedDisconnectFailure as F;
    use rumqttc_v5::PublishNoticeError as E;
    let failure = match error {
        E::SessionReset => F::SessionReset,
        E::ShutdownSupersededByImmediate => F::SupersededByImmediate,
        E::Redirected => F::Redirected,
        E::BrokerOnlySessionResume | E::TopicAliasReplayUnavailable(_) => F::ReplayUnavailable,
        _ => F::Publish,
    };
    let mapped = match error {
        E::V5PubAck(reason) => broker_rejection(v5_puback_code(*reason)),
        E::V5PubRec(reason) => broker_rejection(v5_pubrec_code(*reason)),
        E::V5PubComp(reason) => broker_rejection(v5_pubcomp_code(*reason)),
        E::SessionPersistence(_) => Error::new(
            ErrorKind::Persistence,
            "preceding publish persistence failed",
        )
        .with_delivery(DeliveryStatus::Ambiguous),
        _ => Error::new(ErrorKind::Protocol, "preceding publish did not complete")
            .with_delivery(DeliveryStatus::Ambiguous),
    };
    mapped
        .with_ordered_failure(failure)
        .with_delivery(DeliveryStatus::Ambiguous)
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[cfg(feature = "ordered-shutdown")]
    #[test]
    fn ordered_failures_preserve_typed_reasons_and_redact_source_text() {
        use crate::OrderedDisconnectFailure as F;
        use rumqttc_v5::DisconnectNoticeError as E;
        let source = std::sync::Arc::new(rumqttc_v5::ConnectionError::Io(std::io::Error::other(
            "private host source",
        )));
        for (native, expected) in [
            (E::DisconnectTimeout, F::Timeout),
            (E::Transport(source.clone()), F::Transport),
            (E::Protocol(source.clone()), F::Protocol),
            (E::Persistence(source), F::Persistence),
            (
                E::Publish(rumqttc_v5::PublishNoticeError::SessionPersistence(
                    "private host source".into(),
                )),
                F::Publish,
            ),
            (E::SupersededByImmediate, F::SupersededByImmediate),
            (E::Superseded, F::Superseded),
            (E::ReceiverTerminated, F::ReceiverTerminated),
            (E::SessionReset, F::SessionReset),
            (E::Redirected, F::Redirected),
            (E::ReplayUnavailable, F::ReplayUnavailable),
            (
                E::Publish(rumqttc_v5::PublishNoticeError::SessionReset),
                F::SessionReset,
            ),
        ] {
            let mapped = map_ordered_error(native);
            assert_eq!(mapped.ordered_disconnect_failure(), Some(expected));
            assert_eq!(mapped.delivery_status(), DeliveryStatus::Ambiguous);
            assert!(!mapped.retryable());
            assert!(!format!("{mapped:?} {mapped}").contains("private host source"));
        }
    }

    #[test]
    fn unrecoverable_alias_notice_retains_the_native_terminal_reason() {
        // Managed producer admission prevents unknown alias-only publishes.
        // Keep the defensive native replay failure observable if a restored or
        // legacy request nevertheless reaches this path.
        let outcome = map_publish_notice(Err(
            rumqttc_v5::PublishNoticeError::TopicAliasReplayUnavailable(7),
        ));
        let error = outcome.result().as_ref().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Protocol);
        assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
        assert_eq!(
            error.publish_failure(),
            Some(crate::PublishFailure::TopicAliasReplayUnavailable)
        );
        assert_eq!(
            error.message(),
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
    fn connack_without_properties_still_identifies_mqtt5() {
        let details = connack_details(rumqttc_v5::ConnAck {
            session_present: false,
            code: rumqttc_v5::ConnectReturnCode::Success,
            properties: None,
        });
        assert_eq!(
            details.v5_properties.as_deref(),
            Some(&crate::V5ConnAckProperties::default())
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
            &std::sync::Arc::default(),
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
