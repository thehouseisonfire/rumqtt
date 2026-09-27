use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll};

use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::{
    AuthAction, AuthChallenge, AuthContext, AuthExchange, AuthFailure, AuthProperties,
    AuthenticatorConfig,
};

pub(super) struct AsyncAdapter {
    pub client_id: String,
    pub config: crate::AsyncAuthenticatorConfig,
    pub monitor: Arc<Monitor>,
    pub generation: AtomicU64,
}

impl std::fmt::Debug for AsyncAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncAdapter")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

impl rumqttc_v5::AsyncAuthenticator for AsyncAdapter {
    fn respond(
        &self,
        context: rumqttc_v5::AsyncAuthContext,
        challenge: rumqttc_v5::AsyncAuthChallenge,
    ) -> rumqttc_v5::AuthFuture {
        let exchange = match context.kind {
            rumqttc_v5::AuthExchangeKind::InitialConnect => crate::AuthExchange::Initial,
            rumqttc_v5::AuthExchangeKind::Reauthentication => crate::AuthExchange::Reauthentication,
        };
        let is_start = matches!(challenge, rumqttc_v5::AsyncAuthChallenge::Start);
        let is_success = matches!(challenge, rumqttc_v5::AsyncAuthChallenge::Success { .. });
        if is_start && exchange == crate::AuthExchange::Initial {
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
        if is_start {
            self.monitor.deadline(Some(
                tokio::time::Instant::now() + self.config.exchange_timeout,
            ));
        }
        let deadline = self.monitor.snapshot().1;
        let method = context.method.clone();
        let challenge = match challenge {
            rumqttc_v5::AsyncAuthChallenge::Start => crate::AsyncAuthChallenge::Start,
            rumqttc_v5::AsyncAuthChallenge::Continue {
                reason_code,
                properties,
            } => crate::AsyncAuthChallenge::Continue {
                reason_code,
                properties: properties.map(from_properties),
            },
            rumqttc_v5::AsyncAuthChallenge::Success {
                reason_code,
                properties,
            } => crate::AsyncAuthChallenge::Success {
                reason_code,
                properties: properties.map(from_properties),
            },
        };
        let context = crate::AuthContext {
            client_id: self.client_id.clone(),
            exchange,
            generation: self.generation.load(Ordering::Relaxed),
            method: method.clone(),
        };
        let authority = self.config.authenticator.clone();
        let monitor = self.monitor.clone();
        let callback_guard = self.monitor.begin_async_callback();
        let future = catch_unwind(AssertUnwindSafe(|| {
            crate::runtime::with_host_callback(|| authority.respond(context, challenge))
        }))
        .map(|future| HostAuthFuture {
            future: Some(future),
            monitor: monitor.clone(),
        })
        .map_err(|payload| {
            std::mem::forget(payload);
            crate::AuthFailure::Panic
        });
        Box::pin(async move {
            let _callback_guard = callback_guard;
            let result = match future {
                Ok(mut future) => {
                    let result = if deadline
                        .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
                    {
                        Err(crate::AuthFailure::Timeout)
                    } else if let Some(deadline) = deadline {
                        tokio::time::timeout_at(deadline, &mut future)
                            .await
                            .unwrap_or(Err(crate::AuthFailure::Timeout))
                    } else {
                        (&mut future).await
                    };
                    // Destruction is host code too, including when a ready result or timeout
                    // ends the callback. Report its panic before accepting any response.
                    future.destroy().and(result)
                }
                Err(failure) => Err(failure),
            };
            let result = result.and_then(|action| {
                if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                    return Err(crate::AuthFailure::Timeout);
                }
                if is_success && !matches!(action, crate::AuthAction::Complete) {
                    return Err(crate::AuthFailure::InvalidResponse);
                }
                if let crate::AuthAction::Send(properties) = &action {
                    properties
                        .validate()
                        .map_err(|_| crate::AuthFailure::InvalidResponse)?;
                    if properties
                        .method
                        .as_deref()
                        .is_some_and(|value| value != method)
                    {
                        return Err(crate::AuthFailure::Method);
                    }
                }
                Ok(action)
            });
            if is_success || result.is_err() {
                monitor.deadline(None);
            }
            match result {
                Ok(crate::AuthAction::Complete) => Ok(rumqttc_v5::AuthAction::Complete),
                Ok(crate::AuthAction::Send(properties)) => {
                    Ok(rumqttc_v5::AuthAction::Send(to_properties(properties)))
                }
                Err(failure) => {
                    monitor.fail(failure);
                    Err(rumqttc_v5::AuthError::Failed(
                        "wrapper authentication callback failed".into(),
                    ))
                }
            }
        })
    }

