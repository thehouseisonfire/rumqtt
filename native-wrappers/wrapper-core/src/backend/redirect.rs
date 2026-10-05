use crate::{RedirectDecisionFailure as D, RedirectRequest, RedirectResponse};
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use futures_util::FutureExt;

use crate::{
    BrokerTarget, Error, RedirectEvent, RedirectFailure, RedirectPolicy, RedirectReason,
    RedirectSource, SrvFailure, TransportConfig,
};

pub(super) fn configure(
    options: &mut rumqttc_v5::MqttOptions,
    policy: &RedirectPolicy,
    resolver: Option<crate::SrvResolverConfig>,
    common: &crate::CommonConfig,
    stores: Option<Arc<super::session::AdapterFactory>>,
    tls_callbacks: &Arc<super::TlsCallbackMonitor>,
) -> crate::Result<()> {
    if let RedirectPolicy::Follow {
        max_attempts,
        transport,
    } = policy
    {
        let transport = transport_config(transport, tls_callbacks)?;
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
    if let RedirectPolicy::Application(config) = policy {
        let attempts = std::num::NonZeroUsize::new(config.max_attempts)
            .ok_or_else(|| Error::configuration("redirect attempts must be nonzero"))?;
        let config = config.clone();
        // Validate newly supplied target fields against a credential-free transport template.
        // Actual reuse always comes from current native options, including after prior hops.
        let mut common = common.clone();
        common.username = None;
        common.password = None;
        common.proxy = None;
        common.websocket_headers.clear();
        common.websocket_handshake = None;
        common.transport = TransportConfig::Tcp;
        let tls_callbacks = Arc::clone(tls_callbacks);
        options.set_redirect_policy(rumqttc_v5::RedirectPolicy::try_new(
            attempts,
            move |context| {
                decide(context, &config, &common, stores.as_deref(), &tls_callbacks).map_err(
                    |failure| rumqttc_v5::RedirectTargetError::Policy(native_failure(failure)),
                )
            },
        ));
    }
    #[cfg(feature = "system-srv-resolver")]
    if resolver.is_none() && !matches!(policy, RedirectPolicy::Reject) {
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
    diagnostics: &rumqttc_v5::RedirectDiagnostics,
) -> RedirectEvent {
    RedirectEvent {
        selected_reference: diagnostics.selected_reference.clone(),
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
        followed: matches!(
            failure,
            None | Some(
                RedirectFailure::Callback(_)
                    | RedirectFailure::Dns
                    | RedirectFailure::Timeout
                    | RedirectFailure::Transport
            )
        ),
        attempts: diagnostics.attempts,
        attempt_limit: diagnostics.attempt_limit,
        visited_endpoints: diagnostics.visited_endpoints,
        srv_candidate_index: diagnostics.srv_candidate_index,
        srv_candidate_count: diagnostics.srv_candidate_count,
    }
}

pub(super) fn failure(failure: &rumqttc_v5::RedirectFailure) -> RedirectFailure {
    use rumqttc_v5::RedirectFailure as F;
    match failure {
        F::Disabled => RedirectFailure::Disabled,
        F::Rejected | F::UnadvertisedTarget => RedirectFailure::Rejected,
        F::InvalidReference(_) => RedirectFailure::InvalidReference,
        F::Target(rumqttc_v5::RedirectTargetError::Policy(f)) => {
            RedirectFailure::Policy(wrapper_failure(*f))
        }
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

fn transport_config(
    transport: &TransportConfig,
    tls_callbacks: &Arc<super::TlsCallbackMonitor>,
) -> crate::Result<rumqttc_v5::Transport> {
    #[cfg(not(any(feature = "use-rustls-no-provider", feature = "use-native-tls")))]
    let _ = tls_callbacks;
    Ok(match transport {
        TransportConfig::Tcp => rumqttc_v5::Transport::Tcp,
        #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
        TransportConfig::Tls(tls) => rumqttc_v5::Transport::tls_with_config(super::build_tls(
            tls,
            crate::TlsLayer::Redirect,
            tls_callbacks,
        )?),
        #[cfg(feature = "websocket")]
        TransportConfig::WebSocket => rumqttc_v5::Transport::Ws,
        #[cfg(all(
            feature = "websocket",
            any(feature = "use-rustls-no-provider", feature = "use-native-tls")
        ))]
        TransportConfig::Wss(tls) => rumqttc_v5::Transport::wss_with_config(super::build_tls(
            tls,
            crate::TlsLayer::Redirect,
            tls_callbacks,
        )?),
        _ => return Err(Error::configuration("redirect transport is unavailable")),
    })
}

const fn native_failure(failure: D) -> rumqttc_v5::RedirectPolicyFailure {
    use rumqttc_v5::RedirectPolicyFailure as N;
    match failure {
        D::Callback => N::Callback,
        D::Panic => N::Panic,
        D::Timeout => N::Timeout,
        D::InvalidResponse => N::InvalidResponse,
        D::ResourceLimit => N::ResourceLimit,
        D::StoreInUse => N::StoreInUse,
    }
}
const fn wrapper_failure(failure: rumqttc_v5::RedirectPolicyFailure) -> D {
    use rumqttc_v5::RedirectPolicyFailure as N;
    match failure {
        N::Callback => D::Callback,
        N::Panic => D::Panic,
        N::Timeout => D::Timeout,
        N::InvalidResponse => D::InvalidResponse,
        N::ResourceLimit => D::ResourceLimit,
        N::StoreInUse => D::StoreInUse,
    }
}
fn decide(
    context: &rumqttc_v5::RedirectContext<'_>,
    config: &crate::RedirectAuthorityConfig,
    common: &crate::CommonConfig,
    stores: Option<&super::session::AdapterFactory>,
    tls_callbacks: &Arc<super::TlsCallbackMonitor>,
) -> Result<rumqttc_v5::RedirectDecision, D> {
    let deadline = std::time::Instant::now()
        .checked_add(config.decision_timeout)
        .ok_or(D::InvalidResponse)?;
    let request = snapshot(context, deadline)?;
    let response = match catch_unwind(AssertUnwindSafe(|| {
        crate::runtime::with_host_callback(|| config.authority.decide(request.clone()))
    })) {
        Ok(response) => response,
        Err(payload) => {
            std::mem::forget(payload);
            return Err(D::Panic);
        }
    };
    if std::time::Instant::now() >= deadline {
        return Err(D::Timeout);
    }
    let RedirectResponse {
        request: owner,
        target,
    } = response?;
    if !Arc::ptr_eq(&owner, &request) {
        return Err(D::InvalidResponse);
    }
    let Some((index, target)) = target else {
        return Ok(rumqttc_v5::RedirectDecision::Reject);
    };
    let profile = materialize(context, common, stores, tls_callbacks, index, &target)?;
    if std::time::Instant::now() >= deadline {
        return Err(D::Timeout);
    }
    Ok(rumqttc_v5::RedirectDecision::follow(
        profile.decision_deadline(deadline),
    ))
}

fn snapshot(
    context: &rumqttc_v5::RedirectContext<'_>,
    deadline: std::time::Instant,
) -> Result<Arc<RedirectRequest>, D> {
    if context.references.len() > crate::MAX_REDIRECT_REFERENCES
        || context.client_id.len()
            + context.store_scope.len()
            + context
                .references
                .iter()
                .map(|r| {
                    r.raw().len()
                        + r.host().len()
                        + r.websocket_resource_name().map_or(0, str::len)
                        + r.srv_owner().map_or(0, str::len)
                })
                .sum::<usize>()
            > crate::MAX_REDIRECT_REQUEST_BYTES
    {
        return Err(D::ResourceLimit);
    }
    Ok(Arc::new(RedirectRequest {
        source: match context.outcome.source {
            rumqttc_v5::RedirectSource::ConnAck => crate::RedirectSource::ConnAck,
            rumqttc_v5::RedirectSource::Disconnect => crate::RedirectSource::Disconnect,
        },
        reason: match context.outcome.reason {
            rumqttc_v5::RedirectReason::UseAnotherServer => crate::RedirectReason::UseAnotherServer,
            rumqttc_v5::RedirectReason::ServerMoved => crate::RedirectReason::ServerMoved,
        },
        attempt: context.attempt,
        client_id: context.client_id.to_owned(),
        store_scope: context.store_scope.into(),
        deadline,
        references: context
            .references
            .iter()
            .map(|r| crate::RedirectReference {
                raw: r.raw().into(),
                host: r.host().into(),
                port: r.port(),
                scheme: r.scheme().map(|scheme| match scheme {
                    rumqttc_v5::RedirectScheme::Mqtt => crate::RedirectScheme::Mqtt,
                    rumqttc_v5::RedirectScheme::Mqtts => crate::RedirectScheme::Mqtts,
                    rumqttc_v5::RedirectScheme::Ws => crate::RedirectScheme::Ws,
                    rumqttc_v5::RedirectScheme::Wss => crate::RedirectScheme::Wss,
                    _ => unreachable!("native scheme is validated"),
                }),
                websocket_resource: r.websocket_resource_name().map(str::to_owned),
                srv_owner: r.srv_owner().map(str::to_owned),
            })
            .collect(),
    }))
}

fn materialize(
    context: &rumqttc_v5::RedirectContext<'_>,
    common: &crate::CommonConfig,
    stores: Option<&super::session::AdapterFactory>,
    tls_callbacks: &Arc<super::TlsCallbackMonitor>,
    index: usize,
    target: &crate::RedirectTargetConfig,
) -> Result<rumqttc_v5::RedirectTargetProfile, D> {
    target.validate()?;
    let reference = context
        .references
        .get(index)
        .ok_or(D::InvalidResponse)?
        .clone();
    let transport =
        transport_config(&target.transport, tls_callbacks).map_err(|_| D::InvalidResponse)?;
    let mut profile = rumqttc_v5::RedirectTargetProfile::isolated(reference.clone(), transport)
        .map_err(|_| D::InvalidResponse)?;
    // Reuse native profile materialization, then validate the wrapper's connector and layering contract.
    let mut target_common = common.clone();
    target_common.client_id = match &target.client_id {
        crate::RedirectClientId::Fresh => String::new(),
        crate::RedirectClientId::Reuse => context.client_id.to_owned(),
        crate::RedirectClientId::Replace(id) => id.clone(),
    };
    target_common.transport = target.transport.clone();
    target_common.broker =
        profile
            .broker()
            .and_then(broker)
            .unwrap_or_else(|| crate::BrokerTarget::Tcp {
                host: reference.host().into(),
                port: reference.port().unwrap_or(1883),
            });
    target_common.username.clone_from(&target.username);
    target_common.password = target
        .password
        .as_ref()
        .map(|p| bytes::Bytes::copy_from_slice(p.expose()));
    target_common.validate().map_err(|_| D::InvalidResponse)?;
    profile = profile.client_id(match &target.client_id {
        crate::RedirectClientId::Fresh => rumqttc_v5::RedirectClientId::Fresh,
        crate::RedirectClientId::Reuse => rumqttc_v5::RedirectClientId::Reuse,
        crate::RedirectClientId::Replace(id) => rumqttc_v5::RedirectClientId::Replace(id.clone()),
    });
    let auth = match (target.username.clone(), target.password.as_ref()) {
        (None, None) => rumqttc_v5::ConnectAuth::None,
        (Some(username), None) => rumqttc_v5::ConnectAuth::Username { username },
        (None, Some(password)) => rumqttc_v5::ConnectAuth::Password {
            password: bytes::Bytes::copy_from_slice(password.expose()),
        },
        (Some(username), Some(password)) => rumqttc_v5::ConnectAuth::UsernamePassword {
            username,
            password: bytes::Bytes::copy_from_slice(password.expose()),
        },
    };
    profile = if target.reuse_authentication_authority {
        profile.reuse_authentication(auth)
    } else {
        profile.authentication(auth)
    };
    if target.reuse_network_credentials {
        profile = profile.reuse_network_credentials();
    }
    if let crate::RedirectSession::Reuse { store_scope } = &target.session {
        if context.has_session_store {
            if context.clean_start
                || context.session_expiry_interval.unwrap_or(0) == 0
                || target_common.client_id.is_empty()
                || store_scope.is_empty()
            {
                return Err(D::InvalidResponse);
            }
            let store = stores
                .ok_or(D::InvalidResponse)?
                .prepare(store_scope, &target_common.client_id)
                .map_err(|e| {
                    if e.store_failure() == Some(crate::StoreFailure::InUse) {
                        D::StoreInUse
                    } else {
                        D::InvalidResponse
                    }
                })?;
            profile = profile.session_store_arc(store);
        }
        profile = profile.session(rumqttc_v5::RedirectSession::Reuse {
            store_scope: store_scope.clone(),
        });
    }
    Ok(profile)
}
