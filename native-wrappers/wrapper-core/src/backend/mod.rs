#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub mod tls;
#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub use tls::build_tls;

mod auth;
mod reconnect;
mod redirect;
pub mod session;
mod transport;
pub use transport::{io_error as transport_io_error, io_failure as transport_io_failure};
pub mod v4;
pub mod v5;

#[cfg(feature = "websocket")]
pub use rumqttc_v4::WebSocketRequestContext;

use std::time::Duration;

use crate::operations::CompletionFuture;
use crate::validation::protocol_option_error;
use crate::{
    ClientConfig, Error, ErrorKind, ProtocolConfig, PublishCommand, PublishProtocolOptions, Result,
    SubscribeCommand, SubscribeProtocolOptions, SubscriptionProtocolOptions, UnsubscribeCommand,
    UnsubscribeProtocolOptions,
};

/// Client-owned failures from destroying host TLS work. These are terminal,
/// so they remain visible through poll or driver cancellation and are never reset.
/// Each client owns its monitor even when TLS profiles are shared.
#[derive(Default)]
pub struct TlsCallbackMonitor(parking_lot::Mutex<Option<crate::TlsCallbackFailure>>);
impl TlsCallbackMonitor {
    pub fn failure(&self) -> Option<crate::TlsCallbackFailure> {
        *self.0.lock()
    }
    #[cfg(feature = "use-rustls-no-provider")]
    pub fn fail(&self, failure: crate::TlsCallbackFailure) {
        self.0.lock().get_or_insert(failure);
    }
}

pub enum BackendClient {
    V4(rumqttc_v4::AsyncClient),
    V5(rumqttc_v5::AsyncClient),
}

pub enum BackendDriver {
    V4(Box<v4::Driver>),
    V5(Box<v5::Driver>),
}

#[derive(Clone)]
pub enum PreparedAck {
    V4(rumqttc_v4::ManualAck),
    V5(rumqttc_v5::ManualAck),
}

