mod auth;
mod redirect;
pub mod session;
pub mod v4;
pub mod v5;

use std::time::Duration;

use crate::operations::CompletionFuture;
use crate::validation::protocol_option_error;
use crate::{
    ClientConfig, Error, ErrorKind, ProtocolConfig, PublishCommand, PublishProtocolOptions, Result,
    SubscribeCommand, SubscribeProtocolOptions, SubscriptionProtocolOptions, UnsubscribeCommand,
    UnsubscribeProtocolOptions,
};

pub enum BackendClient {
    V4(rumqttc_v4::AsyncClient),
    V5(rumqttc_v5::AsyncClient),
}

pub enum BackendDriver {
    V4(Box<rumqttc_v4::EventLoop>),
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
        properties: Option<crate::AuthProperties>,
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
            .map_err(v5::map_client_error)?;
        Ok(Box::pin(async move {
            notice
                .wait_async()
                .await
                .map(|_| crate::Completion::Authenticated)
                .map_err(|error| {
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
        }))
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
                    .map_err(v4::map_client_error)?;
                Ok(Box::pin(async move {
                    v4::map_publish_notice(notice.wait_async().await)
                }))
            }
            Self::V5(client) => {
                v5::validate_publish(&command)?;
                let options = v5::publish_options(&command);
                let notice = client
                    .try_publish_tracked(command.topic, command.payload, options)
                    .map_err(v5::map_client_error)?;
                Ok(Box::pin(async move {
                    v5::map_publish_notice(notice.wait_async().await)
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
                    .map_err(v4::map_client_error)?;
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
                .map_err(v5::map_client_error)?;
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
                    .map_err(v4::map_client_error)?;
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
                .map_err(v5::map_client_error)?;
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
                .map_err(v4::map_client_error),
            (Self::V5(client), PreparedAck::V5(ack)) => client
                .try_manual_ack(ack.clone())
                .map_err(v5::map_client_error),
            _ => Err(Error::new(
                ErrorKind::Internal,
                "acknowledgement protocol mismatch",
            )),
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
            .map_err(v5::map_client_error);
        }
        match self {
            Self::V4(client) => timeout
                .map_or_else(
                    || client.try_disconnect(),
                    |timeout| client.try_disconnect_with_timeout(timeout),
                )
                .map_err(v4::map_client_error),
            Self::V5(client) => timeout
                .map_or_else(
                    || client.try_disconnect(),
                    |timeout| client.try_disconnect_with_timeout(timeout),
                )
                .map_err(v5::map_client_error),
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
                .map_err(v5::map_client_error);
        }
        match self {
            Self::V4(client) => client.try_disconnect_now().map_err(v4::map_client_error),
            Self::V5(client) => client.try_disconnect_now().map_err(v5::map_client_error),
        }
    }

    pub(crate) fn best_effort_disconnect_now(&self, protocol: &crate::DisconnectProtocolOptions) {
        _ = self.try_disconnect_now(protocol);
    }
}

impl BackendDriver {
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

#[cfg(any(feature = "use-rustls", feature = "use-native-tls"))]
fn build_tls(config: &crate::TlsConfig) -> Result<rumqttc_v4::TlsConfiguration> {
    match config.backend {
        #[cfg(feature = "use-rustls")]
        crate::TlsBackend::Rustls => build_rustls(config),
        #[cfg(feature = "use-native-tls")]
        crate::TlsBackend::Native => build_native_tls(config),
        #[allow(unreachable_patterns)]
        _ => Err(Error::configuration("selected TLS backend is disabled")),
    }
}

#[cfg(feature = "use-rustls")]
fn build_rustls(config: &crate::TlsConfig) -> Result<rumqttc_v4::TlsConfiguration> {
    let client_auth = match &config.identity {
        Some(crate::TlsClientIdentity::RustlsPem {
            certificate,
            private_key,
        }) => Some((certificate.to_vec(), private_key.expose().to_vec())),
        None => None,
        _ => return Err(Error::configuration("rustls requires a PEM identity")),
    };
    let result = if let crate::TlsRootPolicy::Pem(ca) = &config.roots {
        rumqttc_v4::TlsConfiguration::try_rustls_with_pem_roots(ca, client_auth)
    } else {
        rumqttc_v4::TlsConfiguration::try_rustls_with_native_roots(client_auth)
    };
    // Do not retain source errors from credential parsing: native host diagnostics may
    // recursively display their source chains.
    let mut tls =
        result.map_err(|_| Error::new(ErrorKind::Tls, "failed to construct rustls credentials"))?;
    if let rumqttc_v4::TlsConfiguration::Rustls(client) = &mut tls {
        std::sync::Arc::make_mut(client).alpn_protocols = config.alpn_protocols.clone();
    }
    Ok(tls)
}

#[cfg(feature = "use-native-tls")]
fn build_native_tls(config: &crate::TlsConfig) -> Result<rumqttc_v4::TlsConfiguration> {
    let invalid = || Error::new(ErrorKind::Tls, "failed to construct native TLS credentials");
    let mut builder = native_tls::TlsConnector::builder();
    if let crate::TlsRootPolicy::Pem(ca) = &config.roots {
        builder.disable_built_in_roots(true);
        // native-tls accepts one PEM certificate per call. Split a bundle explicitly.
        let pem = std::str::from_utf8(ca).map_err(|_| invalid())?;
        let mut rest = pem.trim();
        let mut count = 0;
        while !rest.is_empty() {
            if !rest.starts_with("-----BEGIN CERTIFICATE-----") {
                return Err(invalid());
            }
            let end = rest.find("-----END CERTIFICATE-----").ok_or_else(invalid)?
                + "-----END CERTIFICATE-----".len();
            builder.add_root_certificate(
                native_tls::Certificate::from_pem(&rest.as_bytes()[..end])
                    .map_err(|_| invalid())?,
            );
            count += 1;
            rest = rest[end..].trim();
        }
        if count == 0 {
            return Err(invalid());
        }
    }
    if let Some(crate::TlsClientIdentity::NativePkcs12 { identity, password }) = &config.identity {
        let password = std::str::from_utf8(password.expose()).map_err(|_| invalid())?;
        builder.identity(
            native_tls::Identity::from_pkcs12(identity.expose(), password)
                .map_err(|_| invalid())?,
        );
    }
    let alpn: Vec<&str> = config
        .alpn_protocols
        .iter()
        .map(|value| std::str::from_utf8(value).map_err(|_| invalid()))
        .collect::<Result<_>>()?;
    builder.request_alpns(&alpn);
    Ok(rumqttc_v4::TlsConfiguration::NativeConnector(
        builder.build().map_err(|_| invalid())?,
    ))
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
fn build_proxy(config: &crate::ProxyConfig) -> Result<rumqttc_v4::Proxy> {
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
            any(feature = "use-rustls", feature = "use-native-tls")
        ))]
        crate::ProxyConfig::Http {
            host,
            port,
            credentials,
            tls: Some(tls),
        } => (
            rumqttc_v4::Proxy::https(host.clone(), *port, build_tls(tls)?),
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
