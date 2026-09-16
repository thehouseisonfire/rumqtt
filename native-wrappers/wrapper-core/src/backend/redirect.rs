use std::future::Future;
use std::panic::AssertUnwindSafe;

use futures_util::FutureExt;

use crate::{
    BrokerTarget, Error, RedirectEvent, RedirectFailure, RedirectPolicy, RedirectReason,
    RedirectSource, SrvFailure, TransportConfig,
};

pub(super) fn configure(
    options: &mut rumqttc_v5::MqttOptions,
    policy: &RedirectPolicy,
    resolver: Option<crate::SrvResolverConfig>,
) -> crate::Result<()> {
    if let RedirectPolicy::Follow {
        max_attempts,
        transport,
    } = policy
    {
        let transport = match transport {
            TransportConfig::Tcp => rumqttc_v5::Transport::Tcp,
            #[cfg(any(feature = "use-rustls", feature = "use-native-tls"))]
            TransportConfig::Tls(tls) => {
                rumqttc_v5::Transport::tls_with_config(super::build_tls(tls)?)
            }
            #[cfg(feature = "websocket")]
            TransportConfig::WebSocket => rumqttc_v5::Transport::Ws,
            #[cfg(all(
                feature = "websocket",
                any(feature = "use-rustls", feature = "use-native-tls")
            ))]
            TransportConfig::Wss(tls) => {
                rumqttc_v5::Transport::wss_with_config(super::build_tls(tls)?)
            }
            _ => return Err(Error::configuration("redirect transport is unavailable")),
        };
        let attempts = std::num::NonZeroUsize::new(*max_attempts)
            .ok_or_else(|| Error::configuration("redirect attempts must be nonzero"))?;
        options.set_redirect_policy(rumqttc_v5::RedirectPolicy::new(attempts, move |context| {
            context
                .references
                .iter()
                .find_map(|reference| {
                    rumqttc_v5::RedirectTargetProfile::isolated(
                        reference.clone(),
                        transport.clone(),
                    )
                    .ok()
                })
                .map_or(
                    rumqttc_v5::RedirectDecision::Reject,
                    rumqttc_v5::RedirectDecision::follow,
                )
        }));
    }
    #[cfg(feature = "system-srv-resolver")]
    if resolver.is_none() && matches!(policy, RedirectPolicy::Follow { .. }) {
        options.set_srv_resolver(
            rumqttc_v5::SrvResolver::system()
                .map_err(|_| Error::configuration("system SRV resolver initialization failed"))?,
        );
    }
    if let Some(resolver) = resolver {
        options.set_srv_resolver(rumqttc_v5::SrvResolver::new(move |owner| {
            let resolver = resolver.clone();
            async move {
                let work = async { resolver.0.resolve(owner).await };
                tokio::pin!(work);
                let guarded = std::future::poll_fn(|cx| {
                    crate::runtime::with_host_callback(|| work.as_mut().poll(cx))
                });
                let result = AssertUnwindSafe(guarded).catch_unwind().await;
                let result = match result {
                    Ok(result) => result,
                    Err(payload) => {
                        std::mem::forget(payload);
                        Err(SrvFailure::Panic)
                    }
                };
                result
                    .map(|records| {
                        records
                            .into_iter()
                            .map(|record| rumqttc_v5::SrvRecord {
                                priority: record.priority,
                                weight: record.weight,
                                port: record.port,
                                target: record.target,
                            })
                            .collect()
                    })
                    .map_err(rumqttc_v5::SrvLookupError::custom)
            }
        }));
    }
    Ok(())
}

pub(super) fn event(
    outcome: rumqttc_v5::RedirectOutcome,
    target: Option<BrokerTarget>,
    failure: Option<RedirectFailure>,
) -> RedirectEvent {
    RedirectEvent {
        source: match outcome.source {
            rumqttc_v5::RedirectSource::ConnAck => RedirectSource::ConnAck,
            rumqttc_v5::RedirectSource::Disconnect => RedirectSource::Disconnect,
        },
        reason: match outcome.reason {
            rumqttc_v5::RedirectReason::UseAnotherServer => RedirectReason::UseAnotherServer,
            rumqttc_v5::RedirectReason::ServerMoved => RedirectReason::ServerMoved,
        },
        server_reference: outcome.server_reference,
        target,
        failure,
    }
}

pub(super) fn failure(failure: &rumqttc_v5::RedirectFailure) -> RedirectFailure {
    use rumqttc_v5::RedirectFailure as F;
    match failure {
        F::Disabled => RedirectFailure::Disabled,
        F::Rejected | F::UnadvertisedTarget => RedirectFailure::Rejected,
        F::InvalidReference(_) => RedirectFailure::InvalidReference,
        F::Target(_) => RedirectFailure::UnsupportedTarget,
        F::Loop | F::SrvAllTargetsVisited { .. } => RedirectFailure::Loop,
        F::AttemptLimit => RedirectFailure::AttemptLimit,
        F::SrvLookupTimeout { .. } => RedirectFailure::Timeout,
        F::SrvLookup { source, .. } => std::error::Error::source(source)
            .and_then(|source| source.downcast_ref::<SrvFailure>())
            .copied()
            .map_or(RedirectFailure::Dns, RedirectFailure::Callback),
        F::FollowFailed(_) | F::SrvTargetsExhausted { .. } => RedirectFailure::Transport,
        _ => RedirectFailure::Dns,
    }
}

pub(super) fn broker(broker: &rumqttc_v5::Broker) -> Option<BrokerTarget> {
    if let Some((host, port)) = broker.tcp_address() {
        return Some(BrokerTarget::Tcp {
            host: host.into(),
            port,
        });
    }
    #[cfg(feature = "websocket")]
    if let Some(url) = broker.websocket_url() {
        return Some(BrokerTarget::WebSocket { url: url.into() });
    }
    #[cfg(unix)]
    if let Some(path) = broker.unix_path() {
        return Some(BrokerTarget::Unix { path: path.into() });
    }
    None
}