impl PreparedAck {
    pub(crate) const fn key(&self) -> AckKey {
        match self {
            Self::V4(rumqttc_v4::ManualAck::PubAck(ack)) => AckKey::V4PubAck(ack.pkid),
            Self::V4(rumqttc_v4::ManualAck::PubRec(ack)) => AckKey::V4PubRec(ack.pkid),
            Self::V5(rumqttc_v5::ManualAck::PubAck(ack)) => AckKey::V5PubAck(ack.pkid),
            Self::V5(rumqttc_v5::ManualAck::PubRec(ack)) => AckKey::V5PubRec(ack.pkid),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AckKey {
    V4PubAck(u16),
    V4PubRec(u16),
    V5PubAck(u16),
    V5PubRec(u16),
}

impl BackendClient {
    pub(crate) fn try_reauthenticate(
        &self,
        properties: Option<&crate::AuthProperties>,
    ) -> Result<CompletionFuture> {
        let Self::V5(client) = self else {
            return Err(protocol_option_error("reauthentication requires MQTT 5"));
        };
        if properties.is_some() {
            return Err(protocol_option_error(
                "reauthentication properties must come from the configured authenticator",
            ));
        }
        let notice = client
            .try_reauth_tracked(None)
            .map_err(|error| v5::map_client_error(&error))?;
        Ok(Box::pin(async move {
            notice
                .wait_async()
                .await
                .map(|_| crate::Completion::Authenticated)
                .map_err(|error| {
                    if let rumqttc_v5::AuthNoticeError::BrokerRejected(reason) = error {
                        return Error::auth(crate::AuthFailure::BrokerRejected)
                            .with_broker_reason(v5::connack_reason(reason))
                            .with_delivery(crate::DeliveryStatus::Rejected);
                    }
                    if let rumqttc_v5::AuthNoticeError::BrokerDisconnected(reason) = error {
                        return Error::auth(crate::AuthFailure::BrokerRejected)
                            .with_broker_reason(reason as u8)
                            .with_delivery(crate::DeliveryStatus::Rejected);
                    }
                    let failure = match error {
                        rumqttc_v5::AuthNoticeError::OverlappingReauth => {
                            crate::AuthFailure::Overlapping
                        }
                        rumqttc_v5::AuthNoticeError::MissingAuthenticationMethod => {
                            crate::AuthFailure::Method
                        }
                        rumqttc_v5::AuthNoticeError::AuthenticationFailed(_) => {
                            crate::AuthFailure::Rejected
                        }
                        rumqttc_v5::AuthNoticeError::ProtocolError => {
                            crate::AuthFailure::InvalidResponse
                        }
                        _ => crate::AuthFailure::ConnectionClosed,
                    };
                    Error::auth(failure).with_delivery(crate::DeliveryStatus::Ambiguous)
                })
                .into()
        }))
    }
    pub(crate) fn publish_waiter(&self) -> Option<rumqttc_v5::PublishAdmissionWaiter> {
        match self {
            Self::V4(_) => None,
            Self::V5(client) => client.publish_admission_waiter(),
        }
    }

    pub(crate) fn publish_budget_snapshot(&self) -> Result<crate::PublishBudgetSnapshot> {
        let Self::V5(client) = self else {
            return Err(protocol_option_error("publish budget requires MQTT 5"));
        };
        let snapshot = client
            .publish_budget_snapshot()
            .expect("wrapper MQTT 5 clients always have a budget");
        Ok(crate::PublishBudgetSnapshot {
            outstanding: snapshot.outstanding,
            retained_bytes: snapshot.retained_bytes,
            limits: crate::PublishBudgetLimits {
                max_outstanding: snapshot.limits.max_outstanding,
                max_bytes: snapshot.limits.max_bytes,
            },
            recovery_pending: snapshot.recovery_pending,
        })
    }

    pub(crate) fn try_publish(&self, command: PublishCommand) -> Result<CompletionFuture> {
        match self {
            Self::V4(client) => {
                if matches!(command.protocol, PublishProtocolOptions::V5(_)) {
                    return Err(protocol_option_error(
                        "MQTT 5 publish properties require MQTT 5",
                    ));
                }
                let options = v4::publish_options(&command);
                let notice = client
                    .try_publish_tracked(command.topic, command.payload, options)
                    .map_err(|error| v4::map_client_error(&error))?;
                Ok(Box::pin(async move {
                    v4::map_publish_notice(notice.wait_async().await)
                }))
            }
            Self::V5(client) => {
                v5::validate_publish(&command)?;
                let options = v5::publish_options(&command);
                let notice = client
                    .try_publish_tracked(command.topic, command.payload, options)
                    .map_err(|error| v5::map_client_error(&error))?;
                Ok(Box::pin(async move {
                    v5::map_publish_outcome(notice.wait_outcome_async().await)
                }))
            }
        }
    }

    pub(crate) fn try_subscribe(&self, command: SubscribeCommand) -> Result<CompletionFuture> {
        match self {
            Self::V4(client) => {
                if matches!(command.protocol, SubscribeProtocolOptions::V5(_))
                    || command
                        .filters
                        .iter()
                        .any(|filter| matches!(filter.protocol, SubscriptionProtocolOptions::V5(_)))
                {
                    return Err(protocol_option_error(
                        "MQTT 5 subscribe options require MQTT 5",
                    ));
                }
                let filters = command
                    .filters
                    .into_iter()
                    .map(|filter| {
                        rumqttc_v4::SubscribeFilterInput::new(filter.filter, v4::to_qos(filter.qos))
                    })
                    .collect::<Vec<_>>();
                let notice = client
                    .try_subscribe_many_tracked(filters)
                    .map_err(|error| v4::map_client_error(&error))?;
                Ok(Box::pin(async move {
                    v4::map_subscribe_notice(notice.wait_async().await)
                }))
            }
            Self::V5(client) => {
                v5::validate_subscribe(&command)?;
                let properties = match command.protocol {
                    SubscribeProtocolOptions::VersionNeutral => None,
                    SubscribeProtocolOptions::V5(properties) => {
                        Some(v5::to_subscribe_properties(properties))
                    }
                };
                let filters = command
                    .filters
                    .into_iter()
                    .map(|filter| {
                        let input = rumqttc_v5::SubscribeFilterInput::new(
                            filter.filter,
                            v5::to_qos(filter.qos),
                        );
                        match filter.protocol {
                            SubscriptionProtocolOptions::VersionNeutral => input,
                            SubscriptionProtocolOptions::V5(options) => input
                                .no_local(options.no_local)
                                .preserve_retain(options.retain_as_published)
                                .retain_forward_rule(v5::to_retain_forward_rule(
                                    options.retain_forward_rule,
                                )),
                        }
                    })
                    .collect::<Vec<_>>();
                let notice = if let Some(properties) = properties {
                    client.try_subscribe_many_with_properties_tracked(filters, properties)
                } else {
                    client.try_subscribe_many_tracked(filters)
                }
                .map_err(|error| v5::map_client_error(&error))?;
                Ok(Box::pin(async move {
                    v5::map_subscribe_notice(notice.wait_async().await)
                }))
            }
        }
    }

    pub(crate) fn try_unsubscribe(&self, command: UnsubscribeCommand) -> Result<CompletionFuture> {
        match self {
            Self::V4(client) => {
                if matches!(command.protocol, UnsubscribeProtocolOptions::V5(_)) {
                    return Err(protocol_option_error(
                        "MQTT 5 unsubscribe properties require MQTT 5",
                    ));
                }
                let notice = client
                    .try_unsubscribe_many_tracked(command.filters)
                    .map_err(|error| v4::map_client_error(&error))?;
                Ok(Box::pin(async move {
                    v4::map_unsubscribe_notice(notice.wait_async().await)
                }))
            }
            Self::V5(client) => {
                v5::validate_unsubscribe(&command)?;
                let notice = match command.protocol {
                    UnsubscribeProtocolOptions::VersionNeutral => {
                        client.try_unsubscribe_many_tracked(command.filters)
                    }
                    UnsubscribeProtocolOptions::V5(properties) => client
                        .try_unsubscribe_many_with_properties_tracked(
                            command.filters,
                            v5::to_unsubscribe_properties(properties),
                        ),
                }
                .map_err(|error| v5::map_client_error(&error))?;
                Ok(Box::pin(async move {
                    v5::map_unsubscribe_notice(notice.wait_async().await)
                }))
            }
        }
    }

    pub(crate) fn prepare_v4_ack(&self, publish: &rumqttc_v4::Publish) -> Option<PreparedAck> {
        let Self::V4(client) = self else {
            return None;
        };
        client.prepare_ack(publish).map(PreparedAck::V4)
    }

    pub(crate) fn prepare_v5_ack(&self, publish: &rumqttc_v5::Publish) -> Option<PreparedAck> {
        let Self::V5(client) = self else {
            return None;
        };
        client.prepare_ack(publish).map(PreparedAck::V5)
    }

    pub(crate) fn try_manual_ack(&self, ack: &PreparedAck) -> Result<()> {
        match (self, ack) {
            (Self::V4(client), PreparedAck::V4(ack)) => client
                .try_manual_ack(ack.clone())
                .map_err(|error| v4::map_client_error(&error)),
            (Self::V5(client), PreparedAck::V5(ack)) => client
                .try_manual_ack(ack.clone())
                .map_err(|error| v5::map_client_error(&error)),
            _ => Err(Error::new(
                ErrorKind::Internal,
                "acknowledgement protocol mismatch",
            )),
        }
    }

    pub(crate) fn try_ordered_disconnect(
        &self,
        timeout: Option<Duration>,
        protocol: &crate::DisconnectProtocolOptions,
    ) -> Result<crate::ordered::OrderedAdmission> {
        #[cfg(not(feature = "ordered-shutdown"))]
        {
            let _ = (self, timeout, protocol);
            Err(Error::configuration("ordered-shutdown feature is disabled"))
        }
        #[cfg(feature = "ordered-shutdown")]
        match self {
            Self::V4(client) => {
                if !matches!(protocol, crate::DisconnectProtocolOptions::VersionNeutral) {
                    return Err(protocol_option_error(
                        "MQTT 5 disconnect options require MQTT 5",
                    ));
                }
                let notice = timeout
                    .map_or_else(
                        || client.try_disconnect_after_queued(),
                        |timeout| client.try_disconnect_after_queued_with_timeout(timeout),
                    )
                    .map_err(|error| v4::map_client_error(&error))?;
                Ok(crate::ordered::OrderedAdmission {
                    sequence: notice
                        .fence_sequence()
                        .expect("returned native notice is admitted"),
                    deadline: notice.deadline(),
                    notice: Box::pin(async move {
                        notice.wait_async().await.map_err(v4::map_ordered_error)
                    }),
                })
            }
            Self::V5(client) => {
                let notice = if let crate::DisconnectProtocolOptions::V5(options) = protocol {
                    let (reason, properties) = v5::disconnect_properties(options)?;
                    match timeout {
                        Some(timeout) => client
                            .try_disconnect_after_queued_with_properties_timeout(
                                reason, properties, timeout,
                            ),
                        None => {
                            client.try_disconnect_after_queued_with_properties(reason, properties)
                        }
                    }
                } else {
                    timeout.map_or_else(
                        || client.try_disconnect_after_queued(),
                        |timeout| client.try_disconnect_after_queued_with_timeout(timeout),
                    )
                }
                .map_err(|error| v5::map_client_error(&error))?;
                Ok(crate::ordered::OrderedAdmission {
                    sequence: notice
                        .fence_sequence()
                        .expect("returned native notice is admitted"),
                    deadline: notice.deadline(),
                    notice: Box::pin(async move {
                        notice.wait_async().await.map_err(v5::map_ordered_error)
                    }),
                })
            }
        }
    }

    pub(crate) fn try_disconnect(
        &self,
        timeout: Option<Duration>,
        protocol: &crate::DisconnectProtocolOptions,
    ) -> Result<()> {
        if let crate::DisconnectProtocolOptions::V5(properties) = protocol {
            let Self::V5(client) = self else {
                return Err(protocol_option_error(
                    "MQTT 5 disconnect options require MQTT 5",
                ));
            };
            let (reason, properties) = v5::disconnect_properties(properties)?;
            return match timeout {
                Some(timeout) => {
                    client.try_disconnect_with_properties_timeout(reason, properties, timeout)
                }
                None => client.try_disconnect_with_properties(reason, properties),
            }
            .map_err(|error| v5::map_client_error(&error));
        }
        match self {
            Self::V4(client) => timeout
                .map_or_else(
                    || client.try_disconnect(),
                    |timeout| client.try_disconnect_with_timeout(timeout),
                )
                .map_err(|error| v4::map_client_error(&error)),
            Self::V5(client) => timeout
                .map_or_else(
                    || client.try_disconnect(),
                    |timeout| client.try_disconnect_with_timeout(timeout),
                )
                .map_err(|error| v5::map_client_error(&error)),
        }
    }

    pub(crate) fn try_disconnect_now(
        &self,
        protocol: &crate::DisconnectProtocolOptions,
    ) -> Result<()> {
        if let crate::DisconnectProtocolOptions::V5(properties) = protocol {
            let Self::V5(client) = self else {
                return Err(protocol_option_error(
                    "MQTT 5 disconnect options require MQTT 5",
                ));
            };
            let (reason, properties) = v5::disconnect_properties(properties)?;
            return client
                .try_disconnect_now_with_properties(reason, properties)
                .map_err(|error| v5::map_client_error(&error));
        }
        match self {
            Self::V4(client) => client
                .try_disconnect_now()
                .map_err(|error| v4::map_client_error(&error)),
            Self::V5(client) => client
                .try_disconnect_now()
                .map_err(|error| v5::map_client_error(&error)),
        }
    }

    pub(crate) fn best_effort_disconnect_now(&self, protocol: &crate::DisconnectProtocolOptions) {
        _ = self.try_disconnect_now(protocol);
    }
}

impl BackendDriver {
    pub(crate) fn tls_callback_monitor(&self) -> std::sync::Arc<TlsCallbackMonitor> {
        match self {
            Self::V4(driver) => std::sync::Arc::clone(&driver.tls_callbacks),
            Self::V5(driver) => std::sync::Arc::clone(&driver.tls_callbacks),
        }
    }

    pub(crate) async fn run(
        self,
        context: crate::runtime::DriverContext,
    ) -> crate::runtime::TerminalStatus {
        match self {
            Self::V4(eventloop) => v4::run(eventloop, context).await,
            Self::V5(eventloop) => v5::run(eventloop, context).await,
        }
    }
}

pub fn build(config: ClientConfig) -> Result<(BackendClient, BackendDriver)> {
    let ClientConfig { common, protocol } = config;
    match protocol {
        ProtocolConfig::V4(protocol) => {
            let (client, eventloop) = v4::build(&common, protocol)?;
            Ok((BackendClient::V4(client), BackendDriver::V4(eventloop)))
        }
        ProtocolConfig::V5(protocol) => {
            let (client, eventloop) = v5::build(&common, protocol)?;
            Ok((BackendClient::V5(client), BackendDriver::V5(eventloop)))
        }
    }
}

fn build_network(common: &crate::CommonConfig) -> rumqttc_v4::NetworkOptions {
    let config = &common.network;
    let mut network = rumqttc_v4::NetworkOptions::new();
    network.set_connection_timeout(common.connection_timeout.as_secs());
    network.set_tcp_nodelay(config.tcp_nodelay);
    if let Some(size) = config.tcp_send_buffer_size {
        network.set_tcp_send_buffer_size(size);
    }
    if let Some(size) = config.tcp_receive_buffer_size {
        network.set_tcp_recv_buffer_size(size);
    }
    if let Some(address) = config.local_address {
        network.set_bind_addr(address);
    }
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "fuchsia"))]
    if let Some(device) = &config.bind_device {
        network.set_bind_device(device);
    }
    #[cfg(target_os = "linux")]
    network.set_mptcp(config.mptcp);
    network
}

