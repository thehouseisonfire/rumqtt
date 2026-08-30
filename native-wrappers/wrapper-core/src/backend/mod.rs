pub mod v4;
pub mod v5;

use std::time::Duration;

use crate::operations::CompletionFuture;
use crate::validation::protocol_option_error;
use crate::{
    ClientConfig, DeliveryStatus, Error, ErrorKind, ProtocolConfig, PublishCommand,
    PublishProtocolOptions, Result, SubscribeCommand, SubscribeProtocolOptions,
    SubscriptionProtocolOptions, TlsConfig, UnsubscribeCommand, UnsubscribeProtocolOptions,
};

pub enum BackendClient {
    V4(rumqttc_v4::AsyncClient),
    V5(rumqttc_v5::AsyncClient),
}

pub enum BackendDriver {
    V4(Box<rumqttc_v4::EventLoop>),
    V5(Box<rumqttc_v5::EventLoop>),
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

    pub(crate) fn try_disconnect(&self, timeout: Option<Duration>) -> Result<()> {
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

    pub(crate) fn try_disconnect_now(&self) -> Result<()> {
        match self {
            Self::V4(client) => client.try_disconnect_now().map_err(v4::map_client_error),
            Self::V5(client) => client.try_disconnect_now().map_err(v5::map_client_error),
        }
    }

    pub(crate) fn best_effort_disconnect_now(&self) {
        _ = self.try_disconnect_now();
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

fn build_tls(config: &TlsConfig) -> Result<rumqttc_v4::TlsConfiguration> {
    let client_auth = config
        .client_certificate
        .as_ref()
        .zip(config.private_key.as_ref())
        .map(|(certificate, key)| (certificate.to_vec(), key.to_vec()));
    let result = if let Some(ca) = &config.ca {
        rumqttc_v4::TlsConfiguration::try_rustls_with_pem_roots(ca, client_auth)
    } else {
        rumqttc_v4::TlsConfiguration::try_rustls_with_native_roots(client_auth)
    };
    result.map_err(|error| Error::sourced(ErrorKind::Tls, DeliveryStatus::NotApplicable, error))
}

#[cfg(test)]
pub const fn test_v4_puback(packet_id: u16) -> PreparedAck {
    PreparedAck::V4(rumqttc_v4::ManualAck::PubAck(rumqttc_v4::PubAck::new(
        packet_id,
    )))
}