    fn failure(&self, context: rumqttc_v5::AsyncAuthContext, error: rumqttc_v5::AuthError) {
        self.monitor.deadline(None);
        let failure = self.monitor.snapshot().0.unwrap_or(match error {
            rumqttc_v5::AuthError::BrokerRejected(_)
            | rumqttc_v5::AuthError::BrokerDisconnected(_) => crate::AuthFailure::BrokerRejected,
            _ => crate::AuthFailure::ConnectionClosed,
        });
        let context = crate::AuthContext {
            client_id: self.client_id.clone(),
            exchange: match context.kind {
                rumqttc_v5::AuthExchangeKind::InitialConnect => crate::AuthExchange::Initial,
                rumqttc_v5::AuthExchangeKind::Reauthentication => {
                    crate::AuthExchange::Reauthentication
                }
            },
            generation: self.generation.load(Ordering::Relaxed),
            method: context.method,
        };
        let result = catch_unwind(AssertUnwindSafe(|| {
            crate::runtime::with_host_callback(|| {
                self.config.authenticator.failure(context, failure)
            });
        }));
        if let Err(payload) = result {
            std::mem::forget(payload);
            self.monitor.fail(crate::AuthFailure::Panic);
        }
    }
}

/// Owns the host future so cancellation cannot drop it outside the panic boundary.
struct HostAuthFuture {
    future: Option<crate::AuthFuture>,
    monitor: Arc<Monitor>,
}

impl HostAuthFuture {
    fn destroy(&mut self) -> Result<(), crate::AuthFailure> {
        let Some(future) = self.future.take() else {
            return Ok(());
        };
        let result = catch_unwind(AssertUnwindSafe(|| {
            crate::runtime::with_host_callback(|| drop(future));
        }));
        if let Err(payload) = result {
            // Panic payloads can themselves have panicking destructors.
            std::mem::forget(payload);
            self.monitor.fail(crate::AuthFailure::Panic);
            return Err(crate::AuthFailure::Panic);
        }
        Ok(())
    }
}

impl Future for HostAuthFuture {
    type Output = Result<crate::AuthAction, crate::AuthFailure>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match catch_unwind(AssertUnwindSafe(|| {
            crate::runtime::with_host_callback(|| {
                this.future
                    .as_mut()
                    .expect("host authentication future is present")
                    .as_mut()
                    .poll(cx)
            })
        })) {
            Ok(result) => result,
            Err(payload) => {
                std::mem::forget(payload);
                Poll::Ready(Err(crate::AuthFailure::Panic))
            }
        }
    }
}

impl Drop for HostAuthFuture {
    fn drop(&mut self) {
        let _ = self.destroy();
    }
}

#[derive(Debug, Default)]
pub(super) struct Monitor {
    state: Mutex<(Option<AuthFailure>, Option<tokio::time::Instant>)>,
    pub changed: Notify,
    callback_active: AtomicBool,
    pub callback_changed: Notify,
}

impl Monitor {
    pub fn snapshot(&self) -> (Option<AuthFailure>, Option<tokio::time::Instant>) {
        *self.state.lock()
    }
    pub fn fail(&self, failure: AuthFailure) {
        self.state.lock().0.get_or_insert(failure);
        self.changed.notify_one();
    }
    fn deadline(&self, deadline: Option<tokio::time::Instant>) {
        self.state.lock().1 = deadline;
        self.changed.notify_one();
    }

    fn begin_async_callback(self: &Arc<Self>) -> AsyncCallbackGuard {
        self.callback_active.store(true, Ordering::Release);
        self.callback_changed.notify_one();
        AsyncCallbackGuard(self.clone())
    }

    pub fn callback_active(&self) -> bool {
        self.callback_active.load(Ordering::Acquire)
    }
}

struct AsyncCallbackGuard(Arc<Monitor>);

impl Drop for AsyncCallbackGuard {
    fn drop(&mut self) {
        self.0.callback_active.store(false, Ordering::Release);
        self.0.callback_changed.notify_one();
    }
}

#[derive(Debug)]
pub(super) struct Adapter {
    pub client_id: String,
    pub config: AuthenticatorConfig,
    pub monitor: Arc<Monitor>,
    pub generation: u64,
}