#[cfg(any(feature = "http-proxy", feature = "socks-proxy"))]
fn build_proxy(
    config: &crate::ProxyConfig,
    tls_callbacks: &std::sync::Arc<TlsCallbackMonitor>,
) -> Result<rumqttc_v4::Proxy> {
    #[cfg(not(all(
        feature = "http-proxy",
        any(feature = "use-rustls-no-provider", feature = "use-native-tls")
    )))]
    let _ = tls_callbacks;
    let (proxy, credentials) = match config {
        #[cfg(feature = "http-proxy")]
        crate::ProxyConfig::Http {
            host,
            port,
            credentials,
            tls: None,
        } => (rumqttc_v4::Proxy::http(host.clone(), *port), credentials),
        #[cfg(all(
            feature = "http-proxy",
            any(feature = "use-rustls-no-provider", feature = "use-native-tls")
        ))]
        crate::ProxyConfig::Http {
            host,
            port,
            credentials,
            tls: Some(tls),
        } => (
            rumqttc_v4::Proxy::https(
                host.clone(),
                *port,
                build_tls(tls, crate::TlsLayer::Proxy, tls_callbacks)?,
            ),
            credentials,
        ),
        #[cfg(feature = "socks-proxy")]
        crate::ProxyConfig::Socks5 {
            host,
            port,
            credentials,
        } => (rumqttc_v4::Proxy::socks5(host.clone(), *port), credentials),
        #[allow(unreachable_patterns)]
        _ => {
            return Err(Error::configuration(
                "proxy configuration requires disabled features",
            ));
        }
    };
    Ok(match credentials {
        Some(c) => proxy.with_credentials(c.username.clone(), c.password.clone()),
        None => proxy,
    })
}

#[cfg(test)]
pub const fn test_v4_puback(packet_id: u16) -> PreparedAck {
    PreparedAck::V4(rumqttc_v4::ManualAck::PubAck(rumqttc_v4::PubAck::new(
        packet_id,
    )))
}
