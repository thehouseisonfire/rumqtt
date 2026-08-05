use crate::mqttbytes::Error as MqttError;
use crate::mqttbytes::v5::{Auth, AuthProperties, AuthReasonCode, ConnAck};
use crate::notice::{AuthNoticeError, AuthNoticeTx};
use crate::{AuthAction, AuthError, AuthEvent, AuthExchangeKind, AuthFailureReason, AuthOutcome};
use crate::{Event, StateError};
use std::collections::VecDeque;

#[derive(Debug)]
pub enum AuthState {
    Idle,
    Initial {
        method: String,
    },
    Reauth {
        method: String,
        request_id: u64,
        notice: Option<AuthNoticeTx>,
    },
}

impl AuthState {
    const fn kind(&self) -> Option<AuthExchangeKind> {
        match self {
            Self::Idle => None,
            Self::Initial { .. } => Some(AuthExchangeKind::InitialConnect),
            Self::Reauth { .. } => Some(AuthExchangeKind::Reauthentication),
        }
    }

    fn method(&self) -> Option<&str> {
        match self {
            Self::Idle => None,
            Self::Initial { method } | Self::Reauth { method, .. } => Some(method),
        }
    }
}

impl Clone for AuthState {
    fn clone(&self) -> Self {
        match self {
            Self::Idle => Self::Idle,
            Self::Initial { method } => Self::Initial {
                method: method.clone(),
            },
            Self::Reauth {
                method, request_id, ..
            } => Self::Reauth {
                method: method.clone(),
                request_id: *request_id,
                notice: None,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuthLifecycle {
    method: Option<String>,
    state: AuthState,
    next_request_id: u64,
}

impl AuthLifecycle {
    pub(crate) const fn new(method: Option<String>) -> Self {
        Self {
            method,
            state: AuthState::Idle,
            next_request_id: 1,
        }
    }

    pub(crate) fn set_method(&mut self, method: Option<String>) {
        self.method = method;
    }

    pub(crate) fn method(&self) -> Option<&str> {
        self.method.as_deref()
    }

    pub(crate) fn active_exchange(&self) -> Option<(AuthExchangeKind, String)> {
        Some((self.state.kind()?, self.state.method()?.to_owned()))
    }

    pub(crate) fn begin_connect(&mut self, method: Option<String>, events: &mut VecDeque<Event>) {
        self.method.clone_from(&method);
        self.state = method.map_or_else(
            || AuthState::Idle,
            |method| {
                events.push_back(Event::Auth(AuthEvent::Started {
                    kind: AuthExchangeKind::InitialConnect,
                    method: method.clone(),
                }));
                AuthState::Initial { method }
            },
        );
    }

    pub(crate) fn reset(&mut self, reason: AuthNoticeError, events: &mut VecDeque<Event>) {
        let failed = self.fail_active(reason);
        events.extend(failed);
        self.state = AuthState::Idle;
    }

    pub(crate) fn reset_reauth(
        &mut self,
        reason: AuthNoticeError,
        events: &mut VecDeque<Event>,
    ) -> Option<(String, AuthNoticeError)> {
        let AuthState::Reauth { method, .. } = &self.state else {
            return None;
        };

        let method = method.clone();
        let failed = self.fail_active(reason.clone());
        events.extend(failed);
        self.state = AuthState::Idle;
        Some((method, reason))
    }

    pub(crate) fn fail_active(&mut self, reason: AuthNoticeError) -> Vec<Event> {
        let Some(kind) = self.state.kind() else {
            return Vec::new();
        };
        let method = self.state.method().unwrap_or_default().to_owned();
        if let AuthState::Reauth { notice, .. } = &mut self.state
            && let Some(notice) = notice.take()
        {
            notice.error(reason.clone());
        }
        vec![Event::Auth(AuthEvent::Failed {
            kind,
            method,
            reason: AuthFailureReason::from_notice_error(reason),
        })]
    }

    pub(crate) fn validate_successful_connack(&self, connack: &ConnAck) -> Result<(), StateError> {
        let actual = connack
            .properties
            .as_ref()
            .and_then(|properties| properties.authentication_method.as_deref());

        match (self.method(), actual) {
            (Some(expected), Some(actual)) if expected == actual => Ok(()),
            (None, None) => Ok(()),
            _ => Err(protocol_error()),
        }
    }

    pub(crate) fn complete_initial_connack(&mut self, events: &mut VecDeque<Event>) {
        match &self.state {
            AuthState::Initial { method } => {
                events.push_back(Event::Auth(AuthEvent::Succeeded {
                    kind: AuthExchangeKind::InitialConnect,
                    method: method.clone(),
                }));
                self.state = AuthState::Idle;
            }
            AuthState::Idle | AuthState::Reauth { .. } => {}
        }
    }

    pub(crate) fn begin_reauth(
        &mut self,
        properties: Option<AuthProperties>,
        mut notice: Option<AuthNoticeTx>,
        events: &mut VecDeque<Event>,
    ) -> Result<Auth, StateError> {
        if !matches!(self.state, AuthState::Idle) {
            if let Some(notice) = notice {
                notice.error(AuthNoticeError::OverlappingReauth);
            }
            return Err(StateError::AuthError(
                "cannot start re-authentication while an authentication exchange is active"
                    .to_owned(),
            ));
        }

        let Some(method) = self.method.clone() else {
            if let Some(notice) = notice {
                notice.error(AuthNoticeError::MissingAuthenticationMethod);
            }
            return Err(StateError::AuthError(
                "AUTH packet requires a CONNECT Authentication Method".to_owned(),
            ));
        };

        let properties = match normalize_auth_properties(&method, properties) {
            Ok(properties) => properties,
            Err(err) => {
                if let Some(notice) = notice.take() {
                    notice.error(AuthNoticeError::AuthenticationFailed(err.to_string()));
                }
                return Err(err);
            }
        };
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        self.state = AuthState::Reauth {
            method: method.clone(),
            request_id,
            notice,
        };
        events.push_back(Event::Auth(AuthEvent::Started {
            kind: AuthExchangeKind::Reauthentication,
            method,
        }));

        Ok(Auth::new(AuthReasonCode::ReAuthenticate, Some(properties)))
    }

    pub(crate) fn reauth_method(&self) -> Result<&str, AuthNoticeError> {
        if !matches!(self.state, AuthState::Idle) {
            return Err(AuthNoticeError::OverlappingReauth);
        }

        self.method
            .as_deref()
            .ok_or(AuthNoticeError::MissingAuthenticationMethod)
    }

    pub(crate) fn incoming_auth(
        &self,
        auth: &Auth,
        events: &mut VecDeque<Event>,
    ) -> Result<IncomingAuthEffect, StateError> {
        match auth.code {
            AuthReasonCode::Success => {
                let _ = events;
                self.incoming_success(auth)
            }
            AuthReasonCode::Continue => self.incoming_continue(auth, events),
            AuthReasonCode::ReAuthenticate => Err(protocol_error()),
        }
    }

    fn incoming_success(&self, auth: &Auth) -> Result<IncomingAuthEffect, StateError> {
        let Some(kind) = self.state.kind() else {
            return Err(protocol_error());
        };
        let method = self.active_method()?.to_owned();
        validate_incoming_auth_method(&method, auth.properties.as_ref())?;
        Ok(IncomingAuthEffect::Success { kind, method })
    }

    pub(crate) fn complete_success(
        &mut self,
        kind: AuthExchangeKind,
        method: String,
        events: &mut VecDeque<Event>,
    ) {
        if let AuthState::Reauth { notice, .. } = &mut self.state
            && let Some(notice) = notice.take()
        {
            notice.success(AuthOutcome::Success);
        }
        events.push_back(Event::Auth(AuthEvent::Succeeded { kind, method }));
        self.state = AuthState::Idle;
    }

    fn incoming_continue(
        &self,
        auth: &Auth,
        events: &mut VecDeque<Event>,
    ) -> Result<IncomingAuthEffect, StateError> {
        let Some(kind) = self.state.kind() else {
            return Err(protocol_error());
        };
        let method = self.active_method()?.to_owned();
        validate_incoming_auth_method(&method, auth.properties.as_ref())?;
        events.push_back(Event::Auth(AuthEvent::Continue { kind, method }));
        Ok(IncomingAuthEffect::Continue { kind })
    }

    pub(crate) fn outgoing_continue(
        &self,
        properties: Option<AuthProperties>,
    ) -> Result<Auth, StateError> {
        let method = self.active_method()?.to_owned();
        let properties = normalize_auth_properties(&method, properties)?;
        Ok(Auth::new(AuthReasonCode::Continue, Some(properties)))
    }

    fn active_method(&self) -> Result<&str, StateError> {
        self.state.method().ok_or_else(protocol_error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncomingAuthEffect {
    Continue {
        kind: AuthExchangeKind,
    },
    Success {
        kind: AuthExchangeKind,
        method: String,
    },
}

fn validate_incoming_auth_method(
    expected: &str,
    properties: Option<&AuthProperties>,
) -> Result<(), StateError> {
    let Some(actual) = properties.and_then(|properties| properties.method.as_deref()) else {
        return Err(protocol_error());
    };

    if actual != expected {
        return Err(protocol_error());
    }

    Ok(())
}

pub fn normalize_auth_properties(
    expected: &str,
    properties: Option<AuthProperties>,
) -> Result<AuthProperties, StateError> {
    let mut properties = properties.unwrap_or_default();

    match &properties.method {
        Some(actual) if actual != expected => Err(StateError::AuthError(format!(
            "AUTH packet Authentication Method '{actual}' does not match CONNECT Authentication Method '{expected}'"
        ))),
        Some(_) => Ok(properties),
        None => {
            properties.method = Some(expected.to_owned());
            Ok(properties)
        }
    }
}

pub const fn protocol_error() -> StateError {
    StateError::Deserialization(MqttError::ProtocolError)
}

impl AuthAction {
    pub(crate) fn into_continue_properties(self) -> Result<Option<AuthProperties>, AuthError> {
        match self {
            Self::Send(properties) => Ok(Some(properties)),
            Self::Complete => Ok(None),
            Self::Fail(message) => Err(AuthError::Failed(message)),
        }
    }
}