impl Adapter {
    fn invoke(
        &self,
        context: rumqttc_v5::AuthContext<'_>,
        challenge: AuthChallenge,
    ) -> Result<AuthAction, rumqttc_v5::AuthError> {
        let context = AuthContext {
            client_id: self.client_id.clone(),
            exchange: match context.kind {
                rumqttc_v5::AuthExchangeKind::InitialConnect => AuthExchange::Initial,
                rumqttc_v5::AuthExchangeKind::Reauthentication => AuthExchange::Reauthentication,
            },
            generation: self.generation,
            method: context.method.into(),
        };
        let method = context.method.clone();
        let result = catch_unwind(AssertUnwindSafe(|| {
            crate::runtime::with_host_callback(|| {
                self.config.authenticator.respond(context, challenge)
            })
        }));
        let result = match result {
            Ok(result) => result,
            Err(payload) => {
                std::mem::forget(payload);
                Err(AuthFailure::Panic)
            }
        };
        let result = result.and_then(|action| {
            if self
                .monitor
                .snapshot()
                .1
                .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
            {
                return Err(AuthFailure::Timeout);
            }
            if let AuthAction::Send(properties) = &action {
                properties
                    .validate()
                    .map_err(|_| AuthFailure::InvalidResponse)?;
                if properties
                    .method
                    .as_deref()
                    .is_some_and(|value| value != method)
                {
                    return Err(AuthFailure::Method);
                }
            }
            Ok(action)
        });
        result.map_err(|failure| {
            self.monitor.fail(failure);
            rumqttc_v5::AuthError::Failed("wrapper authentication callback failed".into())
        })
    }
}

impl rumqttc_v5::Authenticator for Adapter {
    fn start(
        &mut self,
        context: rumqttc_v5::AuthContext<'_>,
    ) -> Result<Option<rumqttc_v5::AuthProperties>, rumqttc_v5::AuthError> {
        if context.kind == rumqttc_v5::AuthExchangeKind::InitialConnect {
            self.generation = self.generation.saturating_add(1);
        }
        self.monitor.deadline(Some(
            tokio::time::Instant::now() + self.config.exchange_timeout,
        ));
        match self.invoke(context, AuthChallenge::Start)? {
            AuthAction::Complete => Ok(None),
            AuthAction::Send(properties) => Ok(Some(to_properties(properties))),
        }
    }
    fn continue_auth(
        &mut self,
        context: rumqttc_v5::AuthContext<'_>,
        incoming: Option<rumqttc_v5::AuthProperties>,
    ) -> Result<rumqttc_v5::AuthAction, rumqttc_v5::AuthError> {
        self.invoke(
            context,
            AuthChallenge::Continue(incoming.map(from_properties)),
        )
        .map(|action| match action {
            AuthAction::Complete => rumqttc_v5::AuthAction::Complete,
            AuthAction::Send(properties) => rumqttc_v5::AuthAction::Send(to_properties(properties)),
        })
    }
    fn success(
        &mut self,
        context: rumqttc_v5::AuthContext<'_>,
        incoming: Option<rumqttc_v5::AuthProperties>,
    ) -> Result<(), rumqttc_v5::AuthError> {
        let action = self.invoke(
            context,
            AuthChallenge::Success(incoming.map(from_properties)),
        )?;
        self.monitor.deadline(None);
        if action != AuthAction::Complete {
            self.monitor.fail(AuthFailure::InvalidResponse);
            return Err(rumqttc_v5::AuthError::Failed(
                "invalid success response".into(),
            ));
        }
        Ok(())
    }
    fn failure(&mut self, context: rumqttc_v5::AuthContext<'_>, _: rumqttc_v5::AuthError) {
        self.monitor.deadline(None);
        if matches!(
            self.invoke(context, AuthChallenge::Failed),
            Ok(AuthAction::Send(_))
        ) {
            self.monitor.fail(AuthFailure::InvalidResponse);
        }
    }
}

pub(super) fn to_properties(p: AuthProperties) -> rumqttc_v5::AuthProperties {
    rumqttc_v5::AuthProperties {
        method: p.method,
        data: p.data,
        reason: p.reason_string,
        user_properties: p.user_properties,
    }
}
pub(super) fn from_properties(p: rumqttc_v5::AuthProperties) -> AuthProperties {
    AuthProperties {
        method: p.method,
        data: p.data,
        reason_string: p.reason,
        user_properties: p.user_properties,
    }
}
