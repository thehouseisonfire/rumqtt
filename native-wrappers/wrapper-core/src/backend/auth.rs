use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::{
    AuthAction, AuthChallenge, AuthContext, AuthExchange, AuthFailure, AuthProperties,
    AuthenticatorConfig,
};

#[derive(Debug, Default)]
pub(super) struct Monitor {
    state: Mutex<(Option<AuthFailure>, Option<tokio::time::Instant>)>,
    pub changed: Notify,
}

impl Monitor {
    pub fn snapshot(&self) -> (Option<AuthFailure>, Option<tokio::time::Instant>) {
        *self.state.lock()
    }
    fn fail(&self, failure: AuthFailure) {
        self.state.lock().0.get_or_insert(failure);
        self.changed.notify_one();
    }
    fn deadline(&self, deadline: Option<tokio::time::Instant>) {
        self.state.lock().1 = deadline;
        self.changed.notify_one();
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
fn from_properties(p: rumqttc_v5::AuthProperties) -> AuthProperties {
    AuthProperties {
        method: p.method,
        data: p.data,
        reason_string: p.reason,
        user_properties: p.user_properties,
    }
}
